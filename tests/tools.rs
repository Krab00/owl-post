//! Tool-call requests: the envelope, the API allowlist table, the consent
//! hold whatever the policy says, the daemon that never spawns the tool, the
//! stdin-only input and the config refusals, the exit codes and the timeout, the
//! redaction, the stderr merge and the cut, the two-daemon exchange and the
//! thread timeline.
//!
//! A (seed 1) asks, B (seed 2) owns the registry. Every test has its own temp home, its own
//! log file and its own `127.0.0.1:0` daemon, and the tool is always
//! `tests/fixtures/fake-tool.sh` — never a real build or test command.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use common::{
    Peer, TestDaemon, assert_error, claude_home, client, fp, id, policy, post_envelope,
    prepare_home_with, signed, spawn_daemon_with, write_contact_full,
};
use owlpost::config::{Config, Tool};
use owlpost::contacts::Mode;
use owlpost::envelope::{Body, Envelope, Kind, MAX_OUTPUT_BYTES, Payload};
use owlpost::identity::Identity;
use owlpost::route::{self, Marker};
use owlpost::server::{record_state, record_state_for};
use owlpost::spool::{Dir, Record, Spool};
use owlpost::tools;
use serde_json::{Map, Value, json};
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");
const PROJECT: &str = "github.com/company/monorepo";
const TOOL: &str = "test";

// ---------------------------------------------------------------- fixtures

/// The fixture script, by absolute path: `owl draft` runs it with the tool's `cwd` as the
/// working directory, so a relative path would resolve somewhere else.
fn fixture() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-tool.sh")
        .display()
        .to_string()
}

/// A registry entry running the fixture with `knobs` (every knob is an **argument**: the
/// child's environment is cleared but for the three `OWLPOST_*` and `PATH`).
fn fake(log: &Path, knobs: &[&str]) -> Tool {
    let mut argv = vec![fixture(), format!("FAKE_TOOL_LOG={}", log.display())];
    argv.extend(knobs.iter().map(|k| (*k).to_string()));
    Tool {
        argv,
        cwd: None,
        timeout_ms: 30_000,
        max_input_bytes: 4096,
    }
}

fn input(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

/// A tool-call payload from `from` to `to` naming [`TOOL`].
fn call(from: &Identity, to: &Identity, body: Map<String, Value>) -> Payload {
    Payload::tool_call(&fp(from), &fp(to), TOOL, body, None)
}

fn signed_call(from: &Identity, to: &Identity, body: Map<String, Value>) -> Envelope {
    Envelope::sign(&call(from, to, body), from)
}

/// Daemon B with `TOOL` in its registry and `peers` in its contact book.
async fn responder(tool: Tool, peers: &[Peer<'_>]) -> TestDaemon {
    spawn_daemon_with(2, peers, move |cfg| {
        cfg.responder.tools.insert(TOOL.into(), tool);
    })
    .await
}

fn register_session(home: &Path, sid: &str, cwd: &Path) {
    route::write_marker(home, &Marker::new(sid, &cwd.to_string_lossy(), "startup")).unwrap();
}

/// The invariant: session `sid` was not woken and nothing was routed.
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
    assert_eq!(code, 0, "owl {args:?} failed ({code}): {err}");
    out
}

fn only_id(spool: &Spool, dir: Dir) -> String {
    let all = spool.list(dir, |_| true).unwrap();
    assert_eq!(all.len(), 1, "expected one record in {:?}", dir.name());
    all[0].0.clone()
}

/// The fixture's log, split into lines; `[]` when it was never written.
fn log_lines(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// One field of the fixture's `start` line (`argv`, `env`, `stdin`, `cwd`).
fn log_field(line: &str, key: &str) -> String {
    line.split('\t')
        .find_map(|f| f.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in {line}"))
        .to_string()
}

/// A responder home with a tool-call record already released to `pending`, so the draft path
/// is what is under test and not the transport.
struct Drafting {
    home: TempDir,
    id: String,
}

fn drafting(tool: Tool, body: Map<String, Value>) -> Drafting {
    drafting_with(tool, body, |_| {})
}

fn drafting_with(
    tool: Tool,
    body: Map<String, Value>,
    tweak: impl FnOnce(&mut Config),
) -> Drafting {
    let home = tempfile::tempdir().unwrap();
    let (a, b) = (id(1), id(2));
    prepare_home_with(
        home.path(),
        &b,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
        |cfg| {
            cfg.responder.tools.insert(TOOL.into(), tool);
            tweak(cfg);
        },
    );
    let payload = call(&a, &b, body);
    let env = Envelope::sign(&payload, &a);
    let spool = Spool::new(home.path()).unwrap();
    spool
        .put(Dir::Inbox, &payload.id, &common::record(&env, "pending"))
        .unwrap();
    Drafting {
        home,
        id: payload.id.clone(),
    }
}

// ---------------------------------------------------------------- the envelope

/// Both new payloads round-trip through their documented wire keys.
#[test]
fn tool_call_and_reply_round_trip_with_their_wire_keys() {
    let (a, b) = (id(1), id(2));
    let mut req = call(&a, &b, input(&[("package", json!("auth"))]));
    if let Body::ToolCall { project, .. } = &mut req.body {
        *project = Some(PROJECT.into());
    }
    let v: Value = serde_json::from_slice(&req.to_signed_bytes()).unwrap();
    assert_eq!(v["type"], "tool-call", "lowercase alone would say toolcall");
    assert_eq!(v["body"]["tool"], TOOL);
    assert_eq!(v["body"]["input"]["package"], "auth");
    assert_eq!(v["body"]["project"], PROJECT);
    let back: Payload = serde_json::from_value(v).unwrap();
    assert_eq!(back, req);
    assert!(matches!(back.body, Body::ToolCall { .. }), "body variant");

    // The twin: an absent project is omitted from the wire, not sent as null.
    let bare = call(&a, &b, input(&[]));
    let v: Value = serde_json::from_slice(&bare.to_signed_bytes()).unwrap();
    assert!(v["body"].get("project").is_none(), "absent project omitted");

    let reply = Payload::tool_reply(&req, "out\n", 3, 4120, true, 2);
    let v: Value = serde_json::from_slice(&reply.to_signed_bytes()).unwrap();
    assert_eq!(v["type"], "tool-reply");
    for (k, want) in [
        ("output", json!("out\n")),
        ("exit_code", json!(3)),
        ("duration_ms", json!(4120)),
        ("truncated", json!(true)),
        ("redactions", json!(2)),
        ("harness", json!("human")),
    ] {
        assert_eq!(v["body"][k], want, "body.{k}");
    }
    let back: Payload = serde_json::from_value(v).unwrap();
    assert_eq!(back, reply);
    assert!(matches!(back.body, Body::ToolReply { .. }));
}

/// All six bodies still parse into their own variant. `Body` is untagged and
/// `Body::Content` accepts *any* object, so a variant declared after it would be swallowed —
/// this is the test that fails when someone appends one.
#[test]
fn every_body_variant_parses_into_itself() {
    let head = |kind: &str| {
        json!({
            "v": 1, "id": "x1", "type": kind, "from": "owl:a", "to": "owl:b",
            "ts": "2026-09-01T10:00:00Z", "in_reply_to": null
        })
    };
    /// `(wire tag, body, the variant that body must land in)`.
    type Case = (&'static str, Value, fn(&Body) -> bool);
    let cases: Vec<Case> = vec![
        (
            "question",
            json!({ "project": "p", "path": "f", "question": "why?" }),
            |b| matches!(b, Body::Question { .. }),
        ),
        (
            "answer",
            json!({ "answer": "because", "harness": "claude", "redactions": 0, "cached": false }),
            |b| matches!(b, Body::Answer { .. }),
        ),
        (
            "content",
            json!({ "project": "p", "ref": "main", "path": "f" }),
            |b| matches!(b, Body::Content { .. }),
        ),
        (
            "content-reply",
            json!({ "content": "c", "sha256": "h", "truncated": false, "redactions": 0, "harness": "human" }),
            |b| matches!(b, Body::ContentReply { .. }),
        ),
        (
            "tool-call",
            json!({ "tool": "test", "input": { "package": "auth" } }),
            |b| matches!(b, Body::ToolCall { .. }),
        ),
        (
            "tool-reply",
            json!({ "output": "o", "exit_code": 0, "duration_ms": 1, "truncated": false, "redactions": 0, "harness": "human" }),
            |b| matches!(b, Body::ToolReply { .. }),
        ),
    ];
    for (kind, body, want) in cases {
        let mut v = head(kind);
        v["body"] = body;
        let p: Payload = serde_json::from_value(v).unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert_eq!(
            serde_json::to_value(p.kind).unwrap(),
            json!(kind),
            "tag of {kind}"
        );
        assert!(want(&p.body), "{kind} landed in the wrong Body variant");
    }
}

/// The reply copies the request's thread id, exactly as `Payload::answer` does.
#[test]
fn tool_reply_copies_the_context_id() {
    let (a, b) = (id(1), id(2));
    let mut req = call(&a, &b, input(&[]));
    req.context_id = Some("thread-7".into());
    let reply = Payload::tool_reply(&req, "o", 0, 5, false, 0);
    assert_eq!(reply.context_id.as_deref(), Some("thread-7"));
    assert_eq!(reply.in_reply_to.as_deref(), Some(req.id.as_str()));
    assert_eq!(reply.from, req.to);
    assert_eq!(reply.to, req.from);
    // The twin: no thread id on the request, none on the reply.
    req.context_id = None;
    assert_eq!(
        Payload::tool_reply(&req, "o", 0, 5, false, 0).context_id,
        None
    );
}

// ---------------------------------------------------------------- the 400 table

/// Every row of the §7 tool table, each with the exact message, each leaving the spool
/// untouched, no session woken and the tool never started. The last rows are the positive
/// twins: an input exactly at the cap, and a well-formed request, are accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn validation_table_answers_400_and_never_starts_the_tool() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tool.log");
    let a = id(1);
    let d = responder(
        fake(&log, &[]),
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
    )
    .await;
    let c = client(Some(&a), &d.id);
    let sid = "tool-call-400";
    register_session(d.home(), sid, logs.path());

    // `{"pad":"x…"}` is 12 + n bytes compact; the cap is 4096.
    let pad = |bytes: usize| json!("x".repeat(bytes));
    let exactly = 4096 - r#"{"pad":""}"#.len();

    let rows: Vec<(Value, &str)> = vec![
        (json!({ "tool": "nope", "input": {} }), "unknown tool"),
        (json!({ "tool": "", "input": {} }), "unknown tool"),
        (json!({ "input": {} }), "unknown tool"),
        (json!({ "tool": TOOL }), "body.input must be an object"),
        (
            json!({ "tool": TOOL, "input": "package=auth" }),
            "body.input must be an object",
        ),
        (
            json!({ "tool": TOOL, "input": ["package", "auth"] }),
            "body.input must be an object",
        ),
        (
            json!({ "tool": TOOL, "input": null }),
            "body.input must be an object",
        ),
        (
            json!({ "tool": TOOL, "input": { "pad": pad(exactly + 1) } }),
            "input is 4097 bytes, max 4096",
        ),
        (
            json!({ "tool": TOOL, "input": {}, "project": "github.com/other/repo" }),
            "unknown project",
        ),
        (
            json!({ "tool": TOOL, "input": {}, "project": 7 }),
            "unknown project",
        ),
    ];
    for (body, want) in rows {
        let p = call(&a, &d.id, input(&[]));
        let mut v = serde_json::to_value(&p).unwrap();
        v["body"] = body;
        let raw = serde_json::to_string(&v).unwrap();
        let sig = owlpost::identity::sig_string(&a.sign(raw.as_bytes()));
        let resp = post_envelope(&c, &d, &Envelope { raw, sig }).await;
        assert_quiet(d.home(), sid, want);
        assert_error(resp, 400, want).await;
        assert!(
            d.spool().list(Dir::Inbox, |_| true).unwrap().is_empty(),
            "{want}: nothing may be spooled"
        );
        assert!(!log.exists(), "{want}: the tool must never be started");
    }

    // A daemon with no registry at all answers the same coarse message, so a peer cannot
    // tell "no such tool" from "no tools configured".
    let d2 = spawn_daemon_with(
        3,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
        |_| {},
    )
    .await;
    let c2 = client(Some(&a), &d2.id);
    let resp = post_envelope(&c2, &d2, &signed_call(&a, &d2.id, input(&[]))).await;
    assert_error(resp, 400, "unknown tool").await;
    assert!(d2.spool().list(Dir::Inbox, |_| true).unwrap().is_empty());

    // Positive twin 1: exactly at the cap is accepted (the check is `>`, not `>=`).
    let at_cap = call(&a, &d.id, input(&[("pad", pad(exactly))]));
    let resp = post_envelope(&c, &d, &Envelope::sign(&at_cap, &a)).await;
    assert_eq!(resp.status().as_u16(), 202, "an input exactly at the cap");

    // Positive twin 2: a plain request is accepted and always SUBMITTED.
    let env = signed_call(&a, &d.id, input(&[("package", json!("auth"))]));
    let resp = post_envelope(&c, &d, &env).await;
    assert_eq!(resp.status().as_u16(), 202);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["state"], "TASK_STATE_SUBMITTED");
    assert_eq!(body["status"], "accepted");
    let spool = d.spool();
    assert_eq!(spool.list(Dir::Inbox, |_| true).unwrap().len(), 2);
    assert!(!log.exists(), "arrival must not start the tool");
    // The row above is only meaningful because a request that *does* pass validation wakes
    // the very session every 400 row left alone — otherwise `assert_quiet` would pass on a
    // daemon that never routes a tool call at all.
    let rid = spool
        .list(Dir::Inbox, |_| true)
        .unwrap()
        .into_iter()
        .find(|(_, r)| r.state == "consent")
        .expect("a spooled tool-call record")
        .0;
    assert!(
        route::wake_file(d.home(), sid, &rid).is_file(),
        "a valid request does wake the session"
    );
    assert_eq!(
        route::load_routing(d.home(), &rid).current.as_deref(),
        Some(sid),
        "and the routing names it"
    );
}

/// The tag and the body must agree — a `tool-call` carrying a question body is refused
/// rather than spooled as a question (`Body` is untagged).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tool_tag_with_another_body_is_refused() {
    let logs = tempfile::tempdir().unwrap();
    let a = id(1);
    let d = responder(
        fake(&logs.path().join("t.log"), &[]),
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
    )
    .await;
    let c = client(Some(&a), &d.id);
    let p = call(&a, &d.id, input(&[]));
    let mut v = serde_json::to_value(&p).unwrap();
    v["type"] = json!("answer");
    let raw = serde_json::to_string(&v).unwrap();
    let sig = owlpost::identity::sig_string(&a.sign(raw.as_bytes()));
    let resp = post_envelope(&c, &d, &Envelope { raw, sig }).await;
    assert_error(resp, 400, "type must be question, content or tool-call").await;
    assert!(d.spool().list(Dir::Inbox, |_| true).unwrap().is_empty());
}

// ---------------------------------------------------------------- consent only

/// The state table is explicit about the kind, and only about the kind.
#[test]
fn record_state_for_holds_tool_calls_in_every_mode() {
    for mode in [
        None,
        Some(Mode::Manual),
        Some(Mode::Auto),
        Some(Mode::Never),
    ] {
        assert_eq!(
            record_state_for(Kind::ToolCall, mode),
            ("consent", false),
            "tool-call with policy {mode:?}"
        );
        // The twin: a question keeps today's table byte for byte.
        assert_eq!(record_state_for(Kind::Question, mode), record_state(mode));
    }
}

/// An `auto` peer's tool call is held for consent while that same peer's question is
/// still `pending`; the scheduler refuses the record by name; a `never` peer gets 403.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auto_peer_is_held_for_a_tool_call_and_not_for_a_question() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tool.log");
    let a = id(1);
    let d = responder(
        fake(&log, &[]),
        &[Peer::new(&a, "Ana", Some(policy(Mode::Auto, None)))],
    )
    .await;
    let c = client(Some(&a), &d.id);

    assert_eq!(
        post_envelope(&c, &d, &signed_call(&a, &d.id, input(&[])))
            .await
            .status()
            .as_u16(),
        202
    );
    let spool = d.spool();
    let tid = only_id(&spool, Dir::Inbox);
    assert_eq!(
        spool.get(Dir::Inbox, &tid).unwrap().unwrap().state,
        "consent",
        "auto policy does not apply to a tool call"
    );

    // The positive twin: a question from the very same peer is `pending`.
    let q = signed(&a, &d.id, "why?");
    assert_eq!(post_envelope(&c, &d, &q).await.status().as_u16(), 202);
    let qid = spool
        .list(Dir::Inbox, |_| true)
        .unwrap()
        .into_iter()
        .map(|(i, _)| i)
        .find(|i| *i != tid)
        .expect("the question record");
    assert_eq!(
        spool.get(Dir::Inbox, &qid).unwrap().unwrap().state,
        "pending"
    );

    // The scheduler refuses the tool record by name, even once it is `pending`.
    let cfg = Config::load(d.home()).unwrap();
    spool.set_state(Dir::Inbox, &tid, "pending").unwrap();
    let out = owlpost::auto::attempt(d.home(), d.home(), &cfg, &spool, &tid).unwrap();
    assert_eq!(
        out,
        owlpost::auto::Outcome::Skipped(format!(
            "record {tid} is a tool-call request — consent only"
        ))
    );
    assert!(!log.exists(), "the scheduler must never start the tool");

    // A `never` peer is refused at the API, unchanged.
    let e = id(9);
    let d2 = responder(
        fake(&log, &[]),
        &[Peer::new(&e, "Eve", Some(policy(Mode::Never, None)))],
    )
    .await;
    let c2 = client(Some(&e), &d2.id);
    let resp = post_envelope(&c2, &d2, &signed_call(&e, &d2.id, input(&[]))).await;
    assert_eq!(resp.status().as_u16(), 403);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "unavailable");
    assert_eq!(body["state"], "TASK_STATE_REJECTED");
    assert!(d2.spool().list(Dir::Inbox, |_| true).unwrap().is_empty());
    assert!(!log.exists());
}

// ---------------------------------------------------------------- the daemon never runs it

/// With a valid request delivered, the peer allowed and the daemon left running for two
/// scheduler ticks, the log does not exist. It appears only after `owl draft` in the CLI
/// process, and then with exactly one `start` line — one run, no second one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_daemon_never_spawns_the_tool() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tool.log");
    let a = id(1);
    let tool = fake(&log, &[]);
    let d = spawn_daemon_with(
        2,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Auto, None)))],
        move |cfg| {
            cfg.responder.tools.insert(TOOL.into(), tool);
            // Two scheduler ticks must fit in the wait below.
            cfg.pull_interval_secs = 1;
        },
    )
    .await;
    let c = client(Some(&a), &d.id);
    assert_eq!(
        post_envelope(
            &c,
            &d,
            &signed_call(&a, &d.id, input(&[("package", json!("auth"))]))
        )
        .await
        .status()
        .as_u16(),
        202
    );
    let rid = only_id(&d.spool(), Dir::Inbox);
    // `--once` releases the held record without touching the policy, which is already auto.
    owl_ok(d.home(), &["allow", &fp(&a), "--once"]);
    // Two ticks of the auto-accept scan, plus slack.
    tokio::time::sleep(std::time::Duration::from_millis(2_600)).await;
    assert!(
        !log.exists(),
        "the daemon spawned the tool: {:?}",
        log_lines(&log)
    );
    let held = d
        .spool()
        .get(Dir::Inbox, &rid)
        .unwrap()
        .expect("the record is still in the inbox, never answered unattended");
    assert!(held.draft.is_none(), "nothing may be drafted unattended");

    // The twin: the human's own `owl draft` is what runs it — once.
    let home = d.home().to_path_buf();
    let out = tokio::task::spawn_blocking(move || owl_ok(&home, &["draft", &rid]))
        .await
        .unwrap();
    assert!(out.contains("ran test in"), "stdout: {out}");
    let lines = log_lines(&log);
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("start")).count(),
        1,
        "exactly one run: {lines:?}"
    );
}

// ---------------------------------------------------------------- stdin, never argv

/// The input reaches the tool on stdin as compact JSON plus `\n`, the argv the child
/// saw is exactly the configured one, and the environment holds only the three `OWLPOST_*`
/// variables (plus `PATH`) — nothing of the request, nothing of the owner's shell.
#[test]
fn the_input_goes_on_stdin_and_never_into_argv() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tool.log");
    let tool = fake(&log, &[]);
    let argv = tool.argv.clone();
    let body = input(&[("package", json!("auth")), ("filter", json!("session"))]);
    let compact = serde_json::to_string(&body).unwrap();
    let d = drafting(tool, body);
    owl_ok(d.home.path(), &["draft", &d.id]);

    let lines = log_lines(&log);
    assert_eq!(lines.len(), 2, "one start and one done: {lines:?}");
    let start = &lines[0];
    assert_eq!(
        log_field(start, "argv"),
        argv[1..].join(" "),
        "the child's argv is the configured one, with nothing of the input in it"
    );
    assert_eq!(log_field(start, "stdin"), compact, "the compact input");
    assert!(
        !log_field(start, "argv").contains("package"),
        "no part of the input may reach argv: {start}"
    );

    let env = log_field(start, "env");
    let owl_vars: Vec<String> = env
        .split('|')
        .filter(|v| v.starts_with("OWLPOST_"))
        .map(str::to_string)
        .collect();
    assert_eq!(
        owl_vars,
        [
            format!("OWLPOST_PEER={}", fp(&id(1))),
            "OWLPOST_TOOL=1".to_string(),
            format!("OWLPOST_TOOL_NAME={TOOL}"),
        ],
        "exactly the three documented variables, sorted"
    );
    assert!(
        env.contains("|PATH=") || env.starts_with("PATH="),
        "PATH is inherited: {env}"
    );
    assert!(
        !env.contains("FAKE_TOOL_"),
        "the environment is cleared, so the knobs travel in argv: {env}"
    );
    assert!(
        !env.split('|').any(|v| v.starts_with("HOME=")),
        "the owner's HOME must not reach the tool: {env}"
    );
}

/// `argv[0]` is resolved on `PATH` like a harness command, and the child is handed that
/// same `PATH` so a tool that calls another program still works. The lookup itself happens
/// before the environment is replaced, so the second half needs its own assertion: without
/// the pass-through the tool would run and see no `PATH` at all.
#[test]
fn a_bare_program_name_resolves_on_path() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tool.log");
    let mut tool = fake(&log, &[]);
    // `bash <script> <knobs>`: the program is a bare name, resolved on PATH, and the fixture
    // arrives as its first argument.
    tool.argv.insert(0, "bash".to_string());
    let d = drafting(tool, input(&[("k", json!("v"))]));
    let out = owl_ok(d.home.path(), &["draft", &d.id]);
    assert!(out.contains("exit 0"), "stdout: {out}");
    let lines = log_lines(&log);
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("start")).count(),
        1,
        "the tool ran through the PATH lookup"
    );
    let env = log_field(&lines[0], "env");
    let path = env
        .split('|')
        .find_map(|v| v.strip_prefix("PATH="))
        .expect("the child must be handed a PATH");
    assert_eq!(
        path,
        std::env::var("PATH").unwrap_or_default(),
        "the child's PATH is this process's, unchanged"
    );
}

/// The config refusals. A registry that cannot mean what it says stops every command,
/// with the tool named.
#[test]
fn the_config_refuses_an_interpolating_argv_an_empty_one_and_an_unknown_cwd() {
    let home = tempfile::tempdir().unwrap();
    let load = |tools: Value, projects: Value| {
        std::fs::write(
            Config::path(home.path()),
            serde_json::to_vec(&json!({
                "responder": { "tools": tools },
                "projects": projects,
            }))
            .unwrap(),
        )
        .unwrap();
        Config::load(home.path())
    };
    let entry = |argv: Value, cwd: Value| json!({ "test": { "argv": argv, "cwd": cwd, "timeout_ms": 1000, "max_input_bytes": 16 } });
    let err = load(
        entry(json!(["sh", "-c", "run {input}"]), json!(null)),
        json!({}),
    )
    .unwrap_err()
    .to_string();
    assert_eq!(
        err,
        "config: tools.test.argv must not interpolate the input"
    );
    let err = load(entry(json!([]), json!(null)), json!({}))
        .unwrap_err()
        .to_string();
    assert_eq!(err, "config: tools.test.argv must not be empty");
    let err = load(
        entry(json!(["true"]), json!("github.com/other/repo")),
        json!({}),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.starts_with("config: tools.test.cwd names no configured project"),
        "{err}"
    );
    // The twins: the same entry with a configured `cwd`, and with no `cwd` at all, load.
    let cfg = load(
        entry(json!(["true"]), json!(PROJECT)),
        json!({ PROJECT: "/tmp" }),
    )
    .unwrap();
    assert_eq!(cfg.responder.tools["test"].cwd.as_deref(), Some(PROJECT));
    assert!(load(entry(json!(["true"]), json!(null)), json!({})).is_ok());
    // And the default: no registry at all, so the feature is off.
    assert!(Config::default().responder.tools.is_empty());

    // The refusal stops every command, so `owl doctor` — what a user runs when nothing
    // works — must name the offending tool rather than dying silently.
    std::fs::write(
        Config::path(home.path()),
        serde_json::to_vec(&json!({
            "responder": { "tools": entry(json!(["sh", "-c", "run {input}"]), json!(null)) },
        }))
        .unwrap(),
    )
    .unwrap();
    let (_code, out, err) = owl(home.path(), &["doctor"]);
    assert!(
        format!("{out}{err}").contains("tools.test.argv must not interpolate the input"),
        "owl doctor must name the refusal:\n{out}\n{err}"
    );
}

/// `cwd` picks the checkout the tool runs in; `null` means `$OWLPOST_HOME`.
#[test]
fn the_tool_runs_in_the_configured_checkout() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tool.log");
    let checkout = tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(checkout.path()).unwrap();
    let mut tool = fake(&log, &[]);
    tool.cwd = Some(PROJECT.into());
    let checkout_path = real.display().to_string();
    let d = drafting_with(tool, input(&[]), move |cfg| {
        cfg.projects.insert(PROJECT.into(), checkout_path);
    });
    owl_ok(d.home.path(), &["draft", &d.id]);
    assert_eq!(
        log_field(&log_lines(&log)[0], "cwd"),
        real.display().to_string()
    );

    // The twin: no `cwd` runs in the owlpost home.
    let log2 = logs.path().join("home.log");
    let d2 = drafting(fake(&log2, &[]), input(&[]));
    owl_ok(d2.home.path(), &["draft", &d2.id]);
    let want = std::fs::canonicalize(d2.home.path()).unwrap();
    assert_eq!(
        log_field(&log_lines(&log2)[0], "cwd"),
        want.display().to_string()
    );
}

// ---------------------------------------------------------------- exit codes, timeout

/// A tool exiting 3 stores the draft with `exit_code: 3` and `owl draft` still exits 0 —
/// a failing build is an answer, not a CLI failure.
#[test]
fn a_failing_tool_is_still_a_draft_and_owl_draft_exits_zero() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tool.log");
    let d = drafting(fake(&log, &["FAKE_TOOL_EXIT=3"]), input(&[]));
    let (code, out, err) = owl(d.home.path(), &["draft", &d.id]);
    assert_eq!(code, 0, "a non-zero tool exit is not a CLI failure: {err}");
    assert!(out.contains("— exit 3,"), "stdout: {out}");
    let spool = Spool::new(d.home.path()).unwrap();
    let rec = spool.get(Dir::Inbox, &d.id).unwrap().unwrap();
    assert_eq!(rec.state, "drafted");
    let run = tools::stored(&rec).expect("a tool draft");
    assert_eq!(run.exit_code, 3);
    assert_eq!(run.tool, TOOL);
    assert!(!run.truncated);

    // The twin: exit 0 is stored as 0.
    let log2 = logs.path().join("ok.log");
    let d2 = drafting(fake(&log2, &[]), input(&[]));
    owl_ok(d2.home.path(), &["draft", &d2.id]);
    let rec = Spool::new(d2.home.path())
        .unwrap()
        .get(Dir::Inbox, &d2.id)
        .unwrap()
        .unwrap();
    assert_eq!(tools::stored(&rec).unwrap().exit_code, 0);
}

/// A tool sleeping past `timeout_ms` is killed — `exit_code: -1`, `duration_ms` at the
/// timeout — and `owl draft` itself returns well inside the sleep, because every wait on a
/// child is bounded. The log proves the tool started and never finished.
#[test]
fn a_tool_past_its_timeout_is_killed_and_the_draft_returns() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tool.log");
    let mut tool = fake(&log, &["FAKE_TOOL_SLEEP=30"]);
    tool.timeout_ms = 400;
    let d = drafting(tool, input(&[]));
    let started = Instant::now();
    let (code, out, err) = owl(d.home.path(), &["draft", &d.id]);
    let elapsed = started.elapsed();
    assert_eq!(code, 0, "a timeout is a result, not a CLI failure: {err}");
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "owl draft waited {elapsed:?} on a 400 ms timeout — the wait is not bounded"
    );
    assert!(out.contains("exit -1"), "stdout: {out}");
    let rec = Spool::new(d.home.path())
        .unwrap()
        .get(Dir::Inbox, &d.id)
        .unwrap()
        .unwrap();
    let run = tools::stored(&rec).expect("a tool draft");
    assert_eq!(run.exit_code, -1, "killed, so no code of its own");
    assert_eq!(run.duration_ms, 400, "the duration is the timeout");
    let lines = log_lines(&log);
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("start")).count(),
        1,
        "the tool started: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l == "done"),
        "the tool must not have finished: {lines:?}"
    );
}

/// A tool whose `argv[0]` does not exist is exit 1 with the documented line, and
/// nothing is stored — the record stays `pending` for another try.
#[test]
fn a_missing_executable_is_exit_one_and_stores_no_draft() {
    let logs = tempfile::tempdir().unwrap();
    let mut tool = fake(&logs.path().join("never.log"), &[]);
    tool.argv[0] = logs.path().join("no-such-tool").display().to_string();
    let d = drafting(tool, input(&[]));
    let (code, _out, err) = owl(d.home.path(), &["draft", &d.id]);
    assert_eq!(code, 1, "an unstartable tool is a CLI failure");
    assert!(
        err.contains(&format!("tool {TOOL}: no such file or directory")),
        "stderr: {err}"
    );
    let rec = Spool::new(d.home.path())
        .unwrap()
        .get(Dir::Inbox, &d.id)
        .unwrap()
        .unwrap();
    assert_eq!(rec.state, "pending", "nothing drafted");
    assert!(rec.draft.is_none());
}

/// The state gate holds for a tool call too: a record still in `consent` runs nothing, and
/// every flag that picks a harness or replaces the text is a usage error.
#[test]
fn a_held_record_runs_nothing_and_the_draft_flags_are_refused() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tool.log");
    let d = drafting(fake(&log, &[]), input(&[]));
    let spool = Spool::new(d.home.path()).unwrap();
    spool.set_state(Dir::Inbox, &d.id, "consent").unwrap();
    let (code, _, err) = owl(d.home.path(), &["draft", &d.id]);
    assert_eq!(code, 1);
    assert!(err.contains("held for consent"), "stderr: {err}");
    assert!(!log.exists(), "a held record must not start the tool");

    spool.set_state(Dir::Inbox, &d.id, "pending").unwrap();
    for flags in [
        vec!["--harness", "fake"],
        vec!["--text", "mine"],
        vec!["--text", "mine", "--agent"],
        vec!["--prompt"],
    ] {
        let mut args = vec!["draft", d.id.as_str()];
        args.extend(flags.iter().copied());
        let (code, _, err) = owl(d.home.path(), &args);
        assert_eq!(code, 1, "{flags:?} must be refused");
        assert!(
            err.contains(&format!(
                "record {} is a tool-call request — run owl draft {} with no flags",
                d.id, d.id
            )),
            "{flags:?}: {err}"
        );
        assert!(!log.exists(), "{flags:?} must not start the tool");
    }
    // The twin: with no flags it runs.
    owl_ok(d.home.path(), &["draft", &d.id]);
    assert_eq!(log_lines(&log).len(), 2);
}

/// `owl edit` refuses a tool draft: the draft is what the tool printed.
#[test]
fn edit_refuses_a_tool_draft() {
    let logs = tempfile::tempdir().unwrap();
    let d = drafting(fake(&logs.path().join("t.log"), &[]), input(&[]));
    owl_ok(d.home.path(), &["draft", &d.id]);
    let (code, _, err) = owl(d.home.path(), &["edit", &d.id]);
    assert_eq!(code, 1);
    // The whole sentence, not its opening: the tail is what tells the owner what to do
    // instead, so a reword of the tail is a change to the contract too.
    assert!(
        err.contains(&format!(
            "record {} is a tool-call request — its draft is what the tool printed and cannot be edited; `owl reject {}` declines it",
            d.id, d.id
        )),
        "stderr: {err}"
    );
}

// ---------------------------------------------------------------- redact, stderr, cut

/// The responder's redaction patterns run over the output, and stderr is appended under
/// its own line only when the tool wrote any.
#[test]
fn the_output_is_redacted_and_stderr_is_appended_only_when_it_exists() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tool.log");
    let d = drafting(
        fake(
            &log,
            &["FAKE_TOOL_SECRET=1", "FAKE_TOOL_STDERR=warning: slow"],
        ),
        input(&[]),
    );
    let out = owl_ok(d.home.path(), &["draft", &d.id]);
    assert!(out.contains("1 redaction"), "one, singular: {out}");
    let rec = Spool::new(d.home.path())
        .unwrap()
        .get(Dir::Inbox, &d.id)
        .unwrap()
        .unwrap();
    let run = tools::stored(&rec).unwrap();
    assert_eq!(run.redactions, 1);
    assert!(run.output.contains("[redacted]"), "output: {}", run.output);
    assert!(
        !run.output.contains("hunter2"),
        "the secret must not survive: {}",
        run.output
    );
    assert!(
        run.output.contains("--- stderr ---\nwarning: slow"),
        "stderr under its own line: {}",
        run.output
    );

    // The twin: no stderr, no marker, no redaction.
    let log2 = logs.path().join("quiet.log");
    let d2 = drafting(fake(&log2, &[]), input(&[("k", json!("v"))]));
    let out = owl_ok(d2.home.path(), &["draft", &d2.id]);
    assert!(out.contains("0 redactions"), "stdout: {out}");
    let rec = Spool::new(d2.home.path())
        .unwrap()
        .get(Dir::Inbox, &d2.id)
        .unwrap()
        .unwrap();
    let run = tools::stored(&rec).unwrap();
    assert!(
        !run.output.contains("--- stderr ---"),
        "an empty stderr adds nothing: {:?}",
        run.output
    );
    assert_eq!(run.output, "{\"k\":\"v\"}\n", "the echoed input, verbatim");
}

/// An output past `MAX_OUTPUT_BYTES` is cut at a character boundary, `truncated` goes
/// out true and the full length is kept on the draft. The fixture's payload is one ASCII
/// byte in front of two-byte characters, so the cap's offset lands inside a character and a
/// byte slice would split it.
#[test]
fn oversize_output_is_cut_on_a_character_boundary() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tool.log");
    let big = MAX_OUTPUT_BYTES + 4096;
    let d = drafting(fake(&log, &[&format!("FAKE_TOOL_BIG={big}")]), input(&[]));
    let out = owl_ok(d.home.path(), &["draft", &d.id]);
    assert!(out.contains(", truncated, "), "stdout: {out}");
    let rec = Spool::new(d.home.path())
        .unwrap()
        .get(Dir::Inbox, &d.id)
        .unwrap()
        .unwrap();
    let run = tools::stored(&rec).unwrap();
    assert!(run.truncated, "the cut bit");
    assert!(
        run.full_bytes > MAX_OUTPUT_BYTES,
        "full: {}",
        run.full_bytes
    );
    // With `FAKE_TOOL_BIG` the fixture prints one ASCII byte and then two-byte characters
    // only, so the cap's byte offset lands *inside* a character: the cut must stop one byte
    // short of it, and a byte slice would have split the `ł`.
    assert_eq!(run.output.len(), MAX_OUTPUT_BYTES - 1, "cut length");
    assert!(
        run.output.ends_with('ł'),
        "the cut ends on a whole character"
    );
    assert!(
        run.output.starts_with('a'),
        "the one-byte prefix: {:?}",
        &run.output[..1]
    );

    // The twin: a small output is not truncated and keeps its exact length.
    let log2 = logs.path().join("small.log");
    let d2 = drafting(fake(&log2, &[]), input(&[]));
    owl_ok(d2.home.path(), &["draft", &d2.id]);
    let rec = Spool::new(d2.home.path())
        .unwrap()
        .get(Dir::Inbox, &d2.id)
        .unwrap()
        .unwrap();
    let run = tools::stored(&rec).unwrap();
    assert!(!run.truncated);
    assert_eq!(run.full_bytes, run.output.len());
}

// ---------------------------------------------------------------- end to end

/// A calls, B holds for consent, shows what would run, allows, drafts, sends; A's pull
/// ingests the reply and `owl show` prints the exit code, the duration and the output. B's
/// finished record still carries the draft and the input.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_to_end_call_consent_draft_send_and_show() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tool.log");
    let a = id(1);
    let tool = fake(&log, &[]);
    let argv = tool.argv.clone();
    let b = responder(
        tool,
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
    let in_json = a_home.path().join("in.json");
    std::fs::write(&in_json, r#"{"package": "auth"}"#).unwrap();

    let out = owl_ok(
        a_home.path(),
        &[
            "call",
            "Bea",
            TOOL,
            "--input",
            &in_json.display().to_string(),
        ],
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
    assert!(!log.exists(), "nothing ran on arrival");

    // What would run, before anything runs.
    let shown = owl_ok(b.home(), &["show", &rid]);
    assert!(
        shown.contains(&format!("tool:     {TOOL}")),
        "show: {shown}"
    );
    assert!(
        shown.contains(&format!("argv:     {}", argv.join(" "))),
        "show: {shown}"
    );
    assert!(shown.contains("cwd:      "), "show: {shown}");
    assert!(
        shown.contains("\"package\": \"auth\""),
        "the input in full: {shown}"
    );
    assert!(!log.exists(), "owl show must not start the tool");

    owl_ok(b.home(), &["allow", &fp(&a), "--once"]);
    assert!(!log.exists(), "owl allow must not start the tool");
    let drafted = owl_ok(b.home(), &["draft", &rid]);
    assert!(drafted.contains("ran test in"), "draft: {drafted}");
    assert!(drafted.contains("exit 0"), "draft: {drafted}");
    assert!(
        drafted.contains(r#"{"package":"auth"}"#),
        "the output: {drafted}"
    );
    owl_ok(b.home(), &["send", &rid]);

    // The draft stays on the finished record — both sides keep what ran.
    let done = b_spool.get(Dir::Done, &rid).unwrap().unwrap();
    let kept = tools::stored(&done).expect("the draft survives the send");
    assert_eq!(kept.exit_code, 0);
    assert!(kept.output.contains("auth"));
    let payload: Value = serde_json::from_str(&done.raw).unwrap();
    assert_eq!(
        payload["body"]["input"]["package"], "auth",
        "the input the peer sent is still on the record"
    );

    // A pulls: the reply lands in `inbox/`.
    let a_path = a_home.path().to_path_buf();
    let a_clone = id(1);
    let pulled = tokio::task::spawn_blocking(move || pull_once(&a_clone, &a_path))
        .await
        .unwrap();
    let shown = owl_ok(a_home.path(), &["show", &pulled]);
    assert!(shown.contains("exit 0 · "), "show: {shown}");
    assert!(shown.contains(" bytes"), "show: {shown}");
    assert!(
        shown.contains(r#"{"package":"auth"}"#),
        "the output: {shown}"
    );
    let rendered = owl_ok(a_home.path(), &["show", &pulled, "--format", "claude"]);
    assert!(rendered.contains("exit 0 · "), "claude: {rendered}");
    let as_json = owl_ok(a_home.path(), &["--json", "show", &pulled]);
    let v: Value = serde_json::from_str(&as_json).unwrap();
    assert_eq!(v["payload"]["body"]["exit_code"], json!(0));
    assert_eq!(v["payload"]["body"]["harness"], "human");
    assert_eq!(v["type"], "tool-reply");

    // Every event kind of the timeline, written by the real code path that owns it: the
    // server on arrival, `owl call` on the asker's record, `owl draft`, `owl send` and the
    // pull. The thread tests read hand-written events; these are the write points.
    let kinds = |rec: &Record| -> Vec<String> {
        rec.meta["events"]
            .as_array()
            .expect("events")
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect()
    };
    let b_kinds = kinds(&done);
    assert!(
        b_kinds.starts_with(&["tool-requested".to_string()]),
        "the server names the arrival: {b_kinds:?}"
    );
    for want in ["tool-run", "tool-sent"] {
        assert!(
            b_kinds.iter().any(|k| k == want),
            "the responder's record lacks {want}: {b_kinds:?}"
        );
    }
    let run_ev = done.meta["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "tool-run")
        .expect("the tool-run event");
    assert_eq!(run_ev["by"], "human", "a run is always a human's doing");
    assert_eq!(run_ev["detail"]["tool"], TOOL);
    assert_eq!(run_ev["detail"]["exit_code"], json!(0));
    assert!(run_ev["detail"]["duration_ms"].is_number());

    // The ask moves to `done/` when its reply lands, so look in both.
    let ask = [Dir::Asks, Dir::Done]
        .into_iter()
        .find_map(|d| a_spool.get(d, &ask_id).ok().flatten())
        .expect("the asker's own record");
    assert_eq!(
        kinds(&ask)[0],
        "tool-requested",
        "owl call's own record names the request"
    );
    let got = a_spool.get(Dir::Inbox, &pulled).unwrap().unwrap();
    assert_eq!(kinds(&got), ["tool-received"], "the pull names the reply");

    // Exactly one run for the whole exchange.
    assert_eq!(
        log_lines(&log)
            .iter()
            .filter(|l| l.starts_with("start"))
            .count(),
        1
    );
}

/// One pull of B's outbox into A's spool through the real ingestion path.
fn pull_once(a: &Identity, a_home: &Path) -> String {
    let spool = Spool::new(a_home).unwrap();
    let book = owlpost::contacts::ContactBook::load(a_home, a_home).unwrap();
    let contact = book.resolve("Bea").unwrap().clone();
    let open = owlpost::pull::open_asks(&spool).unwrap();
    assert!(
        !open.is_empty(),
        "the asker's own tool-call ask must be open"
    );
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

/// `owl call` refuses an input that is not a JSON object before anything is sent.
#[test]
fn call_refuses_an_input_that_is_not_an_object() {
    let home = tempfile::tempdir().unwrap();
    let (a, b) = (id(1), id(2));
    prepare_home_with(home.path(), &a, &[Peer::new(&b, "Bea", None)], |_| {});
    let bad = home.path().join("bad.json");
    std::fs::write(&bad, "[1,2]").unwrap();
    let (code, _, err) = owl(
        home.path(),
        &["call", "Bea", TOOL, "--input", &bad.display().to_string()],
    );
    assert_eq!(code, 1);
    assert!(err.contains("input must be a JSON object"), "stderr: {err}");
    assert!(
        Spool::new(home.path())
            .unwrap()
            .list(Dir::Asks, |_| true)
            .unwrap()
            .is_empty(),
        "nothing spooled"
    );
}

/// The inbox row names the tool and its compact input, cut at the listing's own width.
#[test]
fn inbox_summarises_a_tool_call() {
    let logs = tempfile::tempdir().unwrap();
    let d = drafting(
        fake(&logs.path().join("t.log"), &[]),
        input(&[("package", json!("auth"))]),
    );
    let out = owl_ok(d.home.path(), &["--json", "inbox"]);
    let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(rows[0]["type"], "tool-call");
    assert_eq!(rows[0]["summary"], r#"test {"package":"auth"}"#);
}

// ---------------------------------------------------------------- the thread

/// The four event kinds on the responder's side, in `ts` order, interleaved with a
/// question and a content request of the same thread.
#[test]
fn thread_shows_the_tool_events_interleaved_with_the_other_kinds() {
    let home = tempfile::tempdir().unwrap();
    let (a, b) = (id(1), id(2));
    prepare_home_with(home.path(), &b, &[Peer::new(&a, "Ana", None)], |_| {});
    let spool = Spool::new(home.path()).unwrap();
    let ctx = "thread-1".to_string();

    let mut req = call(&a, &b, input(&[("package", json!("auth"))]));
    req.context_id = Some(ctx.clone());
    let mut rec = common::record(&Envelope::sign(&req, &a), "answered");
    rec.meta = json!({ "peer": fp(&a) });
    push_at(
        &mut rec,
        "2026-09-01T10:00:00Z",
        "tool-requested",
        None,
        None,
    );
    push_at(
        &mut rec,
        "2026-09-01T10:05:00Z",
        "tool-run",
        Some("human"),
        Some(json!({ "tool": TOOL, "exit_code": 0, "duration_ms": 4120 })),
    );
    push_at(
        &mut rec,
        "2026-09-01T10:09:00Z",
        "tool-sent",
        Some("human"),
        None,
    );
    spool.put(Dir::Done, &req.id, &rec).unwrap();

    let mut q = Payload::question(&fp(&a), &fp(&b), PROJECT, None, "why?");
    q.context_id = Some(ctx.clone());
    let mut qrec = common::record(&Envelope::sign(&q, &a), "answered");
    qrec.meta = json!({ "peer": fp(&a) });
    push_at(&mut qrec, "2026-09-01T10:02:00Z", "received", None, None);
    push_at(
        &mut qrec,
        "2026-09-01T10:07:00Z",
        "sent",
        Some("human"),
        None,
    );
    spool.put(Dir::Done, &q.id, &qrec).unwrap();

    let mut c = Payload::content(
        &fp(&a),
        &fp(&b),
        Some(PROJECT),
        None,
        Some("src/a.rs"),
        None,
    );
    c.context_id = Some(ctx.clone());
    let mut crec = common::record(&Envelope::sign(&c, &a), "answered");
    crec.meta = json!({ "peer": fp(&a) });
    push_at(
        &mut crec,
        "2026-09-01T10:03:00Z",
        "content-requested",
        None,
        None,
    );
    push_at(
        &mut crec,
        "2026-09-01T10:08:00Z",
        "content-sent",
        Some("human"),
        None,
    );
    spool.put(Dir::Done, &c.id, &crec).unwrap();

    let out = owl_ok(home.path(), &["--json", "thread", "Ana"]);
    let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
    let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "tool-requested",
            "received",
            "content-requested",
            "tool-run",
            "sent",
            "content-sent",
            "tool-sent",
        ],
        "interleaved by ts, not grouped by record"
    );
    let run = rows.iter().find(|r| r["kind"] == "tool-run").unwrap();
    assert_eq!(run["by"], "human");
    assert_eq!(run["detail"]["tool"], TOOL);
    assert_eq!(run["detail"]["exit_code"], json!(0));
    assert_eq!(run["detail"]["duration_ms"], json!(4120));
    let first = &rows[0];
    assert_eq!(first["type"], "tool-call");
    assert_eq!(first["text"], r#"test {"package":"auth"}"#);
    assert_eq!(first["dir"], "in", "the peer asked us");
    assert!(rows.iter().all(|r| r["context_id"] == ctx));
}

/// The asker's side: `tool-requested` (out) then `tool-received` (in).
#[test]
fn asker_side_carries_requested_and_received() {
    let home = tempfile::tempdir().unwrap();
    let (a, b) = (id(1), id(2));
    prepare_home_with(home.path(), &a, &[Peer::new(&b, "Bea", None)], |_| {});
    let spool = Spool::new(home.path()).unwrap();
    let mut req = call(&a, &b, input(&[]));
    req.context_id = Some("t".into());
    let mut areq = common::record(&Envelope::sign(&req, &a), "answered");
    areq.meta = json!({ "peer": fp(&b) });
    push_at(
        &mut areq,
        "2026-09-01T10:00:00Z",
        "tool-requested",
        Some("human"),
        None,
    );
    spool.put(Dir::Done, &req.id, &areq).unwrap();

    let reply = Payload::tool_reply(&req, "out\n", 0, 12, false, 0);
    let mut rrec = common::record(&Envelope::sign(&reply, &b), "pending");
    rrec.meta = json!({ "peer": fp(&b) });
    push_at(
        &mut rrec,
        "2026-09-01T10:10:00Z",
        "tool-received",
        None,
        None,
    );
    spool.put(Dir::Inbox, &reply.id, &rrec).unwrap();

    let out = owl_ok(home.path(), &["--json", "thread", "Bea"]);
    let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
    let seen: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r["kind"].as_str().unwrap(), r["dir"].as_str().unwrap()))
        .collect();
    assert_eq!(seen, [("tool-requested", "out"), ("tool-received", "in")]);
    assert_eq!(rows[1]["type"], "tool-reply");
    assert_eq!(rows[1]["text"], "out\n");
    assert_eq!(rows[1]["harness"], "human");
}

/// Appends one event with an explicit timestamp and detail.
fn push_at(rec: &mut Record, ts: &str, kind: &str, by: Option<&str>, detail: Option<Value>) {
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
    if let Some(d) = detail {
        ev["detail"] = d;
    }
    list.push(ev);
}

// ---------------------------------------------------------------- display

/// The 200-line display cap, shared with a content reply.
#[test]
fn display_caps_the_output_at_two_hundred_lines() {
    let cap = owlpost::content::CONTENT_SHOW_LINES;
    let short = (0..cap)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(tools::display(&short), short, "exactly at the cap");
    let long = format!("{short}\nline extra\nline more");
    let shown = tools::display(&long);
    assert!(shown.starts_with(&short));
    assert!(
        shown.ends_with(&format!("… 2 more lines — {} bytes", long.len())),
        "tail: {shown}"
    );
}

/// The `owl show` line on the asker's side.
#[test]
fn show_line_names_exit_duration_and_bytes() {
    assert_eq!(
        tools::Run::show_line(0, 4120, 3120),
        "exit 0 · 4.1s · 3120 bytes"
    );
    assert_eq!(
        tools::Run::show_line(-1, 400, 0),
        "exit -1 · 0.4s · 0 bytes"
    );
}

// ---------------------------------------------------------------- the human timeline

/// The kinds whose row carries our own words print it in a ```` ```text ```` block: a
/// content request's `content-drafted` and `content-sent`, and a tool call's `tool-run` and
/// `tool-sent` (an earlier `OUR_TEXT` list omitted the content pair). One row per kind —
/// dropping any one of the four from the list makes the matching count fall to one.
#[test]
fn the_human_timeline_prints_our_own_words_for_every_kind() {
    let home = tempfile::tempdir().unwrap();
    let (a, b) = (id(1), id(2));
    prepare_home_with(home.path(), &b, &[Peer::new(&a, "Ana", None)], |_| {});
    let spool = Spool::new(home.path()).unwrap();

    // A finished content exchange, with the content we served on the record.
    let mut creq = Payload::content(
        &fp(&a),
        &fp(&b),
        Some(PROJECT),
        None,
        Some("src/a.rs"),
        None,
    );
    creq.context_id = Some("t".into());
    let mut crec = common::record(&Envelope::sign(&creq, &a), "answered");
    crec.meta = json!({ "peer": fp(&a) });
    crec.draft = Some(json!({
        "text": "SERVED-CONTENT\n", "harness": "human", "redactions": 0, "status": "ok",
        "drafted_at": "2026-09-01T10:05:00Z", "sha256": "abc", "truncated": false,
        "full_bytes": 15
    }));
    push_at(
        &mut crec,
        "2026-09-01T10:05:00Z",
        "content-drafted",
        Some("human"),
        None,
    );
    push_at(
        &mut crec,
        "2026-09-01T10:09:00Z",
        "content-sent",
        Some("human"),
        None,
    );
    spool.put(Dir::Done, &creq.id, &crec).unwrap();

    // A finished tool exchange, with the output the tool printed on the record.
    let mut treq = call(&a, &b, input(&[]));
    treq.context_id = Some("t".into());
    let mut trec = common::record(&Envelope::sign(&treq, &a), "answered");
    trec.meta = json!({ "peer": fp(&a) });
    trec.draft = Some(json!({
        "text": "PRINTED-OUTPUT\n", "harness": "human", "redactions": 0, "status": "ok",
        "drafted_at": "2026-09-01T10:15:00Z", "tool": TOOL, "exit_code": 0,
        "duration_ms": 12, "truncated": false, "full_bytes": 15
    }));
    push_at(
        &mut trec,
        "2026-09-01T10:15:00Z",
        "tool-run",
        Some("human"),
        None,
    );
    push_at(
        &mut trec,
        "2026-09-01T10:19:00Z",
        "tool-sent",
        Some("human"),
        None,
    );
    spool.put(Dir::Done, &treq.id, &trec).unwrap();

    let out = owl_ok(home.path(), &["thread", "Ana"]);
    for kind in ["content-drafted", "content-sent", "tool-run", "tool-sent"] {
        assert!(
            out.contains(kind),
            "the timeline lacks the {kind} row:\n{out}"
        );
    }
    // Two rows per record, so each text is printed exactly twice — once per kind.
    assert_eq!(
        out.matches("SERVED-CONTENT").count(),
        2,
        "content-drafted and content-sent must each print the content:\n{out}"
    );
    assert_eq!(
        out.matches("PRINTED-OUTPUT").count(),
        2,
        "tool-run and tool-sent must each print the output:\n{out}"
    );
    assert!(
        out.contains("```text"),
        "our words go in a text block:\n{out}"
    );
}

/// The timeline caps a long draft at the same 200 lines `owl show` uses, instead of pasting
/// a quarter of a megabyte into the conversation.
#[test]
fn the_human_timeline_caps_a_long_tool_output() {
    let home = tempfile::tempdir().unwrap();
    let (a, b) = (id(1), id(2));
    prepare_home_with(home.path(), &b, &[Peer::new(&a, "Ana", None)], |_| {});
    let spool = Spool::new(home.path()).unwrap();
    let lines = owlpost::content::CONTENT_SHOW_LINES + 50;
    let text: String = (1..=lines).map(|n| format!("L-{n}-END\n")).collect();
    let mut treq = call(&a, &b, input(&[]));
    treq.context_id = Some("t".into());
    let mut trec = common::record(&Envelope::sign(&treq, &a), "answered");
    trec.meta = json!({ "peer": fp(&a) });
    trec.draft = Some(json!({
        "text": text, "harness": "human", "redactions": 0, "status": "ok",
        "drafted_at": "2026-09-01T10:15:00Z", "tool": TOOL, "exit_code": 0,
        "duration_ms": 12, "truncated": false, "full_bytes": text.len()
    }));
    push_at(
        &mut trec,
        "2026-09-01T10:15:00Z",
        "tool-run",
        Some("human"),
        None,
    );
    spool.put(Dir::Done, &treq.id, &trec).unwrap();

    let out = owl_ok(home.path(), &["thread", "Ana"]);
    assert!(out.contains("L-200-END"), "the last shown line:\n{out}");
    assert!(
        !out.contains("L-201-END"),
        "one line past the cap was printed"
    );
    assert!(out.contains("… 50 more lines — "), "the trailer:\n{out}");
}

// ------------------------------------------------- refusals and edge cases

/// `owl send` on a tool-call record that was never drafted refuses with the same line every
/// other kind gets, exits non-zero and queues nothing: there is no "send it anyway" path
/// that would sign an empty tool reply.
#[test]
fn send_refuses_a_tool_call_that_was_never_drafted() {
    let logs = tempfile::tempdir().unwrap();
    let d = drafting(fake(&logs.path().join("t.log"), &[]), input(&[]));
    let (code, _out, err) = owl(d.home.path(), &["send", &d.id]);
    assert_ne!(code, 0, "an undrafted record cannot be sent: {err}");
    assert!(
        err.contains(&format!(
            "record {} has no draft — run `owl draft {}` first",
            d.id, d.id
        )),
        "stderr: {err}"
    );
    let spool = Spool::new(d.home.path()).unwrap();
    assert!(
        spool.list(Dir::Outbox, |_| true).unwrap().is_empty(),
        "nothing may be queued"
    );
    let rec = spool.get(Dir::Inbox, &d.id).unwrap().unwrap();
    assert_eq!(rec.state, "pending", "the record is untouched");
    assert!(rec.draft.is_none());
}

/// The owner removed the tool between the request's arrival and their `owl draft`: the
/// lookup fails by name, exit 1, nothing is stored and no child is started. The request
/// passed the API's `unknown tool` gate when it arrived — this is the second, later gate.
#[test]
fn a_tool_removed_after_arrival_is_refused_at_draft_time() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("gone.log");
    let d = drafting_with(fake(&log, &[]), input(&[]), |cfg| {
        // The registry the request named is gone by the time the human types `owl draft`.
        cfg.responder.tools.clear();
    });
    let (code, _out, err) = owl(d.home.path(), &["draft", &d.id]);
    assert_eq!(code, 1, "a tool that is no longer registered is a failure");
    assert!(
        err.contains(&format!("unknown tool {TOOL}")),
        "the message names the tool: {err}"
    );
    assert!(!log.exists(), "no child may be started");
    let rec = Spool::new(d.home.path())
        .unwrap()
        .get(Dir::Inbox, &d.id)
        .unwrap()
        .unwrap();
    assert_eq!(rec.state, "pending", "nothing drafted");
    assert!(rec.draft.is_none());
}

/// `owl call`'s three remaining flags: `--reply-to` reuses the named exchange's thread id,
/// `--project` reaches the peer in `body.project` (and an unknown one is refused by the
/// peer's API, not locally), and `--json` prints the accepted id with the peer's state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn call_carries_the_thread_the_project_and_the_json_state() {
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("tool.log");
    let a = id(1);
    let tool = fake(&log, &[]);
    let b = spawn_daemon_with(
        2,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
        move |cfg| {
            cfg.responder.tools.insert(TOOL.into(), tool);
            // Only the key matters: the API checks membership and never touches the path.
            cfg.projects
                .insert(PROJECT.into(), "/nowhere/monorepo".into());
        },
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
    let in_json = a_home.path().join("in.json");
    std::fs::write(&in_json, r#"{"package": "auth"}"#).unwrap();
    let in_path = in_json.display().to_string();
    let a_spool = Spool::new(a_home.path()).unwrap();

    // A project the peer does not have is refused by the peer, and nothing is spooled here.
    let (code, _out, err) = owl(
        a_home.path(),
        &[
            "call",
            "Bea",
            TOOL,
            "--input",
            &in_path,
            "--project",
            "github.com/other/repo",
        ],
    );
    assert_ne!(code, 0, "an unknown project is a failure: {err}");
    assert!(err.contains("unknown project"), "stderr: {err}");
    assert!(
        a_spool.list(Dir::Asks, |_| true).unwrap().is_empty(),
        "a refused call spools nothing"
    );
    assert!(b.spool().list(Dir::Inbox, |_| true).unwrap().is_empty());

    // The configured project is accepted, and `--json` names the id and the peer's state.
    let out = owl_ok(
        a_home.path(),
        &[
            "--json",
            "call",
            "Bea",
            TOOL,
            "--input",
            &in_path,
            "--project",
            PROJECT,
        ],
    );
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["status"], "accepted");
    assert_eq!(
        v["state"], "TASK_STATE_SUBMITTED",
        "every tool call is held: {out}"
    );
    let first = v["id"].as_str().expect("the accepted id").to_string();
    assert!(
        a_spool.get(Dir::Asks, &first).unwrap().is_some(),
        "the id `--json` printed is the record's own"
    );

    // The project reached the peer on the body, not just the local record.
    let theirs = b.spool().get(Dir::Inbox, &first).unwrap().unwrap();
    let payload: Value = serde_json::from_str(&theirs.raw).unwrap();
    assert_eq!(payload["body"]["project"], PROJECT);
    assert_eq!(payload["body"]["tool"], TOOL);

    // `--reply-to` continues that exchange: same thread id, a new record.
    let out = owl_ok(
        a_home.path(),
        &[
            "--json",
            "call",
            "Bea",
            TOOL,
            "--input",
            &in_path,
            "--reply-to",
            &first,
        ],
    );
    let second = serde_json::from_str::<Value>(&out).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(second, first, "a reply-to is still a new request");
    let ctx = |rid: &str| -> String {
        let rec = a_spool.get(Dir::Asks, rid).unwrap().unwrap();
        serde_json::from_str::<Payload>(&rec.raw)
            .unwrap()
            .context_id
            .expect("a tool call is always threaded")
    };
    assert_eq!(ctx(&second), ctx(&first), "the thread id is reused");
    // The twin: without `--reply-to` the thread is a fresh one.
    let out = owl_ok(
        a_home.path(),
        &["--json", "call", "Bea", TOOL, "--input", &in_path],
    );
    let third = serde_json::from_str::<Value>(&out).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(ctx(&third), ctx(&first), "a fresh call opens a new thread");

    assert!(!log.exists(), "no arrival ever starts the tool");
}

/// The join itself: with both streams non-empty the stored output is exactly stdout,
/// the marker line, then stderr — in that order, with no blank line anywhere.
#[test]
fn stdout_and_stderr_are_joined_under_the_marker_in_order() {
    let logs = tempfile::tempdir().unwrap();
    let d = drafting(
        fake(
            &logs.path().join("both.log"),
            // Nothing here matches a default redaction pattern, so the output is verbatim.
            &["FAKE_TOOL_STDERR=heads up, this was slow"],
        ),
        input(&[("k", json!("v"))]),
    );
    owl_ok(d.home.path(), &["draft", &d.id]);
    let rec = Spool::new(d.home.path())
        .unwrap()
        .get(Dir::Inbox, &d.id)
        .unwrap()
        .unwrap();
    let run = tools::stored(&rec).unwrap();
    assert_eq!(run.redactions, 0, "nothing to redact: {}", run.output);
    assert_eq!(
        run.output, "{\"k\":\"v\"}\n--- stderr ---\nheads up, this was slow\n",
        "stdout, the marker, then stderr"
    );
}

/// The newline the join inserts: the marker always starts its own line and never sits
/// after a blank one. `echo` can only ever produce stdout that already ends in `\n`, so the
/// branch only shows itself on stdout the fixture writes with `printf '%s'` or not at all.
/// The twin with a trailing newline is
/// [`stdout_and_stderr_are_joined_under_the_marker_in_order`].
#[test]
fn the_stderr_marker_is_joined_on_exactly_one_newline() {
    let logs = tempfile::tempdir().unwrap();
    // Nothing in these knobs matches a default redaction pattern, so the output is verbatim.
    let output_of = |name: &str, knobs: &[&str]| -> String {
        let d = drafting(fake(&logs.path().join(name), knobs), input(&[]));
        owl_ok(d.home.path(), &["draft", &d.id]);
        let rec = Spool::new(d.home.path())
            .unwrap()
            .get(Dir::Inbox, &d.id)
            .unwrap()
            .unwrap();
        tools::stored(&rec).unwrap().output
    };

    // Stdout without a trailing newline: exactly one newline is inserted before the marker.
    assert_eq!(
        output_of(
            "raw.log",
            &["FAKE_TOOL_RAW=no newline here", "FAKE_TOOL_STDERR=warning"]
        ),
        "no newline here\n--- stderr ---\nwarning\n",
        "one newline inserted, never two"
    );

    // Genuinely empty stdout: the marker opens the output, with no newline in front of it.
    assert_eq!(
        output_of(
            "silent.log",
            &["FAKE_TOOL_NO_STDOUT=1", "FAKE_TOOL_STDERR=warning"]
        ),
        "--- stderr ---\nwarning\n",
        "an empty stdout adds no leading newline"
    );

    // No stderr at all: stdout stays exactly as the tool wrote it, unterminated and bare.
    assert_eq!(
        output_of("bare.log", &["FAKE_TOOL_RAW=no newline here"]),
        "no newline here",
        "no stderr, no marker and no newline of ours"
    );
}

/// The inbox summary's **width**: a tool-call row whose compact JSON is exactly
/// `SUMMARY_CHARS` long is kept whole, one character more is cut with `…`, and a row whose
/// cut lands inside a two-byte character is cut on the character — never on the byte.
#[test]
fn the_inbox_summary_is_cut_at_sixty_characters_on_a_boundary() {
    let logs = tempfile::tempdir().unwrap();
    let summary_of = |value: String| -> String {
        let d = drafting(
            fake(&logs.path().join("t.log"), &[]),
            input(&[("k", json!(value))]),
        );
        let out = owl_ok(d.home.path(), &["--json", "inbox"]);
        let rows: Vec<Value> = serde_json::from_str(&out).unwrap();
        rows[0]["summary"].as_str().unwrap().to_string()
    };
    // `test {"k":"<v>"}` is 13 + v chars, so 47 characters of value make exactly 60.
    let exact = summary_of("x".repeat(47));
    assert_eq!(exact.chars().count(), 60);
    assert_eq!(exact, format!("test {{\"k\":\"{}\"}}", "x".repeat(47)));
    assert!(!exact.ends_with('…'), "exactly at the width is kept whole");

    // One character more is cut: 60 characters and the ellipsis.
    let over = summary_of("x".repeat(48));
    assert_eq!(over, format!("test {{\"k\":\"{}\"…", "x".repeat(48)));
    assert_eq!(over.chars().count(), 61);

    // 25 `ł` then ASCII: byte offset 60 lands *inside* the 25th character, so a byte slice
    // would panic. The cut keeps 60 whole characters — 24 of the x's, none split.
    let multi = summary_of(format!("{}{}", "ł".repeat(25), "x".repeat(30)));
    assert_eq!(
        multi,
        format!("test {{\"k\":\"{}{}…", "ł".repeat(25), "x".repeat(24))
    );
    assert_eq!(multi.chars().count(), 61, "60 characters and the ellipsis");
}
