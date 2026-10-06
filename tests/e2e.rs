//! End-to-end suite (design §12). Two `owl daemon --foreground` subprocesses A and B,
//! each with its own temp home, port 0, and its own `OWLPOST_NOTIFY_CMD` logging script; one
//! fixture git repo carrying both peer files under `.agents/peers/`; B's `projects` maps the
//! fixture repo and B answers with `tests/fixtures/fake-harness.sh`. Every `owl` command runs
//! from the fixture repo so contact resolution goes through the repo provider, exactly as a
//! user's shell would. No real harness is ever invoked; everything binds 127.0.0.1:0.
//!
//! The test for `scripts/e2e-real.sh` only checks its fail-fast guards — it never gets as
//! far as running a harness.

mod common;

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::{PATH, PROJECT, Peer, client, fp, id, policy, prepare_home_with, question, signed};
use owlpost::config::{Config, Harness};
use owlpost::contacts::Mode;
use owlpost::envelope::{Envelope, Payload};
use owlpost::identity::{Identity, pubkey_string};
use owlpost::spool::{Dir, Spool};
use serde_json::{Value, json};
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");
const FAKE_HARNESS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/fake-harness.sh"
);
const E2E_REAL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/e2e-real.sh");
const QUESTION: &str = "Where is the retry policy defined?";
/// The secret line the fake harness always emits (see `tests/fixtures/fake-harness.sh`); the
/// default `responder.redact` patterns must scrub it before the answer leaves B.
const SECRET: &str = "sk-test-123456";
/// §9 hook injection for exactly one unseen inbox record from Bea.
const CLAUDE_ONE: &str = r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"🦉 owlpost: 1 new answer (Bea 1). Say \"show owlpost inbox\" or run `owl inbox`."}}"#;
/// Bound for every wait in the loop; the whole `manual_loop` must stay under 60 s.
const WAIT: Duration = Duration::from_secs(15);

// ---- fixture repo -------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A committed git repo with `src/client.rs` and an empty `.agents/peers/` directory.
fn fixture_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/client.rs"),
        "pub const RETRIES: u32 = 3; // retry policy\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.join(".agents/peers")).unwrap();
    git(repo, &["add", "."]);
    git(
        repo,
        &[
            "-c",
            "user.email=dev@example.org",
            "-c",
            "user.name=Dev",
            "commit",
            "-q",
            "-m",
            "fixture",
        ],
    );
    dir
}

/// Writes `<repo>/.agents/peers/<name>.json` — the file a peer commits after `owl contact
/// export` (§5).
fn write_peer_file(repo: &Path, id: &Identity, name: &str, endpoints: &[String]) {
    let v = json!({
        "name": name,
        "emails": [format!("{}@example.org", name.to_lowercase())],
        "pubkey": pubkey_string(&id.verifying_key()),
        "endpoints": endpoints,
    });
    std::fs::write(
        repo.join(".agents/peers").join(format!("{name}.json")),
        serde_json::to_vec_pretty(&v).unwrap(),
    )
    .unwrap();
}

// ---- daemon subprocess --------------------------------------------------------------------

/// One peer: a temp home, its identity, the running `owl daemon --foreground` child, and the
/// notify log its `OWLPOST_NOTIFY_CMD` script appends to. `commands` counts every `owl`
/// invocation made against this home by the test (none on B in `auto_loop`).
struct Node {
    home: TempDir,
    id: Identity,
    name: &'static str,
    child: Child,
    addr: SocketAddr,
    notify_log: PathBuf,
    commands: AtomicUsize,
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Node {
    fn path(&self) -> &Path {
        self.home.path()
    }

    fn spool(&self) -> Spool {
        Spool::new(self.path()).unwrap()
    }

    fn fp(&self) -> String {
        fp(&self.id)
    }

    /// Runs `owl --home <home> <args>` from `cwd` (the fixture repo) and returns
    /// (exit code, stdout, stderr).
    fn owl(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        self.commands.fetch_add(1, Ordering::SeqCst);
        let out = Command::new(OWL)
            .env_remove("OWLPOST_HOME")
            .env_remove("EDITOR")
            .current_dir(cwd)
            .arg("--home")
            .arg(self.path())
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    fn owl_ok(&self, cwd: &Path, args: &[&str]) -> String {
        let (code, out, err) = self.owl(cwd, args);
        assert_eq!(
            code, 0,
            "{} owl {args:?}: stdout {out:?} stderr {err:?}",
            self.name
        );
        out
    }

    /// Lines of the notify log (`owlpost|<text>` per notification).
    fn notifications(&self) -> Vec<String> {
        std::fs::read_to_string(&self.notify_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn stop(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }

    fn daemon_log(&self) -> String {
        std::fs::read_to_string(self.path().join("daemon.log")).unwrap_or_default()
    }
}

/// Shell script appending `"$1|$2"` to `<home>/notify.log` per call (the `notify-send`
/// argument shape, see `notify::CMD_ENV`).
fn logging_script(home: &Path) -> (PathBuf, PathBuf) {
    let script = home.join("notify.sh");
    let log = home.join("notify.log");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s|%s\\n' \"$1\" \"$2\" >> '{}'\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, log)
}

fn fake_harness() -> Harness {
    Harness {
        cmd: vec![FAKE_HARNESS.into(), "{prompt}".into()],
        answer_path: "raw".into(),
        enabled: true,
        disabled_reason: None,
        env: Default::default(),
        model: None,
    }
}

/// Prepares a home for `seed` (key, config with `listen = 127.0.0.1:0`, `notify = true`,
/// `pull_interval_secs = 1`, local contacts `peers`) and starts `owl daemon --foreground`
/// from `repo` with the notify override pointing at the logging script. Returns once the
/// daemon wrote `daemon.addr`.
fn spawn_node(
    seed: u8,
    name: &'static str,
    repo: &Path,
    peers: &[Peer<'_>],
    tweak: impl FnOnce(&mut Config),
) -> Node {
    let home = tempfile::tempdir().unwrap();
    let id = id(seed);
    prepare_home_with(home.path(), &id, peers, |cfg| {
        cfg.name = name.into();
        cfg.emails = vec![format!("{}@example.org", name.to_lowercase())];
        cfg.notify = true;
        cfg.pull_interval_secs = 1;
        tweak(cfg);
    });
    let (script, notify_log) = logging_script(home.path());
    let log = std::fs::File::create(home.path().join("daemon.log")).unwrap();
    let child = Command::new(OWL)
        .env_remove("OWLPOST_HOME")
        .env("OWLPOST_NOTIFY_CMD", &script)
        .env("RUST_LOG", "debug")
        .current_dir(repo)
        .arg("--home")
        .arg(home.path())
        .args(["daemon", "--foreground"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    let addr_file = home.path().join("daemon.addr");
    let deadline = Instant::now() + Duration::from_secs(10);
    let addr: SocketAddr = loop {
        if let Ok(s) = std::fs::read_to_string(&addr_file)
            && let Ok(a) = s.trim().parse()
        {
            break a;
        }
        assert!(
            Instant::now() < deadline,
            "{name}: no daemon.addr within 10 s:\n{}",
            std::fs::read_to_string(home.path().join("daemon.log")).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    Node {
        home,
        id,
        name,
        child,
        addr,
        notify_log,
        commands: AtomicUsize::new(0),
    }
}

/// B: responder with the fake harness, `projects[PROJECT] = repo`, no local contacts (A comes
/// from the repo peer file). `policy_for_a` is written as a local overlay when given.
fn spawn_b(repo: &Path, a: &Identity, policy_for_a: Option<owlpost::contacts::Policy>) -> Node {
    let overlay = [Peer::new(a, "Ana", policy_for_a)];
    let peers: &[Peer<'_>] = if overlay[0].policy.is_some() {
        &overlay
    } else {
        &[]
    };
    let repo_path = repo.to_string_lossy().into_owned();
    spawn_node(2, "Bea", repo, peers, |cfg| {
        cfg.harnesses.insert("fake".into(), fake_harness());
        cfg.responder.harness = "fake".into();
        cfg.projects.insert(PROJECT.into(), repo_path);
    })
}

/// A: pulls every second; B is reached through the repo peer file's endpoint.
fn spawn_a(repo: &Path) -> Node {
    spawn_node(1, "Ana", repo, &[], |cfg| cfg.responder.enabled = false)
}

/// The peer files both sides read: A's is written before B starts (B pins A's key at
/// startup), B's after (it carries B's port-0 address).
struct Pair {
    repo: TempDir,
    a: Node,
    b: Node,
}

fn pair(policy_for_a: Option<owlpost::contacts::Policy>) -> Pair {
    let repo = fixture_repo();
    let a_id = id(1);
    write_peer_file(repo.path(), &a_id, "Ana", &[]);
    let b = spawn_b(repo.path(), &a_id, policy_for_a);
    write_peer_file(repo.path(), &b.id, "Bea", &[b.addr.to_string()]);
    git(repo.path(), &["add", ".agents"]);
    let a = spawn_a(repo.path());
    assert_eq!(fp(&a.id), fp(&a_id));
    Pair { repo, a, b }
}

/// Bounded wait for `pred` (checked every 100 ms); panics with `what` on timeout.
fn wait_for(limit: Duration, what: &str, mut pred: impl FnMut() -> bool) {
    let start = Instant::now();
    while !pred() {
        assert!(start.elapsed() < limit, "timed out after {limit:?}: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn ids(spool: &Spool, dir: Dir) -> Vec<String> {
    spool
        .list(dir, |_| true)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

fn state(spool: &Spool, dir: Dir, id: &str) -> Option<String> {
    spool.get(dir, id).unwrap().map(|r| r.state)
}

/// `owl ask Bea <PATH> <QUESTION> --project <PROJECT>` from the repo, as A.
fn ask(p: &Pair) -> (i32, String, String) {
    p.a.owl(
        p.repo.path(),
        &["ask", "Bea", PATH, QUESTION, "--project", PROJECT],
    )
}

fn accepted_id(out: &str) -> String {
    out.trim()
        .strip_prefix("accepted ")
        .unwrap_or_else(|| panic!("ask stdout {out:?}"))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

/// A's inbox holds exactly one record; returns its id.
fn wait_for_answer(a: &Node) -> String {
    let spool = a.spool();
    wait_for(WAIT, "the answer in A's inbox", || {
        !ids(&spool, Dir::Inbox).is_empty()
    });
    let inbox = ids(&spool, Dir::Inbox);
    assert_eq!(inbox.len(), 1, "A inbox {inbox:?}");
    inbox.into_iter().next().unwrap()
}

fn count_containing(lines: &[String], needle: &str) -> usize {
    lines.iter().filter(|l| l.contains(needle)).count()
}

#[test]
fn manual_loop() {
    let started = Instant::now();
    let mut p = pair(None);
    let repo = p.repo.path();
    let b_spool = p.b.spool();
    let a_spool = p.a.spool();

    // A asks: 202, `accepted <id>` on stdout, the ask is open in A's asks/.
    let (code, out, err) = ask(&p);
    assert_eq!(code, 0, "ask: stdout {out:?} stderr {err:?}");
    let qid = accepted_id(&out);
    assert_eq!(state(&a_spool, Dir::Asks, &qid).as_deref(), Some("waiting"));

    // B holds it for consent (no policy for Ana): the record is in B's inbox as `consent`
    // and the question notification fired once.
    assert_eq!(
        state(&b_spool, Dir::Inbox, &qid).as_deref(),
        Some("consent")
    );
    wait_for(WAIT, "B's question notification", || {
        !p.b.notifications().is_empty()
    });
    assert_eq!(
        p.b.notifications(),
        vec![format!("owlpost|Ana asks about {PATH}")]
    );

    // B: `owl allow Ana --once` releases exactly that record to `pending`, writes no policy.
    let out = p.b.owl_ok(repo, &["allow", "Ana", "--once"]);
    assert!(out.contains("released 1"), "{out}");
    assert_eq!(
        state(&b_spool, Dir::Inbox, &qid).as_deref(),
        Some("pending")
    );

    // B: `--count` is 1 and `owl inbox` lists it (the listing marks it seen).
    assert_eq!(p.b.owl_ok(repo, &["inbox", "--count"]), "1\n");
    let out = p.b.owl_ok(repo, &["inbox"]);
    assert!(out.contains(&qid) && out.contains("Ana"), "{out}");
    assert_eq!(p.b.owl_ok(repo, &["inbox", "--count"]), "0\n");
    assert_eq!(p.b.owl_ok(repo, &["inbox", "--count", "--all"]), "1\n");

    // B: `owl draft <id>` runs the fake harness in the mapped checkout; the secret is redacted.
    let out = p.b.owl_ok(repo, &["draft", &qid]);
    assert!(out.contains("[redacted]"), "{out}");
    assert!(!out.contains(SECRET), "secret leaked: {out}");
    assert_eq!(
        state(&b_spool, Dir::Inbox, &qid).as_deref(),
        Some("drafted")
    );

    // B: `owl send <id>` signs the draft into B's outbox.
    let out = p.b.owl_ok(repo, &["send", &qid]);
    let aid = out
        .lines()
        .find_map(|l| l.strip_prefix("sent "))
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or_else(|| panic!("send stdout {out:?}"))
        .to_string();
    assert!(
        out.contains(&format!("(reply to {qid}, to {})", p.a.fp())),
        "{out}"
    );
    assert!(state(&b_spool, Dir::Inbox, &qid).is_none());
    assert_eq!(
        state(&b_spool, Dir::Done, &qid).as_deref(),
        Some("answered")
    );

    // A's daemon pulls it: inbox record, ask moved to done/, B's outbox acked.
    assert_eq!(wait_for_answer(&p.a), aid);
    let rec = a_spool.get(Dir::Inbox, &aid).unwrap().unwrap();
    assert_eq!((rec.state.as_str(), rec.seen), ("pending", false));
    assert_eq!(rec.meta["peer"], p.b.fp());
    assert_eq!(rec.meta["in_reply_to"], qid);
    // The inbox record lands before the ask is moved (pull.rs: store, then move + set_state),
    // so poll for the final state rather than asserting it the instant the record exists.
    wait_for(WAIT, "A's ask moved to done/ as answered", || {
        state(&a_spool, Dir::Done, &qid).as_deref() == Some("answered")
    });
    assert!(state(&a_spool, Dir::Asks, &qid).is_none());
    assert!(ids(&a_spool, Dir::Asks).is_empty());
    wait_for(WAIT, "B's outbox acked", || {
        state(&b_spool, Dir::Done, &aid).as_deref() == Some("acked")
    });
    assert!(state(&b_spool, Dir::Outbox, &aid).is_none());

    // A: the hook injection reports exactly one new record from Bea.
    let out =
        p.a.owl_ok(repo, &["inbox", "--count", "--format", "claude"]);
    assert_eq!(out.trim_end(), CLAUDE_ONE);
    assert_eq!(p.a.owl_ok(repo, &["inbox", "--count"]), "1\n");

    // A: `owl show <id>` prints the answer with the secret redacted and marks it seen.
    let out = p.a.owl_ok(repo, &["show", &aid]);
    assert!(out.contains("type:     answer"), "{out}");
    assert!(out.contains("[redacted]"), "{out}");
    assert!(!out.contains(SECRET), "secret leaked: {out}");
    assert!(out.contains("src/client.rs"), "{out}");
    assert_eq!(p.a.owl_ok(repo, &["inbox", "--count"]), "0\n");
    let shown: Value = serde_json::from_str(&p.a.owl_ok(repo, &["show", &aid, "--json"])).unwrap();
    let answer = shown["payload"]["body"]["answer"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(answer.contains("[redacted]"), "{answer}");

    // Notifications: exactly one question event on B, exactly one answer event on A, and
    // nothing of the other kind on either side.
    wait_for(WAIT, "A's answer notification", || {
        !p.a.notifications().is_empty()
    });
    let (a_n, b_n) = (p.a.notifications(), p.b.notifications());
    assert_eq!(a_n, vec!["owlpost|answer from Bea".to_string()]);
    assert_eq!(b_n, vec![format!("owlpost|Ana asks about {PATH}")]);
    assert_eq!(count_containing(&a_n, "asks about"), 0);
    assert_eq!(count_containing(&b_n, "answer from"), 0);

    // B stopped: the same question is answered from A's cache without a send.
    p.b.stop();
    let (code, out, err) = ask(&p);
    assert_eq!(code, 0, "cached ask: stdout {out:?} stderr {err:?}");
    assert_eq!(out.trim_end(), answer);
    assert!(
        ids(&a_spool, Dir::Asks).is_empty(),
        "cache hit must not open an ask"
    );
    assert_eq!(ids(&a_spool, Dir::Inbox), vec![aid.clone()]);

    // Stopping B changed nothing on A's side of the log.
    assert_eq!(p.a.notifications().len(), 1);
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "manual_loop took {:?}",
        started.elapsed()
    );
}

#[test]
fn auto_loop() {
    let p = pair(Some(policy(Mode::Auto, None)));
    let repo = p.repo.path();
    let b_spool = p.b.spool();

    let (code, out, err) = ask(&p);
    assert_eq!(code, 0, "ask: stdout {out:?} stderr {err:?}");
    let qid = accepted_id(&out);

    // The answer reaches A with no `owl` command run against B's home; B's record went
    // straight from `pending` to done/ (`answered`, then `acked` once A's pull acks it).
    let aid = wait_for_answer(&p.a);
    // `answer::send` puts the outbox record, appends the outgoing log line, then finishes the
    // inbox record — A can see the answer before B's own bookkeeping lands, so poll.
    wait_for(WAIT, "B's inbox record finished as answered", || {
        state(&b_spool, Dir::Inbox, &qid).is_none()
            && state(&b_spool, Dir::Done, &qid).as_deref() == Some("answered")
    });
    wait_for(WAIT, "exactly one outgoing log line on B", || {
        outgoing_lines(&p.b).len() == 1
    });
    assert_eq!(
        p.b.commands.load(Ordering::SeqCst),
        0,
        "no owl command may run on B"
    );
    let rec = p.a.spool().get(Dir::Inbox, &aid).unwrap().unwrap();
    assert_eq!(rec.meta["in_reply_to"], qid);
    let env = Envelope {
        raw: rec.raw.clone(),
        sig: rec.sig.clone(),
    };
    let ans = env.verify(&p.b.id.verifying_key()).expect("signed by B");
    let owlpost::envelope::Body::Answer {
        answer, redactions, ..
    } = &ans.body
    else {
        panic!("not an answer: {ans:?}");
    };
    assert!(answer.contains("[redacted]"), "{answer}");
    assert!(!answer.contains(SECRET), "secret leaked: {answer}");
    assert!(*redactions >= 1);

    // B's outgoing log has exactly one line, and it is the `auto` one.
    let lines = outgoing_lines(&p.b);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["mode"], "auto");
    assert_eq!(lines[0]["question_id"], qid);
    assert_eq!(lines[0]["to"], p.a.fp());
    assert_eq!(lines[0]["harness"], "fake");

    // The answer that arrived is the one B logged, and A's view of it is the same text.
    let out = p.a.owl_ok(repo, &["show", &aid]);
    assert!(out.contains("[redacted]") && !out.contains(SECRET), "{out}");
}

/// Parsed lines of B's `log/outgoing.jsonl` (empty when the file does not exist yet).
fn outgoing_lines(b: &Node) -> Vec<Value> {
    std::fs::read_to_string(b.path().join(owlpost::answer::OUTGOING_LOG))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// `POST /v1/questions` on `node` with `env`, over a client pinned to `node`'s key that
/// presents `who`'s certificate.
async fn post(who: &Identity, node: &Node, env: &Envelope) -> reqwest::Result<reqwest::Response> {
    client(Some(who), &node.id)
        .post(format!("https://{}/v1/questions", node.addr))
        .header("content-type", "application/json")
        .header("X-Owl-Signature", &env.sig)
        .body(env.raw.clone().into_bytes())
        .send()
        .await
}

async fn error_body(resp: reqwest::Response, status: u16) -> (String, reqwest::header::HeaderMap) {
    assert_eq!(resp.status().as_u16(), status);
    let headers = resp.headers().clone();
    let body: Value = resp.json().await.expect("JSON error body");
    (body["error"].as_str().unwrap().to_string(), headers)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn security() {
    let p = pair(None);
    let b_spool = p.b.spool();
    let c = id(3);
    assert_ne!(fp(&c), p.a.fp());

    // (1) C is in nobody's book: the TLS handshake fails, so no request reaches B and B
    // records nothing (no inbox record, no seen-ids file). C's envelope is otherwise valid —
    // it differs from A's only in who signs and presents the certificate.
    let c_env = Envelope::sign(&question(&c, &p.b.id, "let me in?"), &c);
    let err = post(&c, &p.b, &c_env)
        .await
        .expect_err("unknown client must not complete the handshake");
    assert!(
        err.is_connect() || err.is_request(),
        "not a transport error: {err:?}"
    );
    assert!(ids(&b_spool, Dir::Inbox).is_empty());
    assert!(!p.b.path().join("seen-ids.txt").exists());

    // (2) A's genuine envelope is accepted once; the byte-identical replay is 409 and does
    // not spool a second record.
    let q = signed(&p.a.id, &p.b.id, "first?");
    let qid = serde_json::from_str::<Payload>(&q.raw).unwrap().id;
    let resp = post(&p.a.id, &p.b, &q).await.unwrap();
    assert_eq!(resp.status(), 202);
    assert_eq!(ids(&b_spool, Dir::Inbox), vec![qid.clone()]);
    let resp = post(&p.a.id, &p.b, &q).await.unwrap();
    let (error, _) = error_body(resp, 409).await;
    assert_eq!(error, "duplicate id");
    assert_eq!(ids(&b_spool, Dir::Inbox), vec![qid.clone()]);
    assert!(p.b.path().join("seen-ids.txt").exists());

    // (3) Default limits: questions 2..=20 from A are accepted, the 21st within the hour is
    // 429 with Retry-After and is not spooled.
    let default_limit = Config::default().rate_limit_per_peer_per_hour;
    assert_eq!(default_limit, 20, "§3 default");
    for i in 2..=default_limit {
        let resp = post(&p.a.id, &p.b, &signed(&p.a.id, &p.b.id, &format!("q{i}?")))
            .await
            .unwrap();
        assert_eq!(resp.status(), 202, "question {i}");
    }
    let mut inbox = ids(&b_spool, Dir::Inbox);
    inbox.sort();
    assert_eq!(inbox.len(), 20, "{inbox:?}");
    let resp = post(&p.a.id, &p.b, &signed(&p.a.id, &p.b.id, "q21?"))
        .await
        .unwrap();
    let (error, headers) = error_body(resp, 429).await;
    assert_eq!(error, "rate limited");
    let retry: u64 = headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!(
        (1..=180).contains(&retry),
        "20/h bucket: one token per 180 s, got {retry}"
    );
    assert_eq!(
        ids(&b_spool, Dir::Inbox).len(),
        20,
        "the 21st is not spooled"
    );

    // B's daemon log: 20 questions spooled, every one from A, none from C.
    let log = p.b.daemon_log();
    let spooled: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("question spooled"))
        .collect();
    assert_eq!(spooled.len(), 20, "{log}");
    assert!(spooled.iter().all(|l| l.contains(&p.a.fp())), "{log}");
    assert!(!log.contains(&fp(&c)), "{log}");
}

/// `scripts/e2e-real.sh` run with `env`, from an empty `PATH` when `empty_path` is set, so a
/// missing harness binary is induced without touching the machine. Never gets past the
/// guards, so no real harness or `owl` process is started.
fn e2e_real(env: &[(&str, &str)], empty_path: Option<&Path>) -> Output {
    let mut c = Command::new("/bin/bash");
    c.arg(E2E_REAL)
        .env_remove("OWL_HARNESS")
        .env_remove("OWL_BIN")
        .stdin(Stdio::null());
    if let Some(dir) = empty_path {
        c.env("PATH", dir);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn e2e_real_script_fails_fast() {
    let meta = std::fs::metadata(E2E_REAL).expect("scripts/e2e-real.sh exists");
    assert!(meta.is_file());
    assert_ne!(meta.permissions().mode() & 0o111, 0, "must be executable");
    let first = std::fs::read_to_string(E2E_REAL).unwrap();
    assert!(
        first.starts_with("#!/bin/bash\n"),
        "shebang: {:?}",
        first.lines().next()
    );

    // OWL_HARNESS unset: non-zero, names the variable and the accepted values, no stdout.
    let out = e2e_real(&[], None);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
    let err = stderr(&out);
    assert!(err.contains("OWL_HARNESS is unset"), "{err}");
    assert!(err.contains("claude|codex|opencode"), "{err}");

    // OWL_HARNESS set to something outside the list: same guard, names the bad value.
    let out = e2e_real(&[("OWL_HARNESS", "gemini")], None);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("OWL_HARNESS=gemini is not one of claude|codex|opencode"),
        "{err}"
    );

    // OWL_TRANSPORT outside https|iroh: same guard class, names the bad value; it is
    // checked before the PATH lookup, so an empty PATH does not mask it.
    let empty = tempfile::tempdir().unwrap();
    let out = e2e_real(
        &[("OWL_HARNESS", "claude"), ("OWL_TRANSPORT", "tcp")],
        Some(empty.path()),
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("OWL_TRANSPORT=tcp is not one of https|iroh"),
        "{err}"
    );
    // Both accepted values get past that guard (and stop at the next one, the PATH lookup).
    for t in ["https", "iroh"] {
        let out = e2e_real(
            &[("OWL_HARNESS", "claude"), ("OWL_TRANSPORT", t)],
            Some(empty.path()),
        );
        assert_eq!(out.status.code(), Some(2), "{t}: {}", stderr(&out));
        let err = stderr(&out);
        assert!(!err.contains("is not one of https|iroh"), "{t}: {err}");
        assert!(
            err.contains("harness binary 'claude' not found on PATH"),
            "{t}: {err}"
        );
    }

    // Valid harness whose binary is not on PATH: non-zero, names the binary.
    for h in ["claude", "codex", "opencode"] {
        let out = e2e_real(&[("OWL_HARNESS", h)], Some(empty.path()));
        assert_eq!(out.status.code(), Some(2), "{h}: {}", stderr(&out));
        assert!(out.stdout.is_empty(), "{h}: stdout {:?}", out.stdout);
        let err = stderr(&out);
        assert!(
            err.contains(&format!("harness binary '{h}' not found on PATH")),
            "{h}: {err}"
        );
    }

    // Nothing under tests/ or CI wires the script in: `cargo test` reaches it only through
    // this guard test, which never passes a harness that exists.
    let ci = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/ci.yml"
    ))
    .unwrap();
    assert!(
        !ci.contains("e2e-real"),
        "CI must not run the real-harness script"
    );
}
