//! Content requests: the envelope, the API allowlist table, the consent
//! hold whatever the policy says, the cache that is never touched, `owl draft`
//! on a content record, the two-daemon exchange and the thread
//! timeline.
//!
//! A (seed 1) asks, B (seed 2) owns the checkout. Every test has its own temp homes, every
//! daemon binds 127.0.0.1:0 and every fixture repo lives in the test's own tempdir with
//! `GIT_CONFIG_NOSYSTEM` and a private `HOME`, so nothing depends on the machine's git.

mod common;

use std::path::Path;
use std::process::Command;

use common::{
    Peer, TestDaemon, assert_error, claude_home, client, fp, id, policy, post_envelope,
    prepare_home_with, signed, spawn_daemon_with, write_contact_full,
};
use owlpost::config::Config;
use owlpost::contacts::Mode;
use owlpost::content;
use owlpost::envelope::{Body, Envelope, Kind, MAX_CONTENT_BYTES, Payload};
use owlpost::identity::Identity;
use owlpost::route::{self, Marker};
use owlpost::server::{record_state, record_state_for};
use owlpost::spool::{Dir, Record, Spool};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");
const PROJECT: &str = "github.com/company/monorepo";
const FILE: &str = "src/auth/session.rs";
/// The tip's content of `FILE` in [`fixture_repo`]; the first commit's differs.
const TIP: &str = "fn rotate() {}\napi_key: hunter2\n";
const FIRST: &str = "fn old() {}\n";

// ---------------------------------------------------------------- fixtures

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.org")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.org")
        // Fixed dates: every fixture repo gets the same shas, so a sha taken from one
        // resolves in another (`draft_at_an_older_ref_serves_that_commits_content`).
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A checkout with two commits of `FILE`: the first holds [`FIRST`], the tip [`TIP`] (which
/// carries a line the default `responder.redact` matches). Returns the dir and the first
/// commit's sha, so a test can ask for a ref whose content differs from the branch tip.
fn fixture_repo() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FILE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.name", "T"]);
    git(dir.path(), &["config", "user.email", "t@example.org"]);
    std::fs::write(&path, FIRST).unwrap();
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-qm", "first"]);
    let out = Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["rev-parse", "HEAD"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    let first = String::from_utf8_lossy(&out.stdout).trim().to_string();
    std::fs::write(&path, TIP).unwrap();
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-qm", "tip"]);
    (dir, first)
}

/// `git rev-parse <rev>` in `dir`, trimmed.
fn rev_parse(dir: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", rev])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git rev-parse {rev}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Writes one extra file into the checkout's tip commit and returns nothing.
fn commit_file(repo: &Path, rel: &str, bytes: &[u8]) {
    let p = repo.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, bytes).unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "add"]);
}

/// A content request payload from `from` to `to` for `FILE` of `PROJECT`.
fn request(from: &Identity, to: &Identity, git_ref: Option<&str>) -> Payload {
    Payload::content(&fp(from), &fp(to), Some(PROJECT), git_ref, Some(FILE), None)
}

fn signed_content(from: &Identity, to: &Identity, git_ref: Option<&str>) -> Envelope {
    Envelope::sign(&request(from, to, git_ref), from)
}

/// Daemon B with `PROJECT` pointing at `repo` and `peers` in its contact book.
async fn responder(repo: &Path, peers: &[Peer<'_>]) -> TestDaemon {
    let repo = repo.to_path_buf();
    spawn_daemon_with(2, peers, move |cfg| {
        cfg.projects
            .insert(PROJECT.into(), repo.display().to_string());
    })
    .await
}

/// Registers a live Claude session under `home`: a fresh marker (heartbeat now) with its
/// `wake/` and `tmp/` dirs, exactly as the `SessionStart` hook writes one.
fn register_session(home: &Path, sid: &str, cwd: &Path) {
    route::write_marker(home, &Marker::new(sid, &cwd.to_string_lossy(), "startup")).unwrap();
}

/// The invariant: session `sid` was not woken and nothing was routed. `wake/` must exist
/// (a typo'd session id would otherwise make this vacuous) and hold no entry, and
/// `spool/routing/` must be absent or empty.
fn assert_quiet(home: &Path, sid: &str, want: &str) {
    let wake = route::wake_dir(home, sid);
    assert!(wake.is_dir(), "{want}: the session's wake dir must exist");
    let woken: Vec<String> = std::fs::read_dir(&wake)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        woken.is_empty(),
        "{want}: no session may be woken: {woken:?}"
    );
    let routing = route::routing_dir(home);
    let routed: Vec<String> = std::fs::read_dir(&routing)
        .map(|it| {
            it.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        routed.is_empty(),
        "{want}: nothing may be routed: {routed:?}"
    );
}

fn owl(home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(OWL)
        .env_remove("OWLPOST_HOME")
        .env_remove("EDITOR")
        .env("OWLPOST_CLAUDE_HOME", claude_home())
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn owl_ok(home: &Path, args: &[&str]) -> String {
    let (code, out, err) = owl(home, args);
    assert_eq!(code, 0, "owl {args:?} failed: {err}");
    out
}

fn only_id(spool: &Spool, dir: Dir) -> String {
    let all = spool.list(dir, |_| true).unwrap();
    assert_eq!(
        all.len(),
        1,
        "expected one record in {:?}, got {}",
        dir.name(),
        all.len()
    );
    all[0].0.clone()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---------------------------------------------------------------- the envelope

/// Both new payloads round-trip through their documented wire keys, and the untagged
/// `Body` order still sends a question to `Body::Question` and an answer to `Body::Answer`.
#[test]
fn content_and_reply_round_trip_with_their_wire_keys() {
    let (a, b) = (id(1), id(2));
    let req = request(&a, &b, Some("main"));
    let v: Value = serde_json::from_slice(&req.to_signed_bytes()).unwrap();
    assert_eq!(v["type"], "content");
    assert_eq!(v["body"]["project"], PROJECT);
    assert_eq!(
        v["body"]["ref"], "main",
        "the wire key is `ref`, not `git_ref`"
    );
    assert_eq!(v["body"]["path"], FILE);
    assert!(
        v["body"].get("memory").is_none(),
        "absent memory is omitted"
    );
    let back: Payload = serde_json::from_value(v).unwrap();
    assert_eq!(back, req);
    assert!(matches!(back.body, Body::Content { .. }), "body variant");

    let reply = Payload::content_reply(&req, "hello", "abc", Some("deadbeef"), true, 2);
    let v: Value = serde_json::from_slice(&reply.to_signed_bytes()).unwrap();
    assert_eq!(
        v["type"], "content-reply",
        "lowercase alone would say contentreply"
    );
    for (k, want) in [
        ("content", json!("hello")),
        ("sha256", json!("abc")),
        ("ref_resolved", json!("deadbeef")),
        ("truncated", json!(true)),
        ("redactions", json!(2)),
        ("harness", json!("human")),
    ] {
        assert_eq!(v["body"][k], want, "body.{k}");
    }
    let back: Payload = serde_json::from_value(v).unwrap();
    assert_eq!(back, reply);
    assert!(
        matches!(back.body, Body::ContentReply { .. }),
        "a reply must not be swallowed by the all-optional Content variant"
    );
}

/// The untagged order did not shift — bytes byte-identical to today's still parse into
/// the old two variants, and a reply body is never read as a request.
#[test]
fn question_and_answer_bytes_still_parse_the_same() {
    let question = json!({
        "v": 1, "id": "q1", "type": "question", "from": "owl:a", "to": "owl:b",
        "ts": "2026-09-01T10:00:00Z", "in_reply_to": null,
        "body": { "project": "p", "path": "f", "question": "why?" }
    });
    let p: Payload = serde_json::from_value(question).unwrap();
    assert_eq!(p.kind, Kind::Question);
    assert!(matches!(p.body, Body::Question { .. }));

    let answer = json!({
        "v": 1, "id": "a1", "type": "answer", "from": "owl:b", "to": "owl:a",
        "ts": "2026-09-01T10:00:00Z", "in_reply_to": "q1",
        "body": { "answer": "because", "harness": "claude", "redactions": 0, "cached": false }
    });
    let p: Payload = serde_json::from_value(answer).unwrap();
    assert_eq!(p.kind, Kind::Answer);
    assert!(matches!(p.body, Body::Answer { .. }));
}

/// The reply copies the request's thread id, exactly as `Payload::answer` does.
#[test]
fn content_reply_copies_the_context_id() {
    let (a, b) = (id(1), id(2));
    let mut req = request(&a, &b, None);
    req.context_id = Some("thread-7".into());
    let reply = Payload::content_reply(&req, "x", "y", None, false, 0);
    assert_eq!(reply.context_id.as_deref(), Some("thread-7"));
    assert_eq!(reply.in_reply_to.as_deref(), Some(req.id.as_str()));
    assert_eq!(reply.from, req.to);
    assert_eq!(reply.to, req.from);
    // The twin: a request with no thread id yields a reply with none.
    req.context_id = None;
    assert_eq!(
        Payload::content_reply(&req, "x", "y", None, false, 0).context_id,
        None
    );
}

// ---------------------------------------------------------------- the 400 table

/// Every row of the §7 validation table, each with the exact message, and each leaving the
/// spool untouched. The last row is the positive twin: a well-formed request is accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn validation_table_answers_400_and_spools_nothing() {
    let (repo, _) = fixture_repo();
    let a = id(1);
    let memories = tempfile::tempdir().unwrap();
    std::fs::write(memories.path().join("note.md"), "hi\n").unwrap();
    let d = responder(
        repo.path(),
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
    )
    .await;
    let c = client(Some(&a), &d.id);
    // A live session on the responder, so "no session is woken" is a real assertion and not
    // a vacuously empty `sessions/`: a request that got past validation would be routed to
    // this one synchronously, inside the handler (`server::route_new_record`).
    let sid = "content-400";
    register_session(d.home(), sid, repo.path());

    // `body` overrides applied to a valid content request.
    let rows: Vec<(Value, &str)> = vec![
        (
            json!({ "project": PROJECT, "path": FILE, "memory": "note.md" }),
            "body must carry exactly one of path and memory",
        ),
        (
            json!({ "project": PROJECT }),
            "body must carry exactly one of path and memory",
        ),
        (json!({ "path": FILE }), "missing body.project"),
        (
            json!({ "project": "github.com/other/repo", "path": FILE }),
            "unknown project",
        ),
        (
            json!({ "project": PROJECT, "path": "../../etc/passwd" }),
            "path escapes the project",
        ),
        (
            json!({ "project": PROJECT, "path": "/etc/passwd" }),
            "path escapes the project",
        ),
        (
            json!({ "project": PROJECT, "path": "" }),
            "path escapes the project",
        ),
        (
            json!({ "memory": "../../etc/passwd" }),
            "memory key escapes the memory store",
        ),
        // No `memory_root` on this daemon at all.
        (
            json!({ "memory": "note.md" }),
            "memory store not configured",
        ),
        (
            json!({ "project": PROJECT, "path": FILE, "ref": "main;rm -rf /" }),
            "malformed ref",
        ),
        (
            json!({ "project": PROJECT, "path": FILE, "ref": "x".repeat(201) }),
            "malformed ref",
        ),
        // A ref that would reach `git` as a flag rather than as a revision.
        (
            json!({ "project": PROJECT, "path": FILE, "ref": "-h" }),
            "malformed ref",
        ),
        (
            json!({ "project": PROJECT, "path": FILE, "ref": "--help" }),
            "malformed ref",
        ),
        (
            json!({ "project": PROJECT, "path": FILE, "ref": "--" }),
            "malformed ref",
        ),
        (
            json!({ "project": PROJECT, "path": FILE, "ref": "" }),
            "malformed ref",
        ),
    ];
    for (body, want) in rows {
        let mut p = request(&a, &d.id, None);
        let mut v = serde_json::to_value(&p).unwrap();
        v["body"] = body.clone();
        // Re-sign the edited bytes so the failure is the validation, never the signature.
        p = serde_json::from_value(v.clone()).unwrap_or(p);
        let raw = serde_json::to_string(&v).unwrap();
        let sig = owlpost::identity::sig_string(&a.sign(raw.as_bytes()));
        let env = Envelope { raw, sig };
        let _ = p;
        let resp = post_envelope(&c, &d, &env).await;
        // Asserted before the status, so a validation row that fell through to routing fails
        // here — on the invariant — rather than on the 400 the fall-through also loses.
        assert_quiet(d.home(), sid, want);
        assert_error(resp, 400, want).await;
        assert!(
            d.spool().list(Dir::Inbox, |_| true).unwrap().is_empty(),
            "{want}: nothing may be spooled"
        );
    }
    // `private_memory` is false by default even when a root is configured.
    let root = memories.path().display().to_string();
    let d2 = spawn_daemon_with(
        3,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
        {
            let root = root.clone();
            move |cfg| cfg.responder.memory_root = Some(root)
        },
    )
    .await;
    let c2 = client(Some(&a), &d2.id);
    let sid2 = "content-400-mem";
    register_session(d2.home(), sid2, repo.path());
    let mem = Payload::content(&fp(&a), &d2.fp(), None, None, None, Some("note.md"));
    let resp = post_envelope(&c2, &d2, &Envelope::sign(&mem, &a)).await;
    assert_quiet(d2.home(), sid2, "memory store not configured");
    assert_error(resp, 400, "memory store not configured").await;
    assert!(d2.spool().list(Dir::Inbox, |_| true).unwrap().is_empty());

    // The positive twin: a well-formed request is accepted and always SUBMITTED — and it
    // *does* wake the session the table rows left alone.
    let env = signed_content(&a, &d.id, Some("main"));
    let resp = post_envelope(&c, &d, &env).await;
    assert_eq!(resp.status().as_u16(), 202);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "accepted");
    assert_eq!(body["state"], "TASK_STATE_SUBMITTED");
    let spool = d.spool();
    let rid = only_id(&spool, Dir::Inbox);
    assert_eq!(
        spool.get(Dir::Inbox, &rid).unwrap().unwrap().state,
        "consent"
    );
    assert!(
        route::wake_file(d.home(), sid, &rid).is_file(),
        "a valid request does wake the session"
    );
    assert_eq!(
        route::load_routing(d.home(), &rid).current.as_deref(),
        Some(sid),
        "and the routing names it"
    );

    // The positive twins of the four flag-shaped rows: an ordinary ref is still accepted.
    // (The daemon never resolves it here, so these need not exist in the fixture repo.)
    for good in [
        "main",
        "v1.0",
        "feature/x-1",
        "0123456789abcdef0123456789abcdef01234567",
    ] {
        let resp = post_envelope(&c, &d, &signed_content(&a, &d.id, Some(good))).await;
        assert_eq!(resp.status().as_u16(), 202, "ref {good} must be accepted");
    }
}

/// A path inside the allowlist that does not exist still reaches consent: the daemon never
/// stats the file, so a peer cannot probe for it by timing. It fails at `owl draft`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_path_inside_the_allowlist_still_reaches_consent() {
    let (repo, _) = fixture_repo();
    let a = id(1);
    let d = responder(
        repo.path(),
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
    )
    .await;
    let c = client(Some(&a), &d.id);
    let p = Payload::content(
        &fp(&a),
        &d.fp(),
        Some(PROJECT),
        Some("main"),
        Some("src/nope.rs"),
        None,
    );
    let resp = post_envelope(&c, &d, &Envelope::sign(&p, &a)).await;
    assert_eq!(resp.status().as_u16(), 202);
    let spool = d.spool();
    let rid = only_id(&spool, Dir::Inbox);
    owl_ok(d.home(), &["allow", &fp(&a), "--once"]);
    let (code, _, err) = owl(d.home(), &["draft", &rid]);
    assert_eq!(code, 1, "draft must refuse: {err}");
    assert!(
        err.contains("unknown path src/nope.rs at main"),
        "stderr: {err}"
    );
}

// ---------------------------------------------------------------- consent only

/// The state table is explicit about the kind, and only about the kind.
#[test]
fn record_state_for_holds_content_in_every_mode() {
    for mode in [
        None,
        Some(Mode::Manual),
        Some(Mode::Auto),
        Some(Mode::Never),
    ] {
        assert_eq!(
            record_state_for(Kind::Content, mode),
            ("consent", false),
            "content with policy {mode:?}"
        );
        // The twin: a question keeps today's table byte for byte.
        assert_eq!(record_state_for(Kind::Question, mode), record_state(mode));
    }
}

/// An `auto` peer's content request is held for consent while that same peer's question
/// is still `pending` and still auto-answered; a `never` peer gets the unchanged 403.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auto_peer_is_held_for_content_and_not_for_a_question() {
    let (repo, _) = fixture_repo();
    let a = id(1);
    let d = responder(
        repo.path(),
        &[Peer::new(&a, "Ana", Some(policy(Mode::Auto, None)))],
    )
    .await;
    let c = client(Some(&a), &d.id);

    let env = signed_content(&a, &d.id, Some("main"));
    assert_eq!(post_envelope(&c, &d, &env).await.status().as_u16(), 202);
    let spool = d.spool();
    let cid = only_id(&spool, Dir::Inbox);
    let crec = spool.get(Dir::Inbox, &cid).unwrap().unwrap();
    assert_eq!(
        crec.state, "consent",
        "auto policy does not apply to content"
    );

    // The positive twin: a question from the very same peer is `pending` (auto candidate).
    let q = signed(&a, &d.id, "why?");
    assert_eq!(post_envelope(&c, &d, &q).await.status().as_u16(), 202);
    let qid = spool
        .list(Dir::Inbox, |_| true)
        .unwrap()
        .into_iter()
        .map(|(i, _)| i)
        .find(|i| *i != cid)
        .expect("the question record");
    assert_eq!(
        spool.get(Dir::Inbox, &qid).unwrap().unwrap().state,
        "pending"
    );

    // The scheduler refuses the content record by name, even once it is `pending`.
    let cfg = Config::load(d.home()).unwrap();
    spool.set_state(Dir::Inbox, &cid, "pending").unwrap();
    let out = owlpost::auto::attempt(d.home(), d.home(), &cfg, &spool, &cid).unwrap();
    assert_eq!(
        out,
        owlpost::auto::Outcome::Skipped(format!(
            "record {cid} is a content request — consent only"
        ))
    );

    // A `never` peer is still refused at the API, unchanged.
    let e = id(9);
    let d2 = responder(
        repo.path(),
        &[Peer::new(&e, "Eve", Some(policy(Mode::Never, None)))],
    )
    .await;
    let c2 = client(Some(&e), &d2.id);
    let resp = post_envelope(&c2, &d2, &signed_content(&e, &d2.id, None)).await;
    assert_eq!(resp.status().as_u16(), 403);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "unavailable");
    assert_eq!(body["state"], "TASK_STATE_REJECTED");
    assert!(d2.spool().list(Dir::Inbox, |_| true).unwrap().is_empty());
}

// ---------------------------------------------------------------- no cache

/// No cache entry is read or written for a content request — two identical requests both
/// spool, `cache/` stays empty across draft and send, and a planted entry is never served.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_cache_entry_is_read_or_written_for_content() {
    let (repo, _) = fixture_repo();
    let a = id(1);
    let d = responder(
        repo.path(),
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
    )
    .await;
    let c = client(Some(&a), &d.id);
    let spool = d.spool();

    // A planted entry under the hash this request would have as a *question*.
    let hash = owlpost::envelope::question_hash(PROJECT, Some(FILE), "");
    let planted = signed(&a, &d.id, "planted");
    spool
        .cache_put(&hash, &common::record(&planted, "unacked"))
        .unwrap();

    for _ in 0..2 {
        let env = signed_content(&a, &d.id, Some("main"));
        let resp = post_envelope(&c, &d, &env).await;
        assert_eq!(
            resp.status().as_u16(),
            202,
            "a content request is never served from the cache"
        );
    }
    assert_eq!(
        spool.list(Dir::Inbox, |_| true).unwrap().len(),
        2,
        "both identical requests spool their own record"
    );
    // Neither record carries a question hash.
    for (_, rec) in spool.list(Dir::Inbox, |_| true).unwrap() {
        assert!(rec.meta.get("hash").is_none(), "meta: {}", rec.meta);
    }

    let rid = spool.list(Dir::Inbox, |_| true).unwrap()[0].0.clone();
    owl_ok(d.home(), &["allow", &fp(&a), "--once"]);
    owl_ok(d.home(), &["draft", &rid]);
    owl_ok(d.home(), &["send", &rid]);
    let cached = spool.list(Dir::Cache, |_| true).unwrap();
    assert_eq!(
        cached.len(),
        1,
        "only the planted entry, never a content one"
    );
    assert_eq!(cached[0].0, hash);
    // The outbox reply carries no hash either.
    let oid = only_id(&spool, Dir::Outbox);
    let orec = spool.get(Dir::Outbox, &oid).unwrap().unwrap();
    assert!(orec.meta.get("hash").is_none(), "meta: {}", orec.meta);
}

// ---------------------------------------------------------------- owl draft

/// One prepared responder home with a consent-released content record, for the draft tests.
struct Drafting {
    home: TempDir,
    _repo: TempDir,
    id: String,
    first: String,
}

/// Spools `payload` straight into B's inbox (state `pending`) without a daemon: the draft
/// path is what is under test, not the transport.
fn drafting(tweak: impl FnOnce(&mut Config), payload: Payload) -> Drafting {
    let (repo, first) = fixture_repo();
    let home = tempfile::tempdir().unwrap();
    let a = id(1);
    let b = id(2);
    let repo_path = repo.path().display().to_string();
    prepare_home_with(
        home.path(),
        &b,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
        |cfg| {
            cfg.projects.insert(PROJECT.into(), repo_path);
            tweak(cfg);
        },
    );
    let env = Envelope::sign(&payload, &a);
    let spool = Spool::new(home.path()).unwrap();
    spool
        .put(Dir::Inbox, &payload.id, &common::record(&env, "pending"))
        .unwrap();
    Drafting {
        home,
        _repo: repo,
        id: payload.id.clone(),
        first,
    }
}

/// [`content_at`] for a path other than [`FILE`].
fn content_at_path(path: &str, git_ref: Option<&str>) -> Payload {
    Payload::content(
        &fp(&id(1)),
        &fp(&id(2)),
        Some(PROJECT),
        git_ref,
        Some(path),
        None,
    )
}

fn content_at(git_ref: Option<&str>) -> Payload {
    Payload::content(
        &fp(&id(1)),
        &fp(&id(2)),
        Some(PROJECT),
        git_ref,
        Some(FILE),
        None,
    )
}

/// The draft reads the file at the resolved ref, redacts it, prints the documented line
/// and stores the digest, the commit and `harness: "human"`.
#[test]
fn draft_reads_the_file_at_the_ref_and_redacts_it() {
    let d = drafting(|_| {}, content_at(Some("main")));
    let out = owl_ok(d.home.path(), &["draft", &d.id]);
    let redacted = TIP.replace("api_key: hunter2", "[redacted]");
    assert!(out.contains(&format!("content: {FILE}@",)), "stdout: {out}");
    assert!(
        out.contains(&format!("({} bytes, 1 redactions)", redacted.len())),
        "stdout: {out}"
    );
    let spool = Spool::new(d.home.path()).unwrap();
    let rec = spool.get(Dir::Inbox, &d.id).unwrap().unwrap();
    assert_eq!(rec.state, "drafted");
    let c = content::stored(&rec).expect("a content draft");
    assert_eq!(c.text, redacted, "the secret line is gone");
    assert_eq!(c.redactions, 1);
    assert!(!c.truncated);
    assert_eq!(c.sha256, hex(&Sha256::digest(redacted.as_bytes())));
    let resolved = c.ref_resolved.expect("ref_resolved");
    assert_eq!(resolved.len(), 40, "a full commit sha: {resolved}");
    assert_eq!(rec.draft.as_ref().unwrap()["harness"], "human");
    // The printed line names the resolved commit, not the ref the peer typed.
    assert!(out.contains(&resolved[..7]), "stdout: {out}");
}

/// A ref that is not the branch tip serves that commit's content — the resolution is
/// real, not a read of the working tree.
#[test]
fn draft_at_an_older_ref_serves_that_commits_content() {
    let d = drafting(|_| {}, content_at(None));
    let first = d.first.clone();
    let d2 = drafting(|_| {}, content_at(Some(&first)));
    owl_ok(d.home.path(), &["draft", &d.id]);
    owl_ok(d2.home.path(), &["draft", &d2.id]);
    let at = |x: &Drafting| {
        content::stored(
            &Spool::new(x.home.path())
                .unwrap()
                .get(Dir::Inbox, &x.id)
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    };
    // The two fixtures are separate repos (same shas: `git` pins the dates), so compare
    // the content, not the sha.
    assert_eq!(at(&d).text, TIP.replace("api_key: hunter2", "[redacted]"));
    assert_eq!(
        at(&d2).text,
        FIRST,
        "the first commit's content, not the tip's"
    );
    assert_eq!(at(&d2).redactions, 0);
}

/// Every flag that picks or replaces a model is a usage error on a content record, and
/// the record is left untouched. The twin: the same flags still work on a question.
#[test]
fn draft_flags_are_refused_on_a_content_record() {
    for flags in [
        vec!["--harness", "fake"],
        vec!["--text", "mine"],
        vec!["--prompt"],
        vec!["--text", "mine", "--agent"],
    ] {
        let d = drafting(|_| {}, content_at(Some("main")));
        let mut args = vec!["draft", d.id.as_str()];
        args.extend(flags.iter().copied());
        let (code, out, err) = owl(d.home.path(), &args);
        assert_eq!(code, 1, "{flags:?} must be a usage error");
        assert!(
            err.contains(&format!(
                "record {} is a content request — run owl draft {} with no flags",
                d.id, d.id
            )),
            "{flags:?} stderr: {err}"
        );
        assert!(out.is_empty(), "{flags:?} printed {out}");
        let rec = Spool::new(d.home.path())
            .unwrap()
            .get(Dir::Inbox, &d.id)
            .unwrap()
            .unwrap();
        assert_eq!(rec.state, "pending", "{flags:?} changed the record");
        assert!(rec.draft.is_none(), "{flags:?} stored a draft");
    }
}

/// A file over the cap is cut on a character boundary while `sha256` stays the digest of
/// the whole content. The fixture starts with one ASCII byte so the cap lands *inside* a
/// two-byte character: a byte slice would panic or split it.
#[test]
fn oversize_content_is_cut_on_a_character_boundary() {
    let big = format!("a{}", "ł".repeat(MAX_CONTENT_BYTES));
    let d = drafting(|_| {}, content_at(Some("main")));
    commit_file(d._repo.path(), "big.txt", big.as_bytes());
    let p = Payload::content(
        &fp(&id(1)),
        &fp(&id(2)),
        Some(PROJECT),
        Some("main"),
        Some("big.txt"),
        None,
    );
    let spool = Spool::new(d.home.path()).unwrap();
    spool
        .put(
            Dir::Inbox,
            &p.id,
            &common::record(&Envelope::sign(&p, &id(1)), "pending"),
        )
        .unwrap();
    let out = owl_ok(d.home.path(), &["draft", &p.id]);
    let c = content::stored(&spool.get(Dir::Inbox, &p.id).unwrap().unwrap()).unwrap();
    assert!(c.truncated, "over the cap");
    assert!(c.text.len() <= MAX_CONTENT_BYTES, "cut to {}", c.text.len());
    assert_eq!(
        c.text.len(),
        MAX_CONTENT_BYTES - 1,
        "the last character does not fit, so the cut is one byte short"
    );
    assert!(big.starts_with(&c.text), "the cut is a prefix");
    assert_eq!(
        c.sha256,
        hex(&Sha256::digest(big.as_bytes())),
        "the digest is of the WHOLE content, not the prefix"
    );
    assert_eq!(c.full_bytes, big.len());
    assert!(
        out.contains(&format!(
            "truncated, {MAX_CONTENT_BYTES} of {} bytes",
            big.len()
        )),
        "stdout: {out}"
    );
}

/// Neither a non-UTF-8 file nor one carrying a `NUL` byte is served, and nothing is
/// stored for either.
#[test]
fn binary_and_nul_carrying_files_are_refused() {
    for (name, bytes) in [
        ("bin.dat", vec![0xff, 0xfe, 0x00, 0x41]),
        ("nul.txt", b"ok\0still".to_vec()),
    ] {
        let d = drafting(|_| {}, content_at(Some("main")));
        commit_file(d._repo.path(), name, &bytes);
        let p = Payload::content(
            &fp(&id(1)),
            &fp(&id(2)),
            Some(PROJECT),
            Some("main"),
            Some(name),
            None,
        );
        let spool = Spool::new(d.home.path()).unwrap();
        spool
            .put(
                Dir::Inbox,
                &p.id,
                &common::record(&Envelope::sign(&p, &id(1)), "pending"),
            )
            .unwrap();
        let (code, out, err) = owl(d.home.path(), &["draft", &p.id]);
        assert_eq!(code, 1, "{name} must be refused");
        assert!(
            err.contains(&format!("{name} is not text")),
            "stderr: {err}"
        );
        assert!(out.is_empty(), "{name} printed {out}");
        let rec = spool.get(Dir::Inbox, &p.id).unwrap().unwrap();
        assert!(rec.draft.is_none(), "{name} stored a draft");
        assert_eq!(rec.state, "pending");
    }
}

/// A directory is not a blob: `git show <commit>:<dir>` would print a listing, which would
/// turn the allowlist into a file browser.
#[test]
fn a_directory_is_not_served_as_content() {
    let d = drafting(|_| {}, content_at(Some("main")));
    let p = Payload::content(
        &fp(&id(1)),
        &fp(&id(2)),
        Some(PROJECT),
        Some("main"),
        Some("src/auth"),
        None,
    );
    let spool = Spool::new(d.home.path()).unwrap();
    spool
        .put(
            Dir::Inbox,
            &p.id,
            &common::record(&Envelope::sign(&p, &id(1)), "pending"),
        )
        .unwrap();
    let (code, _, err) = owl(d.home.path(), &["draft", &p.id]);
    assert_eq!(code, 1, "a tree must be refused");
    assert!(
        err.contains("unknown path src/auth at main"),
        "stderr: {err}"
    );
}

/// A memory request serves `<memory_root>/<key>`, carries no `ref_resolved`, and a key
/// that resolves through a symlink out of the store is refused.
#[test]
fn memory_requests_serve_the_store_and_refuse_an_escape() {
    let store = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(store.path().join("decisions")).unwrap();
    std::fs::write(store.path().join("decisions/a.md"), "a decision\n").unwrap();
    std::fs::write(outside.path().join("secret.md"), "not yours\n").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        store.path().join("out.md"),
    )
    .unwrap();
    let root = store.path().display().to_string();
    let memory =
        |key: &str| Payload::content(&fp(&id(1)), &fp(&id(2)), None, None, None, Some(key));

    let d = drafting(
        {
            let root = root.clone();
            move |cfg| {
                cfg.responder.memory_root = Some(root);
                cfg.responder.scope.private_memory = true;
            }
        },
        memory("decisions/a.md"),
    );
    let out = owl_ok(d.home.path(), &["draft", &d.id]);
    assert!(
        out.contains("content: memory:decisions/a.md ("),
        "stdout: {out}"
    );
    let spool = Spool::new(d.home.path()).unwrap();
    let c = content::stored(&spool.get(Dir::Inbox, &d.id).unwrap().unwrap()).unwrap();
    assert_eq!(c.text, "a decision\n");
    assert_eq!(c.ref_resolved, None, "a memory entry has no commit");
    // The envelope's negative twin for the outgoing reply: what the asker receives names no commit
    // either, not just what we stored.
    owl_ok(d.home.path(), &["send", &d.id]);
    let oid = only_id(&spool, Dir::Outbox);
    let sent: Value =
        serde_json::from_str(&spool.get(Dir::Outbox, &oid).unwrap().unwrap().raw).unwrap();
    assert_eq!(
        sent["body"]["ref_resolved"],
        Value::Null,
        "a memory reply carries no commit: {sent}"
    );

    // The escape twin: the key is shape-clean but resolves outside the store.
    let d2 = drafting(
        {
            let root = root.clone();
            move |cfg| {
                cfg.responder.memory_root = Some(root);
                cfg.responder.scope.private_memory = true;
            }
        },
        memory("out.md"),
    );
    let (code, _, err) = owl(d2.home.path(), &["draft", &d2.id]);
    assert_eq!(code, 1, "a symlink out of the store must be refused");
    assert!(
        err.contains("out.md is outside the memory store"),
        "stderr: {err}"
    );
    let rec = Spool::new(d2.home.path())
        .unwrap()
        .get(Dir::Inbox, &d2.id)
        .unwrap()
        .unwrap();
    assert!(rec.draft.is_none(), "nothing is stored");
}

/// `owl inbox --json` names the kind and summarises the request the way the mod reads it.
#[test]
fn inbox_summarises_a_content_request() {
    let d = drafting(|_| {}, content_at(Some("main")));
    let out = owl_ok(d.home.path(), &["--json", "inbox", "--all"]);
    let v: Value = serde_json::from_str(&out).unwrap();
    let row = &v["records"].as_array().map_or(&v[0], |r| &r[0]).clone();
    assert_eq!(row["type"], "content");
    assert_eq!(row["summary"], format!("{FILE}@main"));
    assert_eq!(row["path"], FILE);
    // `drafting` spools the record directly, so the state here is the fixture's, not the
    // daemon's; the consent hold has its own test above.
    assert_eq!(row["state"], "pending");
}

// ---------------------------------------------------------------- end to end

/// A asks B for a file, B holds it for consent, allows, drafts and sends; A's pull
/// ingests the reply and `owl show` prints the content and the verified digest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_to_end_request_consent_draft_send_and_verify() {
    let (repo, _) = fixture_repo();
    let a = id(1);
    let b = responder(
        repo.path(),
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
    )
    .await;
    let a_home = tempfile::tempdir().unwrap();
    prepare_home_with(a_home.path(), &a, &[], |_| {});
    write_contact_full(
        a_home.path(),
        &Peer::new(&b.id, "Bea", None),
        &[&b.addr.to_string()],
        &["bea@example.org"],
    );

    let out = owl_ok(
        a_home.path(),
        &["request", "Bea", PROJECT, FILE, "--ref", "main"],
    );
    assert!(
        out.contains("— waiting for the owner's consent"),
        "stdout: {out}"
    );
    let a_spool = Spool::new(a_home.path()).unwrap();
    let ask_id = only_id(&a_spool, Dir::Asks);

    let b_spool = b.spool();
    let rid = only_id(&b_spool, Dir::Inbox);
    assert_eq!(rid, ask_id, "the responder keeps the request's own id");
    assert_eq!(
        b_spool.get(Dir::Inbox, &rid).unwrap().unwrap().state,
        "consent"
    );

    owl_ok(b.home(), &["allow", &fp(&a), "--once"]);
    owl_ok(b.home(), &["draft", &rid]);
    // The human sees the exact bytes before sending.
    let shown = owl_ok(b.home(), &["show", &rid]);
    let redacted = TIP.replace("api_key: hunter2", "[redacted]");
    assert!(shown.contains("content:"), "show: {shown}");
    assert!(shown.contains(&redacted), "show: {shown}");
    owl_ok(b.home(), &["send", &rid]);

    // A pulls: the reply lands in `inbox/`, verified.
    owl_ok(a_home.path(), &["--quiet", "status"]);
    let a_path = a_home.path().to_path_buf();
    let a_clone = id(1);
    // The client is blocking (reqwest::blocking); a tokio worker must not host it.
    let pulled = tokio::task::spawn_blocking(move || pull_once(&a_clone, &a_path))
        .await
        .unwrap();
    let shown = owl_ok(a_home.path(), &["show", &pulled]);
    assert!(
        shown.contains(&redacted),
        "the content, byte-identical: {shown}"
    );
    let sha = hex(&Sha256::digest(redacted.as_bytes()));
    assert!(
        shown.contains(&format!("sha256 {sha} — verified")),
        "show: {shown}"
    );
    // The commit the responder resolved travels to the asker, on the record it
    // received — not only on the envelope the responder kept.
    let received: Value = serde_json::from_str(
        &a_spool
            .get(Dir::Inbox, &pulled)
            .unwrap()
            .unwrap()
            .raw
            .clone(),
    )
    .unwrap();
    let head = rev_parse(repo.path(), "main");
    assert_eq!(head.len(), 40, "a full commit id");
    assert_eq!(
        received["body"]["ref_resolved"].as_str(),
        Some(head.as_str()),
        "the asker's reply names the commit the responder read"
    );

    // The verdict is not a property of the plain format: `--json` and `--format claude` say
    // the same thing about the same bytes, first for the untouched reply.
    let verified = format!("sha256 {sha} — verified");
    let rendered = owl_ok(a_home.path(), &["show", &pulled, "--format", "claude"]);
    assert!(rendered.contains(&verified), "claude: {rendered}");
    let as_json = owl_ok(a_home.path(), &["--json", "show", &pulled]);
    let v: Value = serde_json::from_str(&as_json).unwrap();
    assert_eq!(v["sha256_verified"], json!(true), "json: {as_json}");

    // The tamper twin: the same record with one byte changed exits 1 and says so — in every
    // mode, because the mod and the skill read the rendered and the JSON form, not the plain.
    let rec = a_spool.get(Dir::Inbox, &pulled).unwrap().unwrap();
    let mut v: Value = serde_json::from_str(&rec.raw).unwrap();
    v["body"]["content"] = json!(format!("{redacted}tampered"));
    let tampered = Record {
        raw: serde_json::to_string(&v).unwrap(),
        ..rec
    };
    a_spool.put(Dir::Inbox, &pulled, &tampered).unwrap();
    const MISMATCH: &str = "sha256 mismatch — the content does not match its digest";
    for args in [
        vec!["show", &pulled],
        vec!["show", &pulled, "--format", "claude"],
    ] {
        let (code, out, err) = owl(a_home.path(), &args);
        assert_eq!(code, 1, "a mismatch must exit 1 for {args:?}: {err}");
        assert!(out.contains(MISMATCH), "{args:?}: {out}");
        assert!(!out.contains(&verified), "{args:?}: {out}");
    }
    let (code, out, err) = owl(a_home.path(), &["--json", "show", &pulled]);
    assert_eq!(code, 1, "a mismatch must exit 1 under --json: {err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["sha256_verified"], json!(false), "json: {out}");
}

/// One pull of B's outbox into A's spool through the real ingestion path; returns the id of
/// the record it stored.
fn pull_once(a: &Identity, a_home: &Path) -> String {
    let spool = Spool::new(a_home).unwrap();
    let book = owlpost::contacts::ContactBook::load(a_home, a_home).unwrap();
    let contact = book.resolve("Bea").unwrap().clone();
    let open = owlpost::pull::open_asks(&spool).unwrap();
    let iroh = owlpost::client::Iroh::from_home(a_home);
    let envs = owlpost::client::fetch_outbox(a, &contact, &iroh).unwrap();
    assert!(!envs.is_empty(), "B's outbox is empty");
    for env in &envs {
        owlpost::pull::ingest_envelope(a, &contact, &iroh, &spool, &open, env).unwrap();
    }
    spool
        .list(Dir::Inbox, |_| true)
        .unwrap()
        .into_iter()
        .map(|(i, _)| i)
        .next()
        .expect("the reply landed in the asker's inbox")
}

/// §9: the 200-line display cap, at every call site that prints content. A 250-line file is
/// shown through `owl show` on the responder (plain and `--format claude`) and through
/// `owl show` on the asker's received reply: each prints line 200, none prints line 201, and
/// each ends the block with the documented trailer. Every fixture line carries a delimited
/// token, so `L-20-END` is never a substring of `L-200-END`.
#[test]
fn the_display_cap_cuts_at_two_hundred_lines_in_every_view() {
    const LINES: usize = 250;
    let text: String = (1..=LINES).map(|n| format!("L-{n}-END\n")).collect();
    let cut = content::CONTENT_SHOW_LINES;
    let sha = hex(&Sha256::digest(text.as_bytes()));
    let trailer = format!(
        "… {} more lines — {} bytes, sha256 {sha}",
        LINES - cut,
        text.len()
    );
    let kept = format!("L-{cut}-END");
    let dropped = format!("L-{}-END", cut + 1);
    assert!(
        text.len() < MAX_CONTENT_BYTES,
        "the cut under test is the display one"
    );

    // The responder's two views of its own draft: plain (`cli::show`) and the rendered
    // message table (`render::record_block`).
    let d = drafting(|_| {}, content_at_path("big.txt", Some("main")));
    commit_file(d._repo.path(), "big.txt", text.as_bytes());
    owl_ok(d.home.path(), &["draft", &d.id]);
    let stored = content::stored(
        &Spool::new(d.home.path())
            .unwrap()
            .get(Dir::Inbox, &d.id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(stored.text, text, "nothing is cut on the way to the record");
    assert!(!stored.truncated, "well under the wire cap");
    assert_eq!(stored.sha256, sha);
    for args in [
        vec!["show", d.id.as_str()],
        vec!["show", d.id.as_str(), "--format", "claude"],
    ] {
        let out = owl_ok(d.home.path(), &args);
        assert!(
            out.contains(&kept),
            "{args:?}: line {cut} must be shown: {out}"
        );
        assert!(
            !out.contains(&dropped),
            "{args:?}: line {} must not be shown: {out}",
            cut + 1
        );
        assert!(out.contains(&trailer), "{args:?}: trailer missing: {out}");
    }

    // The asker's view of the reply it received (`cli::show`, the `ContentReply` arm).
    let (a, b) = (id(1), id(2));
    let home = tempfile::tempdir().unwrap();
    prepare_home_with(home.path(), &a, &[Peer::new(&b, "Bea", None)], |_| {});
    let req = request(&a, &b, Some("main"));
    let reply = Payload::content_reply(&req, &text, &sha, Some(&"c".repeat(40)), false, 0);
    Spool::new(home.path())
        .unwrap()
        .put(
            Dir::Inbox,
            &reply.id,
            &common::record(&Envelope::sign(&reply, &b), "pending"),
        )
        .unwrap();
    for args in [
        vec!["show", reply.id.as_str()],
        vec!["show", reply.id.as_str(), "--format", "claude"],
    ] {
        let out = owl_ok(home.path(), &args);
        assert!(
            out.contains(&kept),
            "asker {args:?}: line {cut} must be shown: {out}"
        );
        assert!(
            !out.contains(&dropped),
            "asker {args:?}: line {} must not be shown: {out}",
            cut + 1
        );
        assert!(
            out.contains(&trailer),
            "asker {args:?}: trailer missing: {out}"
        );
        // The digest is still of the whole content, so the cut view is still verified.
        assert!(
            out.contains(&format!("sha256 {sha} — verified")),
            "asker {args:?}: {out}"
        );
    }

    // `verify_line`'s third case, through `owl show`: a reply the responder cut is a
    // deliberate prefix, so its digest cannot match and the line says what arrived instead
    // of crying tamper — and it exits 0.
    let head = text[..text.len() / 2].to_string();
    let cut_reply = Payload::content_reply(&req, &head, &sha, None, true, 0);
    Spool::new(home.path())
        .unwrap()
        .put(
            Dir::Inbox,
            &cut_reply.id,
            &common::record(&Envelope::sign(&cut_reply, &b), "pending"),
        )
        .unwrap();
    let out = owl_ok(home.path(), &["show", &cut_reply.id]);
    assert!(
        out.contains(&format!(
            "truncated — {} bytes received, sha256 {sha} is of the full content",
            head.len()
        )),
        "truncated: {out}"
    );
    assert!(!out.contains("sha256 mismatch"), "truncated: {out}");
}

// ---------------------------------------------------------------- the thread

/// The four event kinds appear on the right side of the thread, in `ts` order, and a
/// question of the same thread interleaves with them without either disappearing.
#[test]
fn thread_shows_the_content_events_on_both_sides() {
    let home = tempfile::tempdir().unwrap();
    let (a, b) = (id(1), id(2));
    prepare_home_with(home.path(), &b, &[Peer::new(&a, "Ana", None)], |_| {});
    let spool = Spool::new(home.path()).unwrap();
    let ctx = "thread-1".to_string();

    // The responder's side of a finished content exchange, and an ordinary question of the
    // same thread stamped *between* its events, so the sort is really by `ts`.
    let mut req = request(&a, &b, Some("main"));
    req.context_id = Some(ctx.clone());
    let mut rec = common::record(&Envelope::sign(&req, &a), "answered");
    rec.meta = json!({ "peer": fp(&a) });
    push_at(&mut rec, "2026-09-01T10:00:00Z", "content-requested", None);
    push_at(
        &mut rec,
        "2026-09-01T10:05:00Z",
        "content-drafted",
        Some("human"),
    );
    push_at(
        &mut rec,
        "2026-09-01T10:09:00Z",
        "content-sent",
        Some("human"),
    );
    spool.put(Dir::Done, &req.id, &rec).unwrap();

    let mut q = Payload::question(&fp(&a), &fp(&b), PROJECT, Some(FILE), "why?");
    q.context_id = Some(ctx.clone());
    let mut qrec = common::record(&Envelope::sign(&q, &a), "answered");
    qrec.meta = json!({ "peer": fp(&a) });
    push_at(&mut qrec, "2026-09-01T10:02:00Z", "received", None);
    push_at(&mut qrec, "2026-09-01T10:07:00Z", "sent", Some("human"));
    spool.put(Dir::Done, &q.id, &qrec).unwrap();

    let out = owl_ok(home.path(), &["--json", "thread", "Ana"]);
    let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
    let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "content-requested",
            "received",
            "content-drafted",
            "sent",
            "content-sent"
        ],
        "interleaved by ts, not grouped by record"
    );
    // The content rows name the request; every row keeps the thread.
    let first = &rows[0];
    assert_eq!(first["type"], "content");
    assert_eq!(first["text"], format!("{FILE}@main"));
    assert_eq!(first["dir"], "in", "the peer asked us");
    assert!(rows.iter().all(|r| r["context_id"] == ctx));

    // `--context` keeps both records of the thread, and drops one of another thread.
    let mut other = request(&a, &b, None);
    other.context_id = Some("thread-2".into());
    let mut orec = common::record(&Envelope::sign(&other, &a), "answered");
    orec.meta = json!({ "peer": fp(&a) });
    push_at(&mut orec, "2026-09-01T11:00:00Z", "content-requested", None);
    spool.put(Dir::Done, &other.id, &orec).unwrap();
    let out = owl_ok(home.path(), &["--json", "thread", "Ana", "--context", &ctx]);
    let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(rows.len(), 5, "only this thread");
}

/// The asker's side: the `asks/` record carries `content-requested` (out) and the
/// received reply `content-received` (in).
#[test]
fn asker_side_carries_requested_and_received() {
    let home = tempfile::tempdir().unwrap();
    let (a, b) = (id(1), id(2));
    prepare_home_with(home.path(), &a, &[Peer::new(&b, "Bea", None)], |_| {});
    let spool = Spool::new(home.path()).unwrap();
    let mut req = request(&a, &b, Some("main"));
    req.context_id = Some("t".into());
    let mut areq = common::record(&Envelope::sign(&req, &a), "answered");
    areq.meta = json!({ "peer": fp(&b) });
    push_at(
        &mut areq,
        "2026-09-01T10:00:00Z",
        "content-requested",
        Some("human"),
    );
    spool.put(Dir::Done, &req.id, &areq).unwrap();

    let reply = Payload::content_reply(&req, "body\n", "deadbeef", Some("abc"), false, 0);
    let mut rrec = common::record(&Envelope::sign(&reply, &b), "pending");
    rrec.meta = json!({ "peer": fp(&b) });
    push_at(&mut rrec, "2026-09-01T10:10:00Z", "content-received", None);
    spool.put(Dir::Inbox, &reply.id, &rrec).unwrap();

    let out = owl_ok(home.path(), &["--json", "thread", "Bea"]);
    let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
    let seen: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r["kind"].as_str().unwrap(), r["dir"].as_str().unwrap()))
        .collect();
    assert_eq!(
        seen,
        [("content-requested", "out"), ("content-received", "in")]
    );
    assert_eq!(rows[1]["type"], "content-reply");
    assert_eq!(rows[1]["text"], "body\n");
    assert_eq!(rows[1]["harness"], "human");
}

/// Appends one event with an explicit timestamp (`events::push` stamps "now", which cannot
/// produce the out-of-order fixture the interleave test needs).
fn push_at(rec: &mut Record, ts: &str, kind: &str, by: Option<&str>) {
    let meta = rec.meta.as_object_mut().expect("meta is an object");
    let list = meta
        .entry("events".to_string())
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("events is an array");
    let mut ev = json!({ "ts": ts, "kind": kind });
    if let Some(b) = by {
        ev["by"] = json!(b);
    }
    list.push(ev);
}

// ---------------------------------------------------------------- display and digest

/// The display cap: the first 200 lines plus one line naming what was left out.
#[test]
fn show_caps_the_content_at_two_hundred_lines() {
    let short = (0..content::CONTENT_SHOW_LINES)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(content::display(&short, "abc"), short, "exactly at the cap");
    let long = format!("{short}\nline extra\nline more");
    let shown = content::display(&long, "abc");
    assert!(shown.starts_with(&short));
    assert!(
        shown.ends_with(&format!(
            "… 2 more lines — {} bytes, sha256 abc",
            long.len()
        )),
        "shown tail: {}",
        &shown[shown.len().saturating_sub(80)..]
    );
    assert_eq!(
        shown.lines().count(),
        content::CONTENT_SHOW_LINES + 1,
        "200 lines plus the summary"
    );
}

/// The digest verdict, both halves, plus the truncated third case.
#[test]
fn verify_line_reports_match_mismatch_and_truncation() {
    let text = "hello\n";
    let sha = hex(&Sha256::digest(text.as_bytes()));
    let (line, ok) = content::verify_line(text, &sha, false);
    assert!(ok);
    assert_eq!(line, format!("sha256 {sha} — verified"));
    let (line, ok) = content::verify_line("hello!\n", &sha, false);
    assert!(!ok);
    assert_eq!(
        line,
        "sha256 mismatch — the content does not match its digest"
    );
    // A truncated reply is a prefix by design: its digest cannot match and must not read as
    // a tamper.
    let (line, ok) = content::verify_line("hel", &sha, true);
    assert!(ok);
    assert!(line.starts_with("truncated — 3 bytes received"), "{line}");
}

/// `escapes_root`'s hostile shapes reach the API, not just the unit: a backslash-separated
/// traversal is refused like a slash-separated one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backslash_traversal_is_refused_too() {
    let (repo, _) = fixture_repo();
    let a = id(1);
    let d = responder(
        repo.path(),
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
    )
    .await;
    let c = client(Some(&a), &d.id);
    let p = Payload::content(
        &fp(&a),
        &d.fp(),
        Some(PROJECT),
        None,
        Some(r"..\..\etc\passwd"),
        None,
    );
    let resp = post_envelope(&c, &d, &Envelope::sign(&p, &a)).await;
    assert_error(resp, 400, "path escapes the project").await;
    assert!(d.spool().list(Dir::Inbox, |_| true).unwrap().is_empty());
}

/// A `content` tag whose body also fits a question must not be spooled as a question: the
/// untagged `Body` would otherwise pick the wrong variant and the wrong rules with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_content_tag_with_a_question_body_is_refused() {
    let (repo, _) = fixture_repo();
    let a = id(1);
    let d = responder(
        repo.path(),
        &[Peer::new(&a, "Ana", Some(policy(Mode::Auto, None)))],
    )
    .await;
    let c = client(Some(&a), &d.id);
    let mut v = serde_json::to_value(request(&a, &d.id, Some("main"))).unwrap();
    v["body"]["question"] = json!("why?");
    let raw = serde_json::to_string(&v).unwrap();
    let sig = owlpost::identity::sig_string(&a.sign(raw.as_bytes()));
    let resp = post_envelope(&c, &d, &Envelope { raw, sig }).await;
    assert_error(resp, 400, "body does not match type").await;
    assert!(d.spool().list(Dir::Inbox, |_| true).unwrap().is_empty());
}

/// The asker's cache is never fed by a content reply either — the guard in
/// `pull::ingest_envelope` and not only the `meta.threaded` flag `owl request` happens to
/// write. The fixture ask deliberately carries no `threaded`, so only the kind check stands
/// between the reply and `cache/`.
#[test]
fn a_content_reply_never_enters_the_askers_cache() {
    let home = tempfile::tempdir().unwrap();
    let (a, b) = (id(1), id(2));
    prepare_home_with(home.path(), &a, &[Peer::new(&b, "Bea", None)], |_| {});
    let spool = Spool::new(home.path()).unwrap();
    let req = request(&a, &b, Some("main"));
    let mut rec = common::record(&Envelope::sign(&req, &a), "waiting");
    // No `threaded`, and a non-empty hash: every flag that could shortcut the guard is off.
    rec.meta = json!({ "peer": fp(&b), "hash": "deadbeef" });
    spool.put(Dir::Asks, &req.id, &rec).unwrap();

    let reply = Payload::content_reply(&req, "body\n", "abc", Some("c0ffee"), false, 0);
    let env = Envelope::sign(&reply, &b);
    let book = owlpost::contacts::ContactBook::load(home.path(), home.path()).unwrap();
    let contact = book.resolve("Bea").unwrap().clone();
    let open = owlpost::pull::open_asks(&spool).unwrap();
    assert!(
        open.contains_key(&req.id),
        "the content ask must be an open ask"
    );
    let iroh = owlpost::client::Iroh::from_home(home.path());
    owlpost::pull::ingest_envelope(&a, &contact, &iroh, &spool, &open, &env).unwrap();

    assert!(
        spool.get(Dir::Inbox, &reply.id).unwrap().is_some(),
        "the reply is still stored"
    );
    assert!(
        spool.list(Dir::Cache, |_| true).unwrap().is_empty(),
        "a content reply must never be cached"
    );
    // The positive twin: an ordinary answer to an unthreaded ask still is.
    let q = Payload::question(&fp(&a), &fp(&b), PROJECT, Some(FILE), "why?");
    let mut qrec = common::record(&Envelope::sign(&q, &a), "waiting");
    qrec.meta = json!({ "peer": fp(&b), "hash": "cafe" });
    spool.put(Dir::Asks, &q.id, &qrec).unwrap();
    let ans = Payload::answer(&q, "because", "claude", 0, false);
    let open = owlpost::pull::open_asks(&spool).unwrap();
    owlpost::pull::ingest_envelope(
        &a,
        &contact,
        &iroh,
        &spool,
        &open,
        &Envelope::sign(&ans, &b),
    )
    .unwrap();
    assert_eq!(
        spool.list(Dir::Cache, |_| true).unwrap().len(),
        1,
        "an ordinary answer still feeds the asker cache"
    );
}

/// The four event kinds are written by the code, not only rendered: this walks the whole
/// exchange over two daemons again and reads `owl thread` on both sides. Nothing here builds
/// an event by hand, so a typo in any of the four writers fails it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_exchange_writes_all_four_event_kinds() {
    let (repo, _) = fixture_repo();
    let a = id(1);
    let b = responder(
        repo.path(),
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
    )
    .await;
    let a_home = tempfile::tempdir().unwrap();
    prepare_home_with(a_home.path(), &a, &[], |_| {});
    write_contact_full(
        a_home.path(),
        &Peer::new(&b.id, "Bea", None),
        &[&b.addr.to_string()],
        &["bea@example.org"],
    );
    owl_ok(
        a_home.path(),
        &["request", "Bea", PROJECT, FILE, "--ref", "main"],
    );
    let rid = only_id(&b.spool(), Dir::Inbox);
    owl_ok(b.home(), &["allow", &fp(&a), "--once"]);
    owl_ok(b.home(), &["draft", &rid]);
    owl_ok(b.home(), &["send", &rid]);
    let a_path = a_home.path().to_path_buf();
    tokio::task::spawn_blocking(move || pull_once(&id(1), &a_path))
        .await
        .unwrap();

    let kinds = |home: &Path, peer: &str| -> Vec<String> {
        let out = owl_ok(home, &["--json", "thread", peer]);
        serde_json::from_str::<Vec<Value>>(&out)
            .unwrap()
            .iter()
            .map(|r| r["kind"].as_str().unwrap().to_string())
            .collect()
    };
    let responder_side = kinds(b.home(), "Ana");
    for want in ["content-requested", "content-drafted", "content-sent"] {
        assert!(
            responder_side.iter().any(|k| k == want),
            "the responder's thread lacks {want}: {responder_side:?}"
        );
    }
    // Order: the request arrives, is drafted, then sent.
    let at = |k: &str| responder_side.iter().position(|x| x == k).unwrap();
    assert!(at("content-requested") < at("content-drafted"));
    assert!(at("content-drafted") < at("content-sent"));

    let asker_side = kinds(a_home.path(), "Bea");
    for want in ["content-requested", "content-received"] {
        assert!(
            asker_side.iter().any(|k| k == want),
            "the asker's thread lacks {want}: {asker_side:?}"
        );
    }
    // The drafted event carries what the human needs to judge it.
    let out = owl_ok(b.home(), &["--json", "thread", "Ana"]);
    let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
    let drafted = rows
        .iter()
        .find(|r| r["kind"] == "content-drafted")
        .expect("the drafted row");
    assert_eq!(drafted["by"], "human", "no model drafted it");
    assert_eq!(drafted["detail"]["redactions"], 1);
    assert_eq!(drafted["detail"]["truncated"], false);
    assert!(drafted["detail"]["bytes"].as_u64().unwrap() > 0);
}

/// `owl edit` refuses a content draft: rewriting the bytes while the signed `sha256` still
/// described the file would reach the asker as a tamper. The twin: it still edits a question.
#[test]
fn edit_refuses_a_content_draft() {
    let d = drafting(|_| {}, content_at(Some("main")));
    owl_ok(d.home.path(), &["draft", &d.id]);
    let before = Spool::new(d.home.path())
        .unwrap()
        .get(Dir::Inbox, &d.id)
        .unwrap()
        .unwrap();
    let (code, _, err) = owl(d.home.path(), &["edit", &d.id]);
    assert_eq!(code, 1, "edit must refuse a content record: {err}");
    assert!(
        err.contains("is a content request — its draft is the file itself and cannot be edited"),
        "stderr: {err}"
    );
    let after = Spool::new(d.home.path())
        .unwrap()
        .get(Dir::Inbox, &d.id)
        .unwrap()
        .unwrap();
    assert_eq!(after.draft, before.draft, "the draft is untouched");
}

/// `owl show --format claude` — what the wake file and the mod's Show button print — carries
/// the drafted content, so the human reads the exact bytes before `owl send`.
#[test]
fn the_claude_format_shows_the_drafted_content() {
    let d = drafting(|_| {}, content_at(Some("main")));
    let redacted = TIP.replace("api_key: hunter2", "[redacted]");
    // Before the draft: the request only, no content anywhere.
    let before = owl_ok(d.home.path(), &["show", &d.id, "--format", "claude"]);
    assert!(before.contains("asks for"), "show: {before}");
    assert!(
        !before.contains(&redacted),
        "nothing is read before draft: {before}"
    );

    owl_ok(d.home.path(), &["draft", &d.id]);
    let after = owl_ok(d.home.path(), &["show", &d.id, "--format", "claude"]);
    assert!(after.contains("content:"), "show: {after}");
    assert!(after.contains(&redacted), "the exact bytes: {after}");
    assert!(
        after.contains(&format!(
            "sha256: {}",
            hex(&Sha256::digest(redacted.as_bytes()))
        )),
        "show: {after}"
    );
}
