//! `owl ask` as a subprocess against in-process daemons.
//!
//! A (seed 1) asks, B (seed 2) answers. Every test has its own temp homes; every daemon
//! binds 127.0.0.1:0; a "closed port" is one a `TcpListener` bound to and released.

mod common;

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_server::Handle;
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use common::*;
use owlpost::client::{self, SendOutcome};
use owlpost::contacts::{ContactBook, Mode};
use owlpost::envelope::{self, Body, Envelope, Kind, Payload, question_hash};
use owlpost::identity::Identity;
use owlpost::spool::{Dir, Record, Spool};
use owlpost::tls;
use serde_json::{Value, json};
use tempfile::TempDir;

const QUESTION: &str = "Why is the refresh token rotated on every read?";
const ANSWER: &str = "Because the session store is append-only.";

/// `host:port` nobody listens on (bound, read, released).
fn closed_port() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().to_string()
}

/// A's home: key, config, and a contact `Bea` for `b` reachable at `endpoints`.
fn asker_home(a: &Identity, b: &Identity, endpoints: &[&str]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    prepare_home(dir.path(), a, true, &[]);
    write_contact_full(
        dir.path(),
        &Peer::new(b, "Bea", None),
        endpoints,
        &["bea@example.org"],
    );
    dir
}

fn manual() -> Option<owlpost::contacts::Policy> {
    Some(policy(Mode::Manual, None))
}

fn owl(home: &Path, cwd: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
    c.env_remove("OWLPOST_HOME")
        .arg("--home")
        .arg(home)
        .current_dir(cwd)
        .stdin(Stdio::null());
    c
}

/// `owl ask Bea <PATH> <QUESTION> --project <PROJECT> <extra…>` run from `cwd`.
fn ask_with(home: &Path, cwd: &Path, question: &str, extra: &[&str]) -> Output {
    owl(home, cwd)
        .args(["ask", "Bea", PATH, question, "--project", PROJECT])
        .args(extra)
        .output()
        .unwrap()
}

fn ask(home: &Path, extra: &[&str]) -> Output {
    ask_with(home, home, QUESTION, extra)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ids(spool: &Spool, dir: Dir) -> Vec<String> {
    spool
        .list(dir, |_| true)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

fn accepted_id(out: &Output) -> String {
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(out));
    let line = stdout(out);
    line.trim()
        .strip_prefix("accepted ")
        .unwrap_or_else(|| panic!("stdout {line:?}"))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn payload(rec: &Record) -> Payload {
    serde_json::from_str(&rec.raw).unwrap()
}

fn answer_text(p: &Payload) -> &str {
    match &p.body {
        Body::Answer { answer, .. } => answer,
        _ => panic!("not an answer: {p:?}"),
    }
}

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

/// Fresh repo with `f.txt`: one commit per `(email, lines)` appending that many lines.
fn fixture_repo(authors: &[(&str, usize)]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    let file = repo.join("f.txt");
    let mut content = String::new();
    for (i, (email, n)) in authors.iter().enumerate() {
        for k in 0..*n {
            content.push_str(&format!("line {i}-{k} by {email}\n"));
        }
        std::fs::write(&file, &content).unwrap();
        git(repo, &["add", "f.txt"]);
        git(
            repo,
            &[
                "-c",
                &format!("user.email={email}"),
                "-c",
                "user.name=Someone",
                "commit",
                "-q",
                "-m",
                &format!("commit {i}"),
            ],
        );
    }
    dir
}

/// Ask from A's home, where the `--project` flag is left out so detection runs in `cwd`.
fn ask_no_project(home: &Path, cwd: &Path, question: &str) -> Output {
    owl(home, cwd)
        .args(["ask", "Bea", PATH, question])
        .output()
        .unwrap()
}

// ---- scripted fake peer ------------------------------------------------------------------
// Speaks B's side of §7 over B's pinned mTLS, but every response is scripted: what the real
// daemon never sends (a malformed `202`, a failing outbox, a refused ack) has to come from here.

#[derive(Default)]
struct FakeScript {
    /// Verbatim `(status, body)` for `POST /v1/questions`; `None` = a well-formed `202`.
    accept: Option<(u16, String)>,
    /// Extra response headers sent with the scripted `accept` row (`Retry-After`,
    /// `X-Owl-Signature`); the daemon never omits either, so their absence is scripted here.
    accept_headers: Vec<(String, String)>,
    /// `GET /v1/outbox` answers `500` until this instant.
    outbox_fails_until: Option<Instant>,
    /// Status of `POST /v1/outbox/{id}/ack` (`204` is what the daemon sends).
    ack_status: u16,
    /// When set, every accepted question is answered with this text straight into the outbox.
    answer_with: Option<String>,
    /// `"state"` of the well-formed `202` body; `None` omits the key entirely.
    accept_state: Option<&'static str>,
    /// Successive `(status, body)` for `GET /v1/questions/{id}`; the last row
    /// repeats once the script runs out, and an EMPTY script is a peer with no task route
    /// (every fetch is a `404`). `id` / `contextId` are filled in from the live question.
    tasks: Vec<(u16, Value)>,
    /// With `answer_with`, hold the answer back until this many task fetches have been
    /// served (`0` = straight into the outbox at accept time). Lets a test script the state
    /// transitions that must be printed BEFORE the answer ends the wait.
    answer_after_tasks: usize,
    /// Answer every question inline (`200`, signed by this peer) with an answer
    /// whose id is this function of the question's id and whose `in_reply_to` is an
    /// unrelated id — so the `in_reply_to` rule cannot be what refuses it.
    inline_answer_id: Option<fn(&str) -> String>,
}

struct FakeState {
    id: Identity,
    script: FakeScript,
    questions: Mutex<Vec<Payload>>,
    outbox: Mutex<Vec<(Payload, Envelope)>>,
    acked: Mutex<Vec<String>>,
    /// The question id of every `GET /v1/questions/{id}` this peer served.
    task_fetches: Mutex<Vec<String>>,
    /// The asker's home, once known; each question that arrives records whether
    /// its id already named a record there (`Spool::same_id`) — what the asker's server
    /// would see if this peer posted a question back under that id right now.
    asker_home: Mutex<Option<PathBuf>>,
    taken_on_arrival: Mutex<Vec<bool>>,
}

struct FakePeer {
    addr: SocketAddr,
    state: Arc<FakeState>,
    handle: Handle<SocketAddr>,
}

impl FakePeer {
    fn questions(&self) -> Vec<Payload> {
        self.state.questions.lock().unwrap().clone()
    }
    fn answers(&self) -> Vec<Payload> {
        self.state
            .outbox
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect()
    }
    fn acked(&self) -> Vec<String> {
        self.state.acked.lock().unwrap().clone()
    }
    /// The ids this peer served a Task for, in order (counted server-side so no
    /// test has to infer the client's polling from timing).
    fn task_fetches(&self) -> Vec<String> {
        self.state.task_fetches.lock().unwrap().clone()
    }
    fn shutdown(&self) {
        self.handle.shutdown();
    }
}

async fn fake_questions(
    State(st): State<Arc<FakeState>>,
    body: String,
) -> (StatusCode, HeaderMap, String) {
    let q: Payload = serde_json::from_str(&body).expect("client sends a JSON payload");
    st.questions.lock().unwrap().push(q.clone());
    let asker_home = st.asker_home.lock().unwrap().clone();
    if let Some(home) = asker_home {
        let taken = !Spool::new(&home)
            .unwrap()
            .same_id(&q.id)
            .unwrap()
            .is_empty();
        st.taken_on_arrival.lock().unwrap().push(taken);
    }
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    if let Some(make_id) = st.script.inline_answer_id {
        let mut ans = Payload::answer(&q, ANSWER, "fake", 0, false);
        ans.id = make_id(&q.id);
        ans.in_reply_to = Some(uuid::Uuid::now_v7().to_string());
        let env = Envelope::sign(&ans, &st.id);
        headers.insert("x-owl-signature", env.sig.parse().unwrap());
        return (StatusCode::OK, headers, env.raw);
    }
    if let Some((status, body)) = &st.script.accept {
        for (k, v) in &st.script.accept_headers {
            headers.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        return (
            StatusCode::from_u16(*status).unwrap(),
            headers,
            body.clone(),
        );
    }
    if let Some(text) = &st.script.answer_with
        && st.script.answer_after_tasks == 0
    {
        let ans = Payload::answer(&q, text, "fake", 0, false);
        let env = Envelope::sign(&ans, &st.id);
        st.outbox.lock().unwrap().push((ans, env));
    }
    let mut body = json!({ "status": "accepted", "id": q.id });
    if let Some(state) = st.script.accept_state {
        body["state"] = json!(state);
    }
    (StatusCode::ACCEPTED, headers, body.to_string())
}

/// `GET /v1/questions/{id}`: the next scripted Task, with `id` / `contextId`
/// filled in from the live question. An empty script is a peer without the route: `404`.
async fn fake_task(
    State(st): State<Arc<FakeState>>,
    AxPath(id): AxPath<String>,
) -> (StatusCode, HeaderMap, String) {
    let served = {
        let mut fetches = st.task_fetches.lock().unwrap();
        fetches.push(id.clone());
        fetches.len()
    };
    let question = st.questions.lock().unwrap().last().cloned();
    // The answer is released only once the scripted transitions have been served.
    if let Some(text) = &st.script.answer_with
        && st.script.answer_after_tasks > 0
        && served >= st.script.answer_after_tasks
        && let Some(q) = &question
    {
        let mut outbox = st.outbox.lock().unwrap();
        if outbox.is_empty() {
            let ans = Payload::answer(q, text, "fake", 0, false);
            let env = Envelope::sign(&ans, &st.id);
            outbox.push((ans, env));
        }
    }
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    let Some((status, body)) = st
        .script
        .tasks
        .get(served - 1)
        .or_else(|| st.script.tasks.last())
    else {
        return (
            StatusCode::NOT_FOUND,
            headers,
            json!({ "error": "unknown question" }).to_string(),
        );
    };
    let mut body = body.clone();
    body["status"]["message"]["messageId"] = json!(format!("{id}-status"));
    body["id"] = json!(id);
    body["contextId"] = json!(question.and_then(|q| q.context_id));
    (
        StatusCode::from_u16(*status).unwrap(),
        headers,
        body.to_string(),
    )
}

/// One A2A Task body in the shape the real server produces; `id` / `contextId`
/// are placeholders that [`fake_task`] replaces with the live question's.
fn task_body(state: &str, text: &str) -> (u16, Value) {
    (
        200,
        json!({
            "id": "",
            "contextId": Value::Null,
            "status": {
                "state": state,
                "timestamp": "2026-09-12T10:00:00Z",
                "message": {
                    "messageId": "status",
                    "role": "ROLE_AGENT",
                    "parts": [{ "text": text }],
                },
            },
            "metadata": { "owlpost": {
                "from": "fake", "to": "asker", "project": PROJECT, "path": PATH,
            } },
        }),
    )
}

async fn fake_outbox(State(st): State<Arc<FakeState>>) -> (StatusCode, Json<Value>) {
    if st
        .script
        .outbox_fails_until
        .is_some_and(|until| Instant::now() < until)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "still starting" })),
        );
    }
    let acked = st.acked.lock().unwrap();
    let items: Vec<Envelope> = st
        .outbox
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _)| !acked.contains(&p.id))
        .map(|(_, e)| e.clone())
        .collect();
    (StatusCode::OK, Json(serde_json::to_value(items).unwrap()))
}

async fn fake_ack(
    State(st): State<Arc<FakeState>>,
    AxPath(id): AxPath<String>,
) -> (StatusCode, Json<Value>) {
    let status = StatusCode::from_u16(st.script.ack_status).unwrap();
    if status == StatusCode::NO_CONTENT {
        st.acked.lock().unwrap().push(id);
        return (status, Json(Value::Null));
    }
    (status, Json(json!({ "error": "ack refused by script" })))
}

/// A fake peer with B's identity (`seed`) that admits only `asker`'s key.
async fn spawn_fake(seed: u8, asker: &Identity, script: FakeScript) -> FakePeer {
    let id = id(seed);
    let allowed = [key(asker)].into_iter().collect();
    let cfg = tls::server_config(&id, allowed, false).unwrap();
    let state = Arc::new(FakeState {
        id,
        script,
        questions: Mutex::new(vec![]),
        outbox: Mutex::new(vec![]),
        acked: Mutex::new(vec![]),
        task_fetches: Mutex::new(vec![]),
        asker_home: Mutex::new(None),
        taken_on_arrival: Mutex::new(vec![]),
    });
    let app = Router::new()
        .route("/v1/questions", post(fake_questions))
        .route("/v1/questions/{id}", get(fake_task))
        .route("/v1/outbox", get(fake_outbox))
        .route("/v1/outbox/{id}/ack", post(fake_ack))
        .with_state(state.clone());
    let handle = Handle::new();
    let server = axum_server::bind("127.0.0.1:0".parse().unwrap())
        .acceptor(RustlsAcceptor::new(RustlsConfig::from_config(cfg)))
        .handle(handle.clone());
    tokio::spawn(server.serve(app.into_make_service()));
    let addr = handle.listening().await.expect("fake peer bound");
    FakePeer {
        addr,
        state,
        handle,
    }
}

fn well_formed() -> FakeScript {
    FakeScript {
        ack_status: 204,
        answer_with: Some(ANSWER.into()),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ask_accepted_writes_ask_record() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let out = ask(home.path(), &[]);
    let qid = accepted_id(&out);
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));

    let spool = Spool::new(home.path()).unwrap();
    let rec = spool.get(Dir::Asks, &qid).unwrap().expect("asks/<id>.json");
    assert_eq!(rec.state, "waiting");
    assert!(!rec.seen);
    assert_eq!(rec.meta["peer"], fp(&b.id));
    assert_eq!(
        rec.meta["hash"],
        question_hash(PROJECT, Some(PATH), QUESTION)
    );
    // The stored raw/sig is the exact signed question, verifiable with A's key.
    let q = Envelope {
        raw: rec.raw.clone(),
        sig: rec.sig.clone(),
    }
    .verify(&a.verifying_key())
    .unwrap();
    assert_eq!(q.id, qid);
    assert_eq!(q.kind, Kind::Question);
    assert_eq!(q.from, fp(&a));
    assert_eq!(q.to, fp(&b.id));
    assert_eq!(
        q.body,
        Body::Question {
            project: PROJECT.into(),
            path: Some(PATH.into()),
            question: QUESTION.into(),
            context: None,
        }
    );
    assert_eq!(ids(&spool, Dir::Asks), std::slice::from_ref(&qid));
    assert!(ids(&spool, Dir::Inbox).is_empty());
    assert!(ids(&spool, Dir::Cache).is_empty());
    assert!(ids(&spool, Dir::Done).is_empty());
    // B spooled the very same question.
    let b_rec = b
        .spool()
        .get(Dir::Inbox, &qid)
        .unwrap()
        .expect("in B's inbox");
    assert_eq!(b_rec.state, "pending");
    assert_eq!(b_rec.raw, rec.raw);

    // --json: machine shape, and a second ask (fresh id) lands next to the first.
    let out = ask(home.path(), &["--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v.get("status").and_then(Value::as_str), Some("accepted"));
    let qid2 = v.get("id").and_then(Value::as_str).unwrap().to_string();
    assert_ne!(qid2, qid);
    assert_eq!(ids(&spool, Dir::Asks).len(), 2);
    b.running.shutdown();
}

/// `owl ask <peer> "<question>"` with no path is accepted, stored without a
/// path (key omitted on the wire, hash over the empty path), and the responder's `owl show`,
/// `owl inbox` print `-` where the path would be.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ask_without_path() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let out = owl(home.path(), home.path())
        .args(["ask", "Bea", QUESTION, "--project", PROJECT])
        .output()
        .unwrap();
    let qid = accepted_id(&out);
    assert!(stderr(&out).is_empty(), "stderr: {}", stderr(&out));

    let spool = Spool::new(home.path()).unwrap();
    let rec = spool.get(Dir::Asks, &qid).unwrap().expect("asks/<id>.json");
    assert_eq!(rec.state, "waiting");
    assert_eq!(rec.meta["hash"], question_hash(PROJECT, None, QUESTION));
    assert_ne!(
        rec.meta["hash"],
        question_hash(PROJECT, Some(PATH), QUESTION),
        "the repo-level question is a different cache key"
    );
    assert_eq!(
        payload(&rec).body,
        Body::Question {
            project: PROJECT.into(),
            path: None,
            question: QUESTION.into(),
            context: None,
        }
    );
    assert!(
        !rec.raw.contains("\"path\""),
        "omitted on the wire: {}",
        rec.raw
    );

    // B spooled the very same bytes and prints `-` for the path.
    let b_rec = b
        .spool()
        .get(Dir::Inbox, &qid)
        .unwrap()
        .expect("in B's inbox");
    assert_eq!(b_rec.raw, rec.raw);
    let show = owl(b.home(), b.home())
        .args(["show", &qid])
        .output()
        .unwrap();
    assert_eq!(show.status.code(), Some(0), "stderr: {}", stderr(&show));
    let shown = stdout(&show);
    assert!(shown.contains("\npath:     -\n"), "{shown}");
    assert!(
        shown.contains(&format!("\nproject:  {PROJECT}\n")),
        "{shown}"
    );
    let shown: Value = serde_json::from_str(&stdout(
        &owl(b.home(), b.home())
            .args(["show", &qid, "--json"])
            .output()
            .unwrap(),
    ))
    .unwrap();
    assert_eq!(shown["path"], "-");
    assert_eq!(shown["payload"]["body"].get("path"), None);
    let inbox = owl(b.home(), b.home())
        .args(["inbox", "--json"])
        .output()
        .unwrap();
    assert_eq!(inbox.status.code(), Some(0), "stderr: {}", stderr(&inbox));
    let rows: Value = serde_json::from_str(&stdout(&inbox)).unwrap();
    assert_eq!(rows[0]["id"], qid.as_str());
    assert_eq!(rows[0]["path"], "-");

    // Negative twin: the question itself is still required, nothing is sent.
    let out = owl(home.path(), home.path())
        .args(["ask", "Bea", "--project", PROJECT])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("missing <question>"),
        "{}",
        stderr(&out)
    );
    assert!(
        !stderr(&out).contains("missing <path>"),
        "the path is not required any more: {}",
        stderr(&out)
    );
    assert_eq!(ids(&spool, Dir::Asks), std::slice::from_ref(&qid));
    // Positive twin: with a path the question still carries it.
    let out = owl(home.path(), home.path())
        .args(["ask", "Bea", PATH, "Why that file?", "--project", PROJECT])
        .output()
        .unwrap();
    let qid2 = accepted_id(&out);
    let rec2 = spool.get(Dir::Asks, &qid2).unwrap().unwrap();
    assert_eq!(
        payload(&rec2).body,
        Body::Question {
            project: PROJECT.into(),
            path: Some(PATH.into()),
            question: "Why that file?".into(),
            context: None,
        }
    );
    assert_eq!(
        rec2.meta["hash"],
        question_hash(PROJECT, Some(PATH), "Why that file?")
    );
    let show = owl(b.home(), b.home())
        .args(["show", &qid2])
        .output()
        .unwrap();
    assert!(
        stdout(&show).contains(&format!("\npath:     {PATH}\n")),
        "{}",
        stdout(&show)
    );
    b.running.shutdown();
}

/// Sends `QUESTION` to a fake peer whose `POST /v1/questions` reply is scripted verbatim.
async fn ask_scripted_202(
    a: &Identity,
    status: u16,
    body: &str,
) -> (Output, FakePeer, Spool, TempDir) {
    let peer = spawn_fake(
        2,
        a,
        FakeScript {
            accept: Some((status, body.to_string())),
            ack_status: 204,
            ..Default::default()
        },
    )
    .await;
    let home = asker_home(a, &peer.state.id, &[&peer.addr.to_string()]);
    let out = ask(home.path(), &[]);
    let spool = Spool::new(home.path()).unwrap();
    (out, peer, spool, home)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_202_with_a_bad_body_is_a_clean_error() {
    let a = id(1);
    // Well-formed twin through the same fake: the id echoes the question id, ask recorded.
    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            ack_status: 204,
            ..Default::default()
        },
    )
    .await;
    let home = asker_home(&a, &peer.state.id, &[&peer.addr.to_string()]);
    let qid = accepted_id(&ask(home.path(), &[]));
    assert_eq!(peer.questions()[0].id, qid);
    assert_eq!(ids(&Spool::new(home.path()).unwrap(), Dir::Asks), [qid]);
    peer.shutdown();

    // Every malformed 202 body: exit 1 (never a panic), the reason named, nothing recorded.
    for (body, reason) in [
        ("[]", "202 body has no id"),
        ("\"accepted\"", "202 body has no id"),
        ("null", "202 body has no id"),
        (r#"{"status":"accepted"}"#, "202 body has no id"),
        (r#"{"status":"accepted","id":7}"#, "202 body has no id"),
        ("accepted", "202 body is not JSON"),
        ("", "202 body is not JSON"),
    ] {
        let (out, peer, spool, _home) = ask_scripted_202(&a, 202, body).await;
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(1), "body {body:?}: {err}");
        assert!(err.contains(reason), "body {body:?}: {err}");
        assert!(!err.contains("panicked"), "body {body:?}: {err}");
        assert!(stdout(&out).is_empty(), "body {body:?}");
        assert_eq!(peer.questions().len(), 1, "the question was sent");
        for dir in Dir::ALL {
            assert!(
                ids(&spool, dir).is_empty(),
                "body {body:?}: {} must stay empty",
                dir.name()
            );
        }
        peer.shutdown();
    }

    // A 202 that accepts some OTHER id is refused: the asks/ record would never be answered.
    let (out, peer, spool, _home) =
        ask_scripted_202(&a, 202, r#"{"status":"accepted","id":"someone-elses-id"}"#).await;
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    let sent = &peer.questions()[0].id;
    assert!(
        err.contains(&format!(
            "peer accepted id someone-elses-id but the question id is {sent}"
        )),
        "{err}"
    );
    assert!(stdout(&out).is_empty());
    assert!(
        ids(&spool, Dir::Asks).is_empty(),
        "no asks/ record for a mismatched id"
    );
    peer.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cache_hit_skips_network() {
    let (a, b) = (id(1), id(2));
    let home = asker_home(&a, &b, &[&closed_port()]);
    let spool = Spool::new(home.path()).unwrap();
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    let q = Payload::question(&fp(&a), &fp(&b), PROJECT, Some(PATH), QUESTION);
    let env = Envelope::sign(&Payload::answer(&q, "Cached answer.", "fake", 0, true), &b);
    spool.cache_put(&hash, &record(&env, "pending")).unwrap();

    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "Cached answer.");
    assert!(
        ids(&spool, Dir::Asks).is_empty(),
        "nothing sent, nothing queued"
    );
    assert!(ids(&spool, Dir::Inbox).is_empty());

    // The hash normalises whitespace and case, so this is the same entry.
    let out = ask_with(
        home.path(),
        home.path(),
        "  WHY is   the refresh token rotated on every READ? ",
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "Cached answer.");

    // --json prints the cached payload itself.
    let out = ask(home.path(), &["--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v.get("type").and_then(Value::as_str), Some("answer"));
    assert_eq!(
        v.get("body")
            .and_then(|b| b.get("answer"))
            .and_then(Value::as_str),
        Some("Cached answer.")
    );

    // A different question misses the cache and reaches the (dead) endpoint.
    let out = ask_with(home.path(), home.path(), "Something else?", &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("offline"), "{}", stderr(&out));

    // --no-cache bypasses the populated entry: the send is attempted and fails offline.
    let out = ask(home.path(), &["--no-cache"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("offline"), "{}", stderr(&out));
    assert!(ids(&spool, Dir::Asks).is_empty());
}

/// A cache entry that is not a verified answer is a miss: the send is attempted (here: to a
/// closed port, so exit 2 offline), and the corrupt entry is warned about, never printed.
#[test]
fn corrupt_cache_entries_fall_through() {
    let (a, b) = (id(1), id(2));
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    let q = Payload::question(&fp(&a), &fp(&b), PROJECT, Some(PATH), QUESTION);
    let question_env = Envelope::sign(&q, &a);
    let answer_env = Envelope::sign(&Payload::answer(&q, "Cached answer.", "fake", 0, true), &b);
    let mut junk_raw = record(&answer_env, "pending");
    junk_raw.raw = "{not json".into();
    let mut object_not_payload = record(&answer_env, "pending");
    object_not_payload.raw = r#"{"status":"pending"}"#.into();
    for (what, rec) in [
        // Positive twin's exact shape, but kind `question`: the one dimension that differs.
        ("a question payload", record(&question_env, "pending")),
        ("unparseable raw", junk_raw),
        ("an object that is not a payload", object_not_payload),
    ] {
        let home = asker_home(&a, &b, &[&closed_port()]);
        let spool = Spool::new(home.path()).unwrap();
        spool.cache_put(&hash, &rec).unwrap();
        let out = ask(home.path(), &[]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{what}: {err}");
        assert!(
            err.contains("ignoring corrupt cache entry"),
            "{what}: {err}"
        );
        assert!(err.contains("offline"), "{what}: {err}");
        assert!(
            stdout(&out).is_empty(),
            "{what}: nothing is printed from the cache"
        );
        assert!(!err.contains("panicked"), "{what}: {err}");
        assert!(ids(&spool, Dir::Asks).is_empty(), "{what}");
        // --quiet drops the warning, not the miss.
        let out = ask(home.path(), &["--quiet"]);
        assert_eq!(out.status.code(), Some(2), "{what}: {}", stderr(&out));
        assert!(
            !stderr(&out).contains("ignoring"),
            "{what}: {}",
            stderr(&out)
        );
    }
    // The twin with kind `answer` and the same closed port is served from the cache.
    let home = asker_home(&a, &b, &[&closed_port()]);
    let spool = Spool::new(home.path()).unwrap();
    spool
        .cache_put(&hash, &record(&answer_env, "pending"))
        .unwrap();
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "Cached answer.");
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
}

/// A cache record that cannot be read at all — not a record with bad contents, but a file the
/// spool fails to load (a directory in its place, or bytes that are not a record) — is also a
/// miss: warned as `unreadable`, then the send is attempted (closed port → exit 2 offline).
/// Twin of `corrupt_cache_entries_fall_through`, which reaches the loadable-but-wrong arm.
#[test]
fn unreadable_cache_entries_fall_through() {
    let (a, b) = (id(1), id(2));
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    for what in ["a directory", "bytes that are not a record"] {
        let home = asker_home(&a, &b, &[&closed_port()]);
        let spool = Spool::new(home.path()).unwrap();
        let path = spool.path(Dir::Cache, &hash);
        if what == "a directory" {
            std::fs::create_dir(&path).unwrap();
        } else {
            std::fs::write(&path, b"{\"raw\": \"tru").unwrap();
        }
        assert!(
            spool.cache_get(&hash).is_err(),
            "{what}: the spool itself fails to read the entry"
        );
        let out = ask(home.path(), &[]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(2), "{what}: {err}");
        assert!(
            err.contains("ignoring unreadable cache entry"),
            "{what}: {err}"
        );
        assert!(!err.contains("corrupt"), "{what}: the other arm: {err}");
        assert!(err.contains("offline"), "{what}: {err}");
        assert!(
            stdout(&out).is_empty(),
            "{what}: nothing printed from the cache"
        );
        assert!(!err.contains("panicked"), "{what}: {err}");
        assert!(ids(&spool, Dir::Asks).is_empty(), "{what}");
        // --quiet drops the warning, not the miss.
        let out = ask(home.path(), &["--quiet"]);
        assert_eq!(out.status.code(), Some(2), "{what}: {}", stderr(&out));
        assert!(
            !stderr(&out).contains("ignoring"),
            "{what}: {}",
            stderr(&out)
        );
        assert!(ids(&spool, Dir::Asks).is_empty(), "{what}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_cache_sends_to_a_live_peer_despite_a_hit() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let spool = Spool::new(home.path()).unwrap();
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    let q = Payload::question(&fp(&a), &fp(&b.id), PROJECT, Some(PATH), QUESTION);
    let env = Envelope::sign(
        &Payload::answer(&q, "Cached answer.", "fake", 0, true),
        &b.id,
    );
    spool.cache_put(&hash, &record(&env, "pending")).unwrap();

    let out = ask(home.path(), &["--no-cache"]);
    let qid = accepted_id(&out);
    assert!(b.spool().get(Dir::Inbox, &qid).unwrap().is_some());
    assert_eq!(ids(&spool, Dir::Asks), [qid]);
    // The cache entry itself is untouched.
    assert_eq!(spool.cache_get(&hash).unwrap().unwrap().raw, env.raw);
    b.running.shutdown();
}

#[test]
fn offline_exits_2_and_queues_nothing() {
    let (a, b) = (id(1), id(2));
    let port = closed_port();
    let home = asker_home(&a, &b, &[&port]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("offline"), "{err}");
    assert!(err.contains(&port), "names the endpoint: {err}");
    assert!(stdout(&out).is_empty());
    let spool = Spool::new(home.path()).unwrap();
    for dir in Dir::ALL {
        assert!(
            ids(&spool, dir).is_empty(),
            "{} must stay empty",
            dir.name()
        );
    }

    // A contact with no endpoints at all is valid (iroh reaches it); with no
    // local daemon to forward over iroh it is offline (2), naming the missing daemon.
    let home = asker_home(&a, &b, &[]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        "owl: offline: no endpoint of Bea reachable (iroh: no local daemon (daemon.addr missing))"
    );
    assert!(stdout(&out).is_empty());

    // Unknown peer: resolution fails before anything is sent (1).
    let out = owl(home.path(), home.path())
        .args(["ask", "Zed", PATH, QUESTION, "--project", PROJECT])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("no contact matches"),
        "{}",
        stderr(&out)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn endpoints_are_tried_in_order() {
    let a = id(1);
    // Two daemons with B's identity, so either one is a valid pinned peer: the question
    // must land in the FIRST reachable one.
    let first = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let second = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let dead = closed_port();
    let (f, s) = (first.addr.to_string(), second.addr.to_string());

    let home = asker_home(&a, &first.id, &[&dead, &f, &s]);
    let qid = accepted_id(&ask(home.path(), &[]));
    assert!(first.spool().get(Dir::Inbox, &qid).unwrap().is_some());
    assert!(second.spool().get(Dir::Inbox, &qid).unwrap().is_none());
    assert_eq!(ids(&Spool::new(home.path()).unwrap(), Dir::Asks), [qid]);

    let home = asker_home(&a, &first.id, &[&dead, &s, &f]);
    let qid = accepted_id(&ask(home.path(), &[]));
    assert!(second.spool().get(Dir::Inbox, &qid).unwrap().is_some());
    assert!(first.spool().get(Dir::Inbox, &qid).unwrap().is_none());

    // Two dead endpoints ahead of the live one still get there.
    let home = asker_home(&a, &first.id, &[&dead, &closed_port(), &f]);
    let qid = accepted_id(&ask(home.path(), &[]));
    assert!(first.spool().get(Dir::Inbox, &qid).unwrap().is_some());
    first.running.shutdown();
    second.running.shutdown();
}

/// Waits for the first inbox record on B, then after `delay` writes B's signed answer to
/// its outbox (state `unacked`) — what `owl send` / the auto-accept scheduler will do.
fn answer_later(
    b_home: PathBuf,
    b_id: Identity,
    delay: Duration,
) -> std::thread::JoinHandle<(String, String)> {
    std::thread::spawn(move || {
        let spool = Spool::new(&b_home).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let (qid, rec) = loop {
            if let Some(x) = spool.list(Dir::Inbox, |_| true).unwrap().into_iter().next() {
                break x;
            }
            assert!(Instant::now() < deadline, "no question reached B");
            std::thread::sleep(Duration::from_millis(20));
        };
        std::thread::sleep(delay);
        let q = payload(&rec);
        let ans = Payload::answer(&q, ANSWER, "fake", 0, false);
        let env = Envelope::sign(&ans, &b_id);
        spool
            .put(Dir::Outbox, &ans.id, &record(&env, "unacked"))
            .unwrap();
        (qid, ans.id)
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_returns_answer() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let answerer = answer_later(
        b.home().to_path_buf(),
        Identity::from_seed([2; 32]),
        Duration::from_secs(1),
    );
    let started = Instant::now();
    let out = ask(home.path(), &["--wait", "10"]);
    let (qid, aid) = answerer.join().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), ANSWER);
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "returned as soon as the answer appeared, not at the deadline"
    );
    assert!(
        stderr(&out).contains(&format!("accepted {qid}")),
        "{}",
        stderr(&out)
    );
    assert!(
        !stderr(&out).contains("warning"),
        "a 204 ack and a clean poll must not warn: {}",
        stderr(&out)
    );

    let spool = Spool::new(home.path()).unwrap();
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    let inbox = spool
        .get(Dir::Inbox, &aid)
        .unwrap()
        .expect("answer in A's inbox");
    assert_eq!(inbox.state, "pending");
    assert!(!inbox.seen);
    let p = payload(&inbox);
    assert_eq!(p.kind, Kind::Answer);
    assert_eq!(p.in_reply_to.as_deref(), Some(qid.as_str()));
    assert_eq!(answer_text(&p), ANSWER);
    assert_eq!(inbox.meta["peer"], fp(&b.id));
    assert_eq!(inbox.meta["in_reply_to"], qid);
    let cached = spool.cache_get(&hash).unwrap().expect("answer cached");
    assert_eq!(cached.raw, inbox.raw);
    assert_eq!(cached.sig, inbox.sig);
    assert!(
        spool.get(Dir::Asks, &qid).unwrap().is_none(),
        "ask left asks/"
    );
    assert_eq!(
        spool.get(Dir::Done, &qid).unwrap().unwrap().state,
        "answered"
    );
    // B: acked → done/.
    let bs = b.spool();
    assert!(bs.get(Dir::Outbox, &aid).unwrap().is_none());
    assert_eq!(bs.get(Dir::Done, &aid).unwrap().unwrap().state, "acked");

    // The same question now hits A's cache without a send.
    b.running.shutdown();
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), ANSWER);
}

/// `--wait` runs the daemon's ingestion: a forged entry ahead of the genuine answer in B's
/// outbox is skipped and never acked; the genuine one is taken.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_skips_forged_entries_and_takes_the_genuine_answer() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let b_home = b.home().to_path_buf();
    let answerer = std::thread::spawn(move || {
        let spool = Spool::new(&b_home).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let (qid, rec) = loop {
            if let Some(x) = spool.list(Dir::Inbox, |_| true).unwrap().into_iter().next() {
                break x;
            }
            assert!(Instant::now() < deadline, "no question reached B");
            std::thread::sleep(Duration::from_millis(20));
        };
        let q = payload(&rec);
        // Forged first (lower id sorts first: UUIDv7 is time-ordered), genuine 1 s later.
        let forged = Payload::answer(&q, "not from Bea", "fake", 0, false);
        spool
            .put(
                Dir::Outbox,
                &forged.id,
                &record(&Envelope::sign(&forged, &id(99)), "unacked"),
            )
            .unwrap();
        std::thread::sleep(Duration::from_secs(1));
        let ans = Payload::answer(&q, ANSWER, "fake", 0, false);
        spool
            .put(
                Dir::Outbox,
                &ans.id,
                &record(
                    &Envelope::sign(&ans, &Identity::from_seed([2; 32])),
                    "unacked",
                ),
            )
            .unwrap();
        (qid, forged.id, ans.id)
    });
    let out = ask(home.path(), &["--wait", "10"]);
    let (qid, forged_id, aid) = answerer.join().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), ANSWER);

    let spool = Spool::new(home.path()).unwrap();
    assert!(spool.get(Dir::Inbox, &forged_id).unwrap().is_none());
    assert!(spool.get(Dir::Inbox, &aid).unwrap().is_some());
    assert_eq!(ids(&spool, Dir::Inbox), std::slice::from_ref(&aid));
    assert_eq!(
        spool.get(Dir::Done, &qid).unwrap().unwrap().state,
        "answered"
    );
    let bs = b.spool();
    assert_eq!(
        bs.get(Dir::Outbox, &forged_id).unwrap().unwrap().state,
        "unacked",
        "forged entry never acked"
    );
    assert!(bs.get(Dir::Done, &forged_id).unwrap().is_none());
    assert_eq!(bs.get(Dir::Done, &aid).unwrap().unwrap().state, "acked");
    b.running.shutdown();
}

/// Runs `owl ask --wait 10` against a scripted fake peer and returns (output, peer, home).
async fn ask_wait_fake(a: &Identity, script: FakeScript) -> (Output, FakePeer, TempDir) {
    let peer = spawn_fake(2, a, script).await;
    let home = asker_home(a, &peer.state.id, &[&peer.addr.to_string()]);
    let out = ask(home.path(), &["--wait", "10"]);
    (out, peer, home)
}

fn assert_answered_via_wait(out: &Output, peer: &FakePeer, home: &Path) -> (String, String) {
    let qid = peer.questions()[0].id.clone();
    let aid = peer.answers()[0].id.clone();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(out));
    assert_eq!(stdout(out).trim(), ANSWER);
    let spool = Spool::new(home).unwrap();
    let inbox = spool
        .get(Dir::Inbox, &aid)
        .unwrap()
        .expect("answer in inbox");
    assert_eq!(inbox.state, "pending");
    assert_eq!(inbox.meta["in_reply_to"], qid);
    let cached = spool
        .cache_get(&question_hash(PROJECT, Some(PATH), QUESTION))
        .unwrap()
        .expect("cached");
    assert_eq!(cached.raw, inbox.raw);
    assert!(
        spool.get(Dir::Asks, &qid).unwrap().is_none(),
        "ask left asks/"
    );
    assert_eq!(
        spool.get(Dir::Done, &qid).unwrap().unwrap().state,
        "answered"
    );
    (qid, aid)
}

/// The peer's outbox is unreachable for the first ~3.5 s (daemon restarting, say), so the polls
/// at t=0 and t=2 s both fail: `--wait` warns exactly once and keeps polling until the deadline
/// instead of giving up on the first error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_keeps_polling_through_transient_outbox_errors() {
    let a = id(1);
    let started = Instant::now();
    let (out, peer, home) = ask_wait_fake(
        &a,
        FakeScript {
            outbox_fails_until: Some(Instant::now() + Duration::from_millis(3500)),
            ..well_formed()
        },
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "answered once the outbox recovered, not at the deadline"
    );
    let (_qid, aid) = assert_answered_via_wait(&out, &peer, home.path());
    let err = stderr(&out);
    assert_eq!(
        err.matches("warning: polling Bea").count(),
        1,
        "one warning for two failing polls, not one per poll: {err}"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(3500),
        "the answer cannot arrive before the outbox recovers"
    );
    assert!(err.contains("500"), "names the peer's status: {err}");
    assert_eq!(peer.acked(), [aid], "acked once the answer was stored");
    peer.shutdown();

    // --quiet: same outcome, no warning at all.
    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            outbox_fails_until: Some(Instant::now() + Duration::from_millis(3500)),
            ..well_formed()
        },
    )
    .await;
    let home = asker_home(&a, &peer.state.id, &[&peer.addr.to_string()]);
    let out = ask(home.path(), &["--wait", "10", "--quiet"]);
    assert_answered_via_wait(&out, &peer, home.path());
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
    peer.shutdown();
}

/// The answer is stored, cached and the ask filed before the ack; a refused ack (anything but
/// `204`) is surfaced as a warning naming the answer, and the exchange still succeeds — the
/// unacked outbox entry is the responder's to expire (§3.5).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_warns_when_the_ack_is_refused() {
    let a = id(1);
    for status in [500u16, 404, 200] {
        let (out, peer, home) = ask_wait_fake(
            &a,
            FakeScript {
                ack_status: status,
                ..well_formed()
            },
        )
        .await;
        let (_qid, aid) = assert_answered_via_wait(&out, &peer, home.path());
        let err = stderr(&out);
        assert!(
            err.contains(&format!("warning: ack of {aid} failed")),
            "{status}: {err}"
        );
        assert!(err.contains("ack refused by script"), "{status}: {err}");
        assert!(
            peer.acked().is_empty(),
            "{status}: the fake never recorded an ack"
        );
        peer.shutdown();
    }
    // The 204 twin: identical flow, acked, silent.
    let (out, peer, home) = ask_wait_fake(&a, well_formed()).await;
    let (_qid, aid) = assert_answered_via_wait(&out, &peer, home.path());
    assert!(!stderr(&out).contains("warning"), "{}", stderr(&out));
    assert_eq!(peer.acked(), [aid]);
    peer.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_times_out_with_exit_4_and_keeps_the_ask_waiting() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let out = ask(home.path(), &["--wait", "0"]);
    assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
    assert!(stderr(&out).contains("timeout"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty());
    let spool = Spool::new(home.path()).unwrap();
    let asks = ids(&spool, Dir::Asks);
    assert_eq!(asks.len(), 1);
    let rec = spool.get(Dir::Asks, &asks[0]).unwrap().unwrap();
    assert_eq!(rec.state, "waiting");
    assert!(stderr(&out).contains(&asks[0]), "names the ask id");
    assert!(ids(&spool, Dir::Inbox).is_empty());
    assert!(ids(&spool, Dir::Cache).is_empty());
    assert!(ids(&spool, Dir::Done).is_empty());
    assert!(b.spool().get(Dir::Inbox, &asks[0]).unwrap().is_some());

    // An answer to a DIFFERENT question in B's outbox is not ours: still a timeout.
    let bs = b.spool();
    let other_q = Payload::question(&fp(&a), &fp(&b.id), PROJECT, Some(PATH), "other?");
    let other = Envelope::sign(
        &Payload::answer(&other_q, "not yours", "fake", 0, false),
        &b.id,
    );
    let other_id = payload(&record(&other, "unacked")).id;
    bs.put(Dir::Outbox, &other_id, &record(&other, "unacked"))
        .unwrap();
    let out = ask_with(
        home.path(),
        home.path(),
        "Second question?",
        &["--wait", "1"],
    );
    assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
    assert_eq!(ids(&spool, Dir::Asks).len(), 2);
    assert!(ids(&spool, Dir::Inbox).is_empty());
    assert!(
        bs.get(Dir::Outbox, &other_id).unwrap().is_some(),
        "not acked"
    );
    b.running.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unavailable_and_rate_limited_exit_codes() {
    let a = id(1);
    // 403: policy never.
    let never = spawn_daemon(
        2,
        true,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Never, None)))],
    )
    .await;
    let home = asker_home(&a, &never.id, &[&never.addr.to_string()]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("unavailable"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty());
    assert!(ids(&Spool::new(home.path()).unwrap(), Dir::Asks).is_empty());
    never.running.shutdown();

    // 403: responder disabled.
    let disabled = spawn_daemon(2, false, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &disabled.id, &[&disabled.addr.to_string()]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("unavailable"), "{}", stderr(&out));
    disabled.running.shutdown();

    // 429: one question per hour → the second ask is rate limited.
    let limited = spawn_daemon(
        2,
        true,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, Some(1))))],
    )
    .await;
    let home = asker_home(&a, &limited.id, &[&limited.addr.to_string()]);
    let first = accepted_id(&ask(home.path(), &[]));
    let out = ask_with(home.path(), home.path(), "Second question?", &[]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("rate limited, retry after "), "{err}");
    let secs: u64 = err
        .split("retry after ")
        .nth(1)
        .and_then(|s| s.trim_end().strip_suffix('s'))
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("retry-after seconds in {err:?}"));
    assert!((1..=3600).contains(&secs), "{secs}");
    assert_eq!(
        ids(&Spool::new(home.path()).unwrap(), Dir::Asks),
        [first],
        "only the accepted ask is recorded"
    );
    limited.running.shutdown();
}

/// `429` with and without `Retry-After`: the daemon always sends the header, so the bare arm
/// (exit 3, plain `rate limited`) and an unparseable header come from the scripted fake. Each
/// row differs from the next in the header only; nothing is spooled for any of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rate_limited_429_with_and_without_retry_after() {
    let a = id(1);
    let body = json!({ "error": "rate limited" }).to_string();
    for (what, headers, expect_msg) in [
        ("no Retry-After", vec![], "rate limited"),
        (
            "Retry-After: 42",
            vec![("retry-after".to_string(), "42".to_string())],
            "rate limited, retry after 42s",
        ),
        (
            "Retry-After: 0",
            vec![("retry-after".to_string(), "0".to_string())],
            "rate limited, retry after 0s",
        ),
        (
            "Retry-After as an HTTP date (not seconds)",
            vec![(
                "retry-after".to_string(),
                "Wed, 21 Oct 2026 07:28:00 GMT".to_string(),
            )],
            "rate limited",
        ),
    ] {
        let peer = spawn_fake(
            2,
            &a,
            FakeScript {
                accept: Some((429, body.clone())),
                accept_headers: headers,
                ..well_formed()
            },
        )
        .await;
        let home = asker_home(&a, &peer.state.id, &[&peer.addr.to_string()]);
        let out = ask(home.path(), &[]);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(3), "{what}: {err}");
        assert_eq!(err.trim(), format!("owl: {expect_msg}"), "{what}");
        assert!(stdout(&out).is_empty(), "{what}");
        assert!(!err.contains("panicked"), "{what}: {err}");
        assert_eq!(peer.questions().len(), 1, "{what}: the question was sent");
        let spool = Spool::new(home.path()).unwrap();
        for dir in Dir::ALL {
            assert!(
                ids(&spool, dir).is_empty(),
                "{what}: {} must stay empty",
                dir.name()
            );
        }
        peer.shutdown();
    }
}

/// A `200` is an answer only with its `X-Owl-Signature` header: the same signed body without
/// the header is a clean exit 1 naming the header, and nothing is spooled; with it, the answer
/// is verified, printed, stored and cached like `answer_200_is_verified_stored_and_cached`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answer_200_without_signature_header_is_a_clean_error() {
    let a = id(1);
    let b = id(2);
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    // The responder cache is shared across peers (§7): the 200 body may answer C's question.
    let c = id(3);
    let earlier = Payload::question(&fp(&c), &fp(&b), PROJECT, Some(PATH), QUESTION);
    let ans = Payload::answer(&earlier, "From the responder cache.", "fake", 1, false);
    let env = Envelope::sign(&ans, &b);

    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            accept: Some((200, env.raw.clone())),
            accept_headers: vec![],
            ..well_formed()
        },
    )
    .await;
    let home = asker_home(&a, &peer.state.id, &[&peer.addr.to_string()]);
    let out = ask(home.path(), &[]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(
        err.contains("200 answer without X-Owl-Signature header"),
        "{err}"
    );
    assert!(!err.contains("panicked"), "{err}");
    assert!(stdout(&out).is_empty());
    assert_eq!(peer.questions().len(), 1, "the question was sent");
    let spool = Spool::new(home.path()).unwrap();
    for dir in Dir::ALL {
        assert!(
            ids(&spool, dir).is_empty(),
            "{} must stay empty",
            dir.name()
        );
    }
    peer.shutdown();

    // Twin: the same body with its signature header is the verified answer.
    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            accept: Some((200, env.raw.clone())),
            accept_headers: vec![("x-owl-signature".to_string(), env.sig.clone())],
            ..well_formed()
        },
    )
    .await;
    let home = asker_home(&a, &peer.state.id, &[&peer.addr.to_string()]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "From the responder cache.");
    let spool = Spool::new(home.path()).unwrap();
    let inbox = spool
        .get(Dir::Inbox, &ans.id)
        .unwrap()
        .expect("answer in inbox");
    assert_eq!(inbox.raw, env.raw);
    assert_eq!(inbox.sig, env.sig);
    assert_eq!(spool.cache_get(&hash).unwrap().unwrap().raw, env.raw);
    assert!(ids(&spool, Dir::Asks).is_empty(), "answered: nothing waits");
    assert_eq!(ids(&spool, Dir::Done).len(), 1, "the question is filed");
    peer.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answer_200_is_verified_stored_and_cached() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    // B's responder cache holds the answer to an earlier, equivalent question from C.
    let c = id(3);
    let earlier = Payload::question(&fp(&c), &fp(&b.id), PROJECT, Some(PATH), QUESTION);
    let ans = Payload::answer(&earlier, "From the responder cache.", "fake", 1, false);
    let env = Envelope::sign(&ans, &b.id);
    b.spool()
        .cache_put(&hash, &record(&env, "pending"))
        .unwrap();

    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "From the responder cache.");
    let spool = Spool::new(home.path()).unwrap();
    assert!(ids(&spool, Dir::Asks).is_empty(), "answered: nothing waits");
    let inbox = spool
        .get(Dir::Inbox, &ans.id)
        .unwrap()
        .expect("answer in inbox");
    assert_eq!(inbox.state, "pending");
    assert_eq!(inbox.raw, env.raw);
    assert_eq!(inbox.sig, env.sig);
    assert_eq!(spool.cache_get(&hash).unwrap().unwrap().raw, env.raw);
    let done = ids(&spool, Dir::Done);
    assert_eq!(done.len(), 1, "the question is filed as answered");
    let done_rec = spool.get(Dir::Done, &done[0]).unwrap().unwrap();
    assert_eq!(done_rec.state, "answered");
    assert_eq!(payload(&done_rec).kind, Kind::Question);
    assert_eq!(done_rec.meta["answer"], ans.id);
    assert!(
        b.spool().get(Dir::Inbox, &done[0]).unwrap().is_none(),
        "B never spooled it"
    );

    // Same setup, but the cached answer is signed by someone else: refused, nothing stored.
    let forged = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let bad = Envelope::sign(&ans, &c);
    forged
        .spool()
        .cache_put(&hash, &record(&bad, "pending"))
        .unwrap();
    let home = asker_home(&a, &forged.id, &[&forged.addr.to_string()]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("does not verify"), "{}", stderr(&out));
    let spool = Spool::new(home.path()).unwrap();
    for dir in Dir::ALL {
        assert!(
            ids(&spool, dir).is_empty(),
            "{} must stay empty",
            dir.name()
        );
    }
    b.running.shutdown();
    forged.running.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unexpected_status_is_an_error_naming_it() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let book = ContactBook::load(home.path(), home.path()).unwrap();
    let contact = book.resolve("Bea").unwrap().clone();
    // A stale timestamp is the one bad request the client can be made to send.
    let mut q = question(&a, &b.id, QUESTION);
    q.ts = envelope::unix_to_rfc3339(envelope::now_unix() - 3600);
    let env = Envelope::sign(&q, &a);
    let ident = Identity::from_seed([1; 32]);
    let err = tokio::task::spawn_blocking(move || {
        client::send_question(
            &ident,
            &contact,
            &client::Iroh::Unavailable("none".into()),
            &env,
        )
    })
    .await
    .unwrap()
    .unwrap_err()
    .to_string();
    assert_eq!(err, "peer returned 400 Bad Request: stale ts");
    // And the happy path through the library maps to Accepted with the question id.
    let contact = book.resolve("Bea").unwrap().clone();
    let q = question(&a, &b.id, QUESTION);
    let env = Envelope::sign(&q, &a);
    let ident = Identity::from_seed([1; 32]);
    let out = tokio::task::spawn_blocking(move || {
        client::send_question(
            &ident,
            &contact,
            &client::Iroh::Unavailable("none".into()),
            &env,
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(out, SendOutcome::Accepted { ref id, .. } if *id == q.id),
        "{out:?}"
    );
    b.running.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_comes_from_origin_remote_unless_overridden() {
    let a = id(1);
    // B maps every project the asks below name: it refuses a project it does not map.
    let b = spawn_daemon_with(2, &[Peer::new(&a, "Ana", manual())], |cfg| {
        cfg.responder.enabled = true;
        for p in ["github.com/org/repo", "custom/id", "plainproj"] {
            cfg.projects.insert(p.into(), "/unused".into());
        }
    })
    .await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let repo = fixture_repo(&[("ana@example.org", 1)]);
    git(
        repo.path(),
        &["remote", "add", "origin", "git@github.com:org/repo.git"],
    );
    let project_of = |out: &Output| {
        let qid = accepted_id(out);
        match payload(&b.spool().get(Dir::Inbox, &qid).unwrap().unwrap()).body {
            Body::Question { project, .. } => project,
            _ => unreachable!("the fixture signs a question"),
        }
    };
    // Detected from the remote, also from a subdirectory of the repo.
    let out = ask_no_project(home.path(), repo.path(), "one?");
    assert_eq!(project_of(&out), "github.com/org/repo");
    let sub = repo.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let out = ask_no_project(home.path(), &sub, "two?");
    assert_eq!(project_of(&out), "github.com/org/repo");
    // --project wins over detection.
    let out = owl(home.path(), repo.path())
        .args(["ask", "Bea", PATH, "three?", "--project", "custom/id"])
        .output()
        .unwrap();
    assert_eq!(project_of(&out), "custom/id");
    // No repo: the directory name.
    let plain = tempfile::tempdir().unwrap();
    let dir = plain.path().join("plainproj");
    std::fs::create_dir(&dir).unwrap();
    let out = ask_no_project(home.path(), &dir, "four?");
    assert_eq!(project_of(&out), "plainproj");
    // The cache key follows the detected project: the same question in another project
    // is a different hash (both asks were accepted, so nothing was cached either way).
    let spool = Spool::new(home.path()).unwrap();
    assert_eq!(ids(&spool, Dir::Asks).len(), 4);
    b.running.shutdown();
}

#[test]
fn blame_candidates() {
    let (a, ana, zed) = (id(1), id(2), id(3));
    // 20 blamed lines; the unmatched author owns 11 of them, so a share over matched lines
    // only (6/9, 3/9) is nowhere near a share over all blamed lines (6/20, 3/20).
    // Zed owns more lines than Ana but sorts after her by name, Ana is committed first, and
    // Ana's fingerprint sorts first (the contact book's own order): the expected order
    // [Zed, Ana] is the line-share order and none of the name, commit or book orders.
    let repo = fixture_repo(&[
        ("ana@example.org", 3),
        ("zed@example.org", 6),
        ("nobody@example.org", 11),
    ]);
    assert!(
        fp(&ana) < fp(&zed),
        "book order is fingerprint order: Ana first"
    );
    let home = tempfile::tempdir().unwrap();
    prepare_home(
        home.path(),
        &a,
        true,
        &[Peer::new(&ana, "Ana", None), Peer::new(&zed, "Zed", None)],
    );
    let out = owl(home.path(), repo.path())
        .args(["ask", "--file", "f.txt", "Who owns this?", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let list = v.as_array().expect("JSON list");
    assert_eq!(list.len(), 2, "{v}");
    let names: Vec<&str> = list
        .iter()
        .map(|c| c.get("name").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(
        names,
        ["Zed", "Ana"],
        "ordered by line share, not by name: {v}"
    );
    let lines: Vec<u64> = list
        .iter()
        .map(|c| c.get("lines").and_then(Value::as_u64).unwrap())
        .collect();
    assert_eq!(lines, [6, 3]);
    let shares: Vec<f64> = list
        .iter()
        .map(|c| c.get("share").and_then(Value::as_f64).unwrap())
        .collect();
    assert_eq!(
        shares,
        [6.0 / 20.0, 3.0 / 20.0],
        "share of ALL blamed lines"
    );
    assert_eq!(shares, [0.3, 0.15]);
    assert_eq!(
        list[0].get("fingerprint").and_then(Value::as_str),
        Some(fp(&zed).as_str())
    );
    assert_eq!(
        list[1].get("fingerprint").and_then(Value::as_str),
        Some(fp(&ana).as_str())
    );
    assert!(!stdout(&out).contains("nobody"));
    // Listing candidates sends nothing.
    assert!(ids(&Spool::new(home.path()).unwrap(), Dir::Asks).is_empty());

    // Same repo, but Zed's contact carries an email git blame never saw: only Ana is listed.
    let home2 = tempfile::tempdir().unwrap();
    prepare_home(home2.path(), &a, true, &[Peer::new(&ana, "Ana", None)]);
    write_contact_full(
        home2.path(),
        &Peer::new(&zed, "Zed", None),
        &[],
        &["zed@elsewhere.example"],
    );
    let out = owl(home2.path(), repo.path())
        .args(["ask", "--file", "f.txt", "Who owns this?", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let names: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.get("name").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(names, ["Ana"], "{v}");

    // Without --json and without a terminal: the list goes to stderr with a hint, exit 1,
    // numbered in line-share order.
    let out = owl(home.path(), repo.path())
        .args(["ask", "--file", "f.txt", "Who owns this?"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    let zed_line = err
        .lines()
        .find(|l| l.starts_with("1) Zed"))
        .unwrap_or_else(|| panic!("Zed is candidate 1: {err}"));
    assert!(zed_line.contains("6 lines (30%)"), "{zed_line}");
    let ana_line = err
        .lines()
        .find(|l| l.starts_with("2) Ana"))
        .unwrap_or_else(|| panic!("Ana is candidate 2: {err}"));
    assert!(ana_line.contains("3 lines (15%)"), "{ana_line}");
    assert!(!err.contains("3)"), "exactly two candidates: {err}");
    assert!(err.contains("--peer"), "{err}");
    assert!(stdout(&out).is_empty());

    // A file git knows nothing about: blame fails cleanly.
    std::fs::write(repo.path().join("untracked.txt"), "x\n").unwrap();
    let out = owl(home.path(), repo.path())
        .args(["ask", "--file", "untracked.txt", "Who owns this?", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("git blame"), "{}", stderr(&out));

    // No contact matches any author: a clear error, exit 1.
    let home3 = tempfile::tempdir().unwrap();
    prepare_home(home3.path(), &a, true, &[Peer::new(&zed, "Yul", None)]);
    let out = owl(home3.path(), repo.path())
        .args(["ask", "--file", "f.txt", "Who owns this?"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("no contact matches"),
        "{}",
        stderr(&out)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_with_peer_skips_blame_and_sends() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    // Not even a git repo: with --peer, blame never runs and the file is just the path.
    let out = owl(home.path(), home.path())
        .args([
            "ask",
            "--file",
            "src/nowhere.rs",
            "--peer",
            "bea@example.org",
            "Why?",
            "--project",
            PROJECT,
        ])
        .output()
        .unwrap();
    let qid = accepted_id(&out);
    let rec = b.spool().get(Dir::Inbox, &qid).unwrap().unwrap();
    assert_eq!(
        payload(&rec).body,
        Body::Question {
            project: PROJECT.into(),
            path: Some("src/nowhere.rs".into()),
            question: "Why?".into(),
            context: None,
        }
    );
    // Missing question with --file is a usage error, nothing sent.
    let out = owl(home.path(), home.path())
        .args(["ask", "--file", "src/nowhere.rs", "--peer", "Bea"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("missing <question>"),
        "{}",
        stderr(&out)
    );
    assert_eq!(ids(&Spool::new(home.path()).unwrap(), Dir::Asks).len(), 1);
    b.running.shutdown();
}

// ---- Task state, threads, context ----------------------------------------------

/// Every new `owl` run is reaped under this deadline instead of an unbounded `wait()`.
const BOUND: Duration = Duration::from_secs(90);

/// Waits for `child` with a deadline: `try_wait` in a loop, then `kill()` and fail.
fn reap(mut child: std::process::Child, limit: Duration, what: &str) -> Output {
    let deadline = Instant::now() + limit;
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what} did not exit within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn owl_bounded(home: &Path, args: &[&str], limit: Duration) -> Output {
    let child = owl(home, home)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    reap(child, limit, "owl")
}

/// [`ask`] under a deadline.
fn ask_bounded(home: &Path, extra: &[&str], limit: Duration) -> Output {
    let mut args: Vec<&str> = vec!["ask", "Bea", PATH, QUESTION, "--project", PROJECT];
    args.extend_from_slice(extra);
    owl_bounded(home, &args, limit)
}

/// `owl ask … --context -` with `snippet` written to the child's stdin.
fn ask_context_stdin(home: &Path, snippet: &str, limit: Duration) -> Output {
    let mut child = owl(home, home)
        .args([
            "ask",
            "Bea",
            PATH,
            QUESTION,
            "--project",
            PROJECT,
            "--context",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(snippet.as_bytes())
        .unwrap();
    reap(child, limit, "owl ask --context -")
}

/// A fake that accepts every question (`202`, optional `state`) and never answers.
fn accepts_only(state: Option<&'static str>) -> FakeScript {
    FakeScript {
        ack_status: 204,
        accept_state: state,
        ..Default::default()
    }
}

fn home_for(a: &Identity, peer: &FakePeer) -> TempDir {
    asker_home(a, &peer.state.id, &[&peer.addr.to_string()])
}

/// The `<HH:MM> <text>` state lines of a `--wait` run, in order, with the clock parsed (not
/// matched as a substring). Warnings and the `accepted …` line start with a word, not a time.
fn transitions(err: &str) -> Vec<String> {
    err.lines()
        .filter_map(|line| {
            let (clock, text) = line.split_once(' ')?;
            let (h, m) = clock.split_once(':')?;
            if h.len() != 2 || m.len() != 2 {
                return None;
            }
            let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
            assert!(h < 24 && m < 60, "not a clock time: {line:?}");
            Some(text.to_string())
        })
        .collect()
}

fn question_context(p: &Payload) -> Option<&str> {
    match &p.body {
        Body::Question { context, .. } => context.as_deref(),
        _ => panic!("not a question: {p:?}"),
    }
}

// The `202` state is named on stdout, in plain text and in `--json`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_line_names_the_task_state() {
    let a = id(1);
    // Each row differs from the next only in the `state` the peer's `202` carried.
    for (state, text) in [
        (
            Some(envelope::TASK_STATE_SUBMITTED),
            Some("waiting for the owner's consent"),
        ),
        (
            Some(envelope::TASK_STATE_WORKING),
            Some("the owner's agent is answering"),
        ),
        // No `state` key at all, and a state name this version does not know: both fall
        // back to the bare line (`state_text` → `None`).
        (None, None),
        (Some("TASK_STATE_CANCELED"), None),
    ] {
        let peer = spawn_fake(2, &a, accepts_only(state)).await;
        let home = home_for(&a, &peer);

        let out = ask_bounded(home.path(), &[], BOUND);
        assert_eq!(out.status.code(), Some(0), "{state:?}: {}", stderr(&out));
        assert!(stderr(&out).is_empty(), "{state:?}: {}", stderr(&out));
        let qid = peer.questions()[0].id.clone();
        let expected = match text {
            Some(t) => format!("accepted {qid} — {t}\n"),
            None => format!("accepted {qid}\n"),
        };
        assert_eq!(stdout(&out), expected, "{state:?}");

        let out = ask_bounded(home.path(), &["--json"], BOUND);
        assert_eq!(out.status.code(), Some(0), "{state:?}: {}", stderr(&out));
        let qid2 = peer.questions()[1].id.clone();
        let got: Value = serde_json::from_str(&stdout(&out)).unwrap();
        // The raw state name travels in `--json`, not the human text (and `null` when the
        // peer sent none) — asserted as a whole object, so no extra key can sneak in.
        assert_eq!(
            got,
            json!({ "status": "accepted", "id": qid2, "state": state }),
            "{state:?}"
        );
        assert!(peer.task_fetches().is_empty(), "no --wait, no task fetch");
        peer.shutdown();
    }
}

/// `--wait` prints one `<HH:MM> <text>` line per CHANGE of the peer's Task text — the
/// repeated `SUBMITTED` tick prints nothing — and the answer still lands on stdout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_prints_each_task_state_change_once() {
    let a = id(1);
    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            accept_state: Some(envelope::TASK_STATE_SUBMITTED),
            tasks: vec![
                task_body(
                    envelope::TASK_STATE_SUBMITTED,
                    "waiting for the owner's consent",
                ),
                task_body(
                    envelope::TASK_STATE_SUBMITTED,
                    "waiting for the owner's consent",
                ),
                task_body(
                    envelope::TASK_STATE_WORKING,
                    "the owner's agent is answering",
                ),
            ],
            answer_after_tasks: 3,
            ..well_formed()
        },
    )
    .await;
    let home = home_for(&a, &peer);
    let out = ask_bounded(home.path(), &["--wait", "20"], BOUND);
    let (qid, _aid) = assert_answered_via_wait(&out, &peer, home.path());
    let err = stderr(&out);
    assert_eq!(
        transitions(&err),
        [
            "waiting for the owner's consent",
            "the owner's agent is answering"
        ],
        "one line per change, the repeat silent: {err}"
    );
    assert!(
        err.contains(&format!(
            "accepted {qid} — waiting for the owner's consent; waiting up to 20s for an answer"
        )),
        "{err}"
    );
    // Counted by the peer, not inferred from the client's timing: three Task fetches (the
    // fourth tick found the answer in the outbox and returned first).
    assert_eq!(peer.task_fetches(), [qid.clone(), qid.clone(), qid]);
    peer.shutdown();
}

/// `--quiet` drops every state line; the same script still returns the answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_quiet_prints_no_task_states() {
    let a = id(1);
    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            accept_state: Some(envelope::TASK_STATE_SUBMITTED),
            tasks: vec![
                task_body(
                    envelope::TASK_STATE_SUBMITTED,
                    "waiting for the owner's consent",
                ),
                task_body(
                    envelope::TASK_STATE_WORKING,
                    "the owner's agent is answering",
                ),
            ],
            answer_after_tasks: 2,
            ..well_formed()
        },
    )
    .await;
    let home = home_for(&a, &peer);
    let out = ask_bounded(home.path(), &["--wait", "20", "--quiet"], BOUND);
    let (qid, _aid) = assert_answered_via_wait(&out, &peer, home.path());
    let err = stderr(&out);
    assert!(transitions(&err).is_empty(), "{err}");
    assert!(err.is_empty(), "{err}");
    assert_eq!(peer.task_fetches(), [qid.clone(), qid]);
    peer.shutdown();
}

/// A Task that turns `REJECTED` ends the wait at once — `declined by <name>: <text>`,
/// exit 2, the ask filed under `done/` as `declined` and shown so by `owl history`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_declined_task_exits_2_and_files_the_ask() {
    let a = id(1);
    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            accept_state: Some(envelope::TASK_STATE_SUBMITTED),
            tasks: vec![
                task_body(
                    envelope::TASK_STATE_SUBMITTED,
                    "waiting for the owner's consent",
                ),
                task_body(envelope::TASK_STATE_REJECTED, "the owner declined"),
            ],
            ack_status: 204,
            ..Default::default()
        },
    )
    .await;
    let home = home_for(&a, &peer);
    let out = ask_bounded(home.path(), &["--wait", "20"], BOUND);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(err.contains("declined by Bea: the owner declined"), "{err}");
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    // The rejecting tick reports the decline, not another state line.
    assert_eq!(
        transitions(&err),
        ["waiting for the owner's consent"],
        "{err}"
    );

    let qid = peer.questions()[0].id.clone();
    assert_eq!(peer.task_fetches(), [qid.clone(), qid.clone()]);
    let spool = Spool::new(home.path()).unwrap();
    assert!(ids(&spool, Dir::Asks).is_empty(), "the ask left asks/");
    assert_eq!(
        spool.get(Dir::Done, &qid).unwrap().unwrap().state,
        "declined"
    );
    assert!(ids(&spool, Dir::Inbox).is_empty());
    assert!(ids(&spool, Dir::Cache).is_empty());

    let hist = owl_bounded(home.path(), &["history", "--json"], BOUND);
    assert_eq!(hist.status.code(), Some(0), "{}", stderr(&hist));
    let rows: Value = serde_json::from_str(&stdout(&hist)).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    assert_eq!(rows[0]["id"], qid.as_str());
    assert_eq!(rows[0]["state"], "declined");
    assert_eq!(rows[0]["peer_name"], "Bea");
    peer.shutdown();
}

/// A peer without the Task route (`404` on every fetch — the `Ok(None)` arm) waits
/// exactly as before: the answer is delivered, exit 0, and nothing is printed about state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_works_against_a_peer_without_a_task_route() {
    let a = id(1);
    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            // Empty script = no task route; the answer is held back so the route is really
            // reached before the wait ends.
            tasks: vec![],
            answer_after_tasks: 2,
            ..well_formed()
        },
    )
    .await;
    let home = home_for(&a, &peer);
    let out = ask_bounded(home.path(), &["--wait", "20"], BOUND);
    let (qid, _aid) = assert_answered_via_wait(&out, &peer, home.path());
    let err = stderr(&out);
    assert!(transitions(&err).is_empty(), "{err}");
    assert!(
        !err.contains("warning"),
        "a 404 is not worth a warning: {err}"
    );
    assert!(
        err.contains(&format!("accepted {qid}; waiting up to 20s")),
        "no state from this peer, so no dash: {err}"
    );
    assert_eq!(peer.task_fetches(), [qid.clone(), qid]);
    peer.shutdown();
}

/// `--context <file>` trims the file (outer whitespace only) and sends the bytes as
/// `body.context`; without it the field is absent. The negative twin differs only in the flag.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_travels_on_the_wire_and_into_the_ask_record() {
    let a = id(1);
    let peer = spawn_fake(2, &a, accepts_only(None)).await;
    let home = home_for(&a, &peer);
    let snippet = "  \n\tfn rotate(t: &Token) -> Token {\n\n    t.next()\n}\n \n";
    let trimmed = snippet.trim();
    assert!(
        trimmed.contains('\n') && trimmed.contains("\n\n"),
        "the fixture proves the trim is outer-only"
    );
    let file = home.path().join("snippet.txt");
    std::fs::write(&file, snippet).unwrap();

    let out = ask_bounded(home.path(), &["--context", file.to_str().unwrap()], BOUND);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let qid = peer.questions()[0].id.clone();
    assert_eq!(stdout(&out), format!("accepted {qid}\n"));

    let spool = Spool::new(home.path()).unwrap();
    let rec = spool.get(Dir::Asks, &qid).unwrap().expect("asks/<id>.json");
    let stored: Payload = serde_json::from_str(&rec.raw).unwrap();
    assert_eq!(
        stored.body,
        Body::Question {
            project: PROJECT.into(),
            path: Some(PATH.into()),
            question: QUESTION.into(),
            context: Some(trimmed.into()),
        }
    );
    assert_eq!(
        question_context(&stored).unwrap().as_bytes(),
        trimmed.as_bytes(),
        "byte-identical to the trimmed file"
    );
    assert_eq!(rec.meta["threaded"], true);
    // The peer got the same bytes.
    assert_eq!(
        question_context(&peer.questions()[0]).unwrap().as_bytes(),
        trimmed.as_bytes()
    );

    // Negative twin: the same ask without `--context` carries none and is not threaded.
    let out = ask_bounded(home.path(), &[], BOUND);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let qid2 = peer.questions()[1].id.clone();
    let rec2 = spool.get(Dir::Asks, &qid2).unwrap().unwrap();
    assert_eq!(question_context(&payload(&rec2)), None);
    assert!(
        !rec2.raw.contains("\"context\""),
        "omitted on the wire: {}",
        rec2.raw
    );
    assert_eq!(rec2.meta["threaded"], false);
    peer.shutdown();
}

/// `--context -` reads the snippet from stdin, trimmed the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_from_stdin() {
    let a = id(1);
    let peer = spawn_fake(2, &a, accepts_only(None)).await;
    let home = home_for(&a, &peer);
    let snippet = "\n\n  error[E0308]: mismatched types\n\n   --> src/lib.rs:12\n  \n";
    let trimmed = snippet.trim();

    let out = ask_context_stdin(home.path(), snippet, BOUND);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let qid = peer.questions()[0].id.clone();
    assert_eq!(stdout(&out), format!("accepted {qid}\n"));
    assert_eq!(
        question_context(&peer.questions()[0]).unwrap().as_bytes(),
        trimmed.as_bytes()
    );
    let spool = Spool::new(home.path()).unwrap();
    let rec = spool.get(Dir::Asks, &qid).unwrap().unwrap();
    assert_eq!(question_context(&payload(&rec)), Some(trimmed));
    assert_eq!(rec.meta["threaded"], true);
    peer.shutdown();
}

/// The cap is 8192 BYTES, measured after trimming. Each row differs from the accepted
/// ones only in the size of the snippet; nothing is sent for a rejected one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_cap_is_bytes_after_trimming() {
    let a = id(1);
    let peer = spawn_fake(2, &a, accepts_only(None)).await;
    let home = home_for(&a, &peer);
    let multibyte = "ł".repeat(4097);
    assert_eq!((multibyte.len(), multibyte.chars().count()), (8194, 4097));
    let padded = format!("\n\n{}\n\n", "x".repeat(8192));
    let mut sent = 0;
    for (what, snippet, expected) in [
        ("exactly the cap", "x".repeat(8192), Ok(8192)),
        ("one byte over", "x".repeat(8193), Err(8193)),
        // 4097 chars, 8194 bytes: under the cap in characters, over it in bytes.
        ("multi-byte over", multibyte.clone(), Err(8194)),
        // Raw 8196 bytes, 8192 once trimmed: the trim happens before the measurement.
        ("padded to the cap", padded.clone(), Ok(8192)),
    ] {
        let file = home.path().join("big.txt");
        std::fs::write(&file, &snippet).unwrap();
        let out = ask_bounded(home.path(), &["--context", file.to_str().unwrap()], BOUND);
        match expected {
            Ok(bytes) => {
                sent += 1;
                assert_eq!(out.status.code(), Some(0), "{what}: {}", stderr(&out));
                assert_eq!(
                    question_context(peer.questions().last().unwrap())
                        .unwrap()
                        .len(),
                    bytes,
                    "{what}"
                );
            }
            Err(n) => {
                assert_eq!(out.status.code(), Some(1), "{what}: {}", stderr(&out));
                assert_eq!(
                    stderr(&out).trim_end(),
                    format!("owl: context is {n} bytes, max 8192"),
                    "{what}"
                );
                assert!(stdout(&out).is_empty(), "{what}");
            }
        }
        assert_eq!(
            peer.questions().len(),
            sent,
            "{what}: nothing else was sent"
        );
    }
    peer.shutdown();
}

/// A `--context` question bypasses the asker cache in both directions — it is never
/// served from a hit, and its answer is never written back. The 2×2 of
/// {context absent, present} × {cache hit, no cache}.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_bypasses_the_asker_cache() {
    let a = id(1);
    let peer = spawn_fake(2, &a, well_formed()).await;
    let home = home_for(&a, &peer);
    let spool = Spool::new(home.path()).unwrap();
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    let file = home.path().join("snippet.txt");
    std::fs::write(&file, "let token = rotate(&t);").unwrap();
    let ctx = ["--context", file.to_str().unwrap()];

    // 1. No context, no cache: sent, answered, and the answer cached.
    let out = ask_bounded(home.path(), &["--wait", "20"], BOUND);
    assert_answered_via_wait(&out, &peer, home.path());
    let cached = spool.cache_get(&hash).unwrap().expect("cached").raw;
    assert_eq!(peer.questions().len(), 1);

    // 2. No context, cache hit: served from the cache, nothing sent.
    let out = ask_bounded(home.path(), &[], BOUND);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), ANSWER);
    assert_eq!(peer.questions().len(), 1, "the cache answered it");

    // 3. Context + the same cache hit: the hit is ignored, the question goes out again, and
    //    the new answer does not overwrite the cache entry.
    let out = ask_bounded(home.path(), &[ctx[0], ctx[1], "--wait", "20"], BOUND);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), ANSWER);
    assert_eq!(peer.questions().len(), 2, "the cache did not serve it");
    assert_eq!(
        question_context(&peer.questions()[1]),
        Some("let token = rotate(&t);")
    );
    assert_eq!(
        spool.cache_get(&hash).unwrap().unwrap().raw,
        cached,
        "the threaded answer was not written to the cache"
    );
    let aid2 = peer.answers()[1].id.clone();
    assert!(
        spool.get(Dir::Inbox, &aid2).unwrap().is_some(),
        "it is stored in inbox/, only not cached"
    );

    // 4. Context, no cache at all: answered, stored, and the cache stays empty.
    let home2 = home_for(&a, &peer);
    let spool2 = Spool::new(home2.path()).unwrap();
    let out = ask_bounded(home2.path(), &[ctx[0], ctx[1], "--wait", "20"], BOUND);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), ANSWER);
    assert_eq!(peer.questions().len(), 3);
    let qid3 = peer.questions()[2].id.clone();
    assert!(
        ids(&spool2, Dir::Cache).is_empty(),
        "a threaded answer is never cached"
    );
    assert_eq!(ids(&spool2, Dir::Inbox).len(), 1);
    assert_eq!(
        spool2.get(Dir::Done, &qid3).unwrap().unwrap().state,
        "answered"
    );
    peer.shutdown();
}

/// Pure serde: `context` and `context_id` are omitted when absent, and both an
/// explicit `null` and a missing key read back as `None`.
#[test]
fn context_is_optional_on_the_wire() {
    let (a, b) = (id(1), id(2));
    let mut p = Payload::question(&fp(&a), &fp(&b), PROJECT, Some(PATH), QUESTION);
    let bare = serde_json::to_string(&p).unwrap();
    assert!(!bare.contains("\"context\""), "{bare}");
    assert!(!bare.contains("\"context_id\""), "{bare}");
    assert_eq!(
        serde_json::from_str::<Payload>(&bare).unwrap(),
        p,
        "absent keys round-trip to None"
    );

    p.context_id = Some("0199001a-0000-7000-8000-000000000001".into());
    if let Body::Question { context, .. } = &mut p.body {
        *context = Some("let x = 1;".into());
    }
    let full = serde_json::to_string(&p).unwrap();
    assert!(full.contains("\"context\":\"let x = 1;\""), "{full}");
    assert_eq!(serde_json::from_str::<Payload>(&full).unwrap(), p);

    // Explicit nulls are the same as absent.
    let mut v: Value = serde_json::from_str(&full).unwrap();
    v["context_id"] = Value::Null;
    v["body"]["context"] = Value::Null;
    let nulled: Payload = serde_json::from_value(v).unwrap();
    assert_eq!(nulled.context_id, None);
    assert_eq!(question_context(&nulled), None);
}

/// `--reply-to` an open `asks/` record reuses that exchange's thread id, on the wire
/// and in the new record; without it every ask mints a fresh UUID thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reply_to_reuses_an_open_asks_thread() {
    let a = id(1);
    let peer = spawn_fake(2, &a, accepts_only(None)).await;
    let home = home_for(&a, &peer);
    let spool = Spool::new(home.path()).unwrap();

    let qid = accepted_id(&ask_bounded(home.path(), &[], BOUND));
    let thread = payload(&spool.get(Dir::Asks, &qid).unwrap().unwrap())
        .context_id
        .expect("a fresh thread id");

    let out = ask_bounded(home.path(), &["--reply-to", &qid], BOUND);
    let qid2 = accepted_id(&out);
    assert_ne!(qid2, qid);
    assert_eq!(
        peer.questions()[1].context_id.as_deref(),
        Some(thread.as_str()),
        "the peer saw the thread on the wire"
    );
    assert_eq!(
        payload(&spool.get(Dir::Asks, &qid2).unwrap().unwrap()).context_id,
        Some(thread.clone())
    );
    assert_eq!(
        spool.get(Dir::Asks, &qid2).unwrap().unwrap().meta["threaded"],
        true
    );
    // The open ask itself is untouched.
    assert_eq!(
        payload(&spool.get(Dir::Asks, &qid).unwrap().unwrap()).context_id,
        Some(thread)
    );
    peer.shutdown();
}

/// The thread of a FINISHED exchange is reachable from either end — the question in
/// `done/` and the answer in `inbox/` — and the answer carries the question's thread id.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reply_to_reuses_a_finished_exchange_thread() {
    let a = id(1);
    let peer = spawn_fake(2, &a, well_formed()).await;
    let home = home_for(&a, &peer);
    let spool = Spool::new(home.path()).unwrap();

    let out = ask_bounded(home.path(), &["--wait", "20"], BOUND);
    let (qid, aid) = assert_answered_via_wait(&out, &peer, home.path());
    let thread = payload(&spool.get(Dir::Done, &qid).unwrap().unwrap())
        .context_id
        .expect("the question's thread");
    // The answer we received is on the same thread as the question we sent.
    assert_eq!(
        payload(&spool.get(Dir::Inbox, &aid).unwrap().unwrap()).context_id,
        Some(thread.clone()),
        "the answer copies the question's context_id"
    );

    // From the `done/` question…
    let from_done = accepted_id(&ask_bounded(home.path(), &["--reply-to", &qid], BOUND));
    assert_eq!(
        payload(&spool.get(Dir::Asks, &from_done).unwrap().unwrap()).context_id,
        Some(thread.clone())
    );
    // …and from the `inbox/` answer (whose counterpart is `payload.from`).
    let from_answer = accepted_id(&ask_bounded(home.path(), &["--reply-to", &aid], BOUND));
    assert_eq!(
        payload(&spool.get(Dir::Asks, &from_answer).unwrap().unwrap()).context_id,
        Some(thread.clone())
    );
    assert_eq!(
        peer.questions()
            .iter()
            .filter_map(|q| q.context_id.clone())
            .filter(|c| *c == thread)
            .count(),
        3,
        "all three questions travelled on one thread"
    );
    peer.shutdown();
}

/// A thread the peer started: their question to us, still in `inbox/` or finished in `done/`
/// (answered or rejected), is continued by `--reply-to` on its own thread id.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reply_to_continues_a_thread_the_peer_started() {
    let a = id(1);
    let peer = spawn_fake(2, &a, accepts_only(None)).await;
    let home = home_for(&a, &peer);
    let spool = Spool::new(home.path()).unwrap();
    for (n, (dir, state)) in [
        (Dir::Inbox, "pending"),
        (Dir::Done, "answered"),
        (Dir::Done, "rejected"),
    ]
    .into_iter()
    .enumerate()
    {
        let thread = uuid::Uuid::now_v7().to_string();
        let mut q = Payload::question(&fp(&peer.state.id), &fp(&a), PROJECT, Some(PATH), "Yours?");
        q.context_id = Some(thread.clone());
        let env = Envelope::sign(&q, &peer.state.id);
        spool.put(dir, &q.id, &record(&env, state)).unwrap();

        let qid = accepted_id(&ask_bounded(home.path(), &["--reply-to", &q.id], BOUND));
        assert_eq!(
            peer.questions()[n].context_id.as_deref(),
            Some(&*thread),
            "{state}: the reply travels on the peer's thread"
        );
        assert_eq!(
            payload(&spool.get(Dir::Asks, &qid).unwrap().unwrap()).context_id,
            Some(thread),
            "{state}: the stored ask keeps the peer's thread"
        );
    }
    peer.shutdown();
}

/// Every `--reply-to` failure names its reason and sends nothing. The positive twin is
/// the same record shape, addressed to Bea and carrying a thread id.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reply_to_errors_name_the_reason() {
    let a = id(1);
    let other = id(3);
    let peer = spawn_fake(2, &a, accepts_only(None)).await;
    let home = home_for(&a, &peer);
    write_contact_full(
        home.path(),
        &Peer::new(&other, "Cid", None),
        &[&closed_port()],
        &["cid@example.org"],
    );
    let spool = Spool::new(home.path()).unwrap();
    let thread = uuid::Uuid::now_v7().to_string();
    // Three records of the SAME shape; each differs from the twin below it in one field.
    let mut stored = Vec::new();
    for (to, context_id) in [
        (&peer.state.id, Some(thread.clone())),
        (&other, Some(thread.clone())),
        (&peer.state.id, None),
    ] {
        let mut q = Payload::question(&fp(&a), &fp(to), PROJECT, Some(PATH), "Earlier?");
        q.context_id = context_id;
        let env = Envelope::sign(&q, &a);
        spool
            .put(Dir::Asks, &q.id, &record(&env, "waiting"))
            .unwrap();
        stored.push(q.id);
    }
    let (to_bea, to_cid, no_thread) = (&stored[0], &stored[1], &stored[2]);

    for (what, id, message) in [
        (
            "unknown id",
            "does-not-exist",
            "owl: no exchange does-not-exist",
        ),
        (
            "another peer",
            to_cid.as_str(),
            &format!("owl: {to_cid} was asked to Cid, not Bea"),
        ),
        (
            "no thread id",
            no_thread.as_str(),
            &format!("owl: {no_thread} carries no thread id"),
        ),
    ] {
        let out = ask_bounded(home.path(), &["--reply-to", id], BOUND);
        assert_eq!(out.status.code(), Some(1), "{what}: {}", stderr(&out));
        assert_eq!(stderr(&out).trim_end(), message, "{what}");
        assert!(stdout(&out).is_empty(), "{what}");
        assert!(peer.questions().is_empty(), "{what}: nothing was sent");
        assert_eq!(ids(&spool, Dir::Asks).len(), 3, "{what}: nothing recorded");
    }

    // Positive twin: the record addressed to Bea, carrying the same thread id, works.
    let out = ask_bounded(home.path(), &["--reply-to", to_bea], BOUND);
    let qid = accepted_id(&out);
    assert_eq!(peer.questions()[0].context_id.as_deref(), Some(&*thread));
    assert_eq!(
        payload(&spool.get(Dir::Asks, &qid).unwrap().unwrap()).context_id,
        Some(thread)
    );
    peer.shutdown();
}

/// Without `--reply-to` each ask starts its own thread — a fresh, parseable UUID.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_ask_mints_a_fresh_thread_id() {
    let a = id(1);
    let peer = spawn_fake(2, &a, accepts_only(None)).await;
    let home = home_for(&a, &peer);
    let spool = Spool::new(home.path()).unwrap();

    let first = accepted_id(&ask_bounded(home.path(), &[], BOUND));
    let second = accepted_id(&ask_with(home.path(), home.path(), "And this one?", &[]));
    let threads: Vec<String> = [&first, &second]
        .iter()
        .map(|qid| {
            let rec = spool.get(Dir::Asks, qid).unwrap().unwrap();
            let stored = payload(&rec).context_id.expect("a thread id");
            let sent = peer
                .questions()
                .iter()
                .find(|q| q.id == **qid)
                .unwrap()
                .context_id
                .clone()
                .unwrap();
            assert_eq!(stored, sent, "the stored thread is the one sent");
            assert!(uuid::Uuid::parse_str(&stored).is_ok(), "{stored} is a UUID");
            stored
        })
        .collect();
    assert_ne!(threads[0], threads[1], "a fresh thread per ask");
    peer.shutdown();
}

/// [`ask_bounded`] with a question of its own, so a test can seed a second exchange whose
/// hash differs from [`QUESTION`]'s.
fn ask_q_bounded(home: &Path, question: &str, extra: &[&str], limit: Duration) -> Output {
    let mut args: Vec<&str> = vec!["ask", "Bea", PATH, question, "--project", PROJECT];
    args.extend_from_slice(extra);
    owl_bounded(home, &args, limit)
}

/// Both ERROR arms of `client::fetch_task` — a non-200/404 status and a `200` whose
/// body is not a Task — are swallowed by `wait_for_answer` (`if let Ok(Some(task))`). The
/// pinned behaviour: the wait is NOT aborted, no `<HH:MM>` transition line and no warning
/// reach stderr, and the outbox poll still delivers the answer. The peer's own fetch counter
/// proves the failing route was really reached, twice, before the answer arrived.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_survives_both_error_arms_of_the_task_fetch() {
    let a = id(1);
    for (label, row) in [
        // `status => bail!(error_message(status, …))`.
        ("500", (500u16, json!({ "error": "task store is down" }))),
        // The `200` arm with `Task::parse` → `None` → `task body is not an A2A Task`.
        // `fake_task` fills in `id`, `contextId` and `status.message.messageId`, so the body
        // does end up with a `status` object — but still no `status.state`, which is the one
        // key `Task::parse` requires, so this genuinely fails to parse.
        ("200-not-a-task", (200u16, json!({ "hello": "world" }))),
    ] {
        let peer = spawn_fake(
            2,
            &a,
            FakeScript {
                tasks: vec![row],
                // Hold the answer back until the failing route has been hit twice.
                answer_after_tasks: 2,
                ..well_formed()
            },
        )
        .await;
        let home = home_for(&a, &peer);
        let out = ask_bounded(home.path(), &["--wait", "20"], BOUND);
        let (qid, _aid) = assert_answered_via_wait(&out, &peer, home.path());
        let err = stderr(&out);
        assert!(transitions(&err).is_empty(), "{label}: {err}");
        // The whole of stderr is the accepted line: the error is swallowed without a word.
        assert_eq!(
            err.trim(),
            format!("accepted {qid}; waiting up to 20s for an answer"),
            "{label}"
        );
        assert_eq!(
            peer.task_fetches(),
            [qid.clone(), qid],
            "{label}: the failing route was really called"
        );
        peer.shutdown();
    }
}

/// The inline-answer path writes the asker cache
/// only for an UNTHREADED question. All three arms send the same `<project, path, question>`
/// — neither `--context` nor `--reply-to` enters `question_hash` — so they share one hash
/// and differ in exactly the one flag that makes the question threaded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inline_answer_caches_only_unthreaded_questions() {
    let a = id(1);
    let b = id(2);
    // The peer answers every question straight away with a `200` out of its responder cache
    // (§7: that cache is shared, so the body may answer C's earlier, equivalent question).
    let c = id(3);
    let earlier = Payload::question(&fp(&c), &fp(&b), PROJECT, Some(PATH), QUESTION);
    let ans = Payload::answer(&earlier, "From the responder cache.", "fake", 1, false);
    let env = Envelope::sign(&ans, &b);
    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            accept: Some((200, env.raw.clone())),
            accept_headers: vec![("x-owl-signature".to_string(), env.sig.clone())],
            ..well_formed()
        },
    )
    .await;
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);

    // 1. Positive twin: a plain question answered inline IS cached.
    let home = home_for(&a, &peer);
    let spool = Spool::new(home.path()).unwrap();
    let out = ask_bounded(home.path(), &[], BOUND);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "From the responder cache.");
    assert_eq!(
        spool.cache_get(&hash).unwrap().expect("plain: cached").raw,
        env.raw
    );

    // 2. The same question with `--context` is NOT cached — while the rest of the inline
    //    path still runs: the answer lands in `inbox/` and the ask in `done/` as `answered`.
    let home = home_for(&a, &peer);
    let spool = Spool::new(home.path()).unwrap();
    let file = home.path().join("snippet.txt");
    std::fs::write(&file, "let token = rotate(&t);").unwrap();
    let out = ask_bounded(home.path(), &["--context", file.to_str().unwrap()], BOUND);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "From the responder cache.");
    assert!(
        ids(&spool, Dir::Cache).is_empty(),
        "--context: the asker cache stays empty"
    );
    let ctx_qid = peer.questions().last().unwrap().id.clone();
    assert_eq!(
        question_context(peer.questions().last().unwrap()),
        Some("let token = rotate(&t);"),
        "the arm really was the threaded one"
    );
    let inbox = spool
        .get(Dir::Inbox, &ans.id)
        .unwrap()
        .expect("answer in inbox");
    assert_eq!(inbox.raw, env.raw);
    assert_eq!(inbox.meta["hash"], hash.as_str());
    assert_eq!(
        spool.get(Dir::Done, &ctx_qid).unwrap().unwrap().state,
        "answered"
    );

    // 3. `--reply-to` makes the question threaded the same way. The thread is seeded with a
    //    DIFFERENT question, so its (unthreaded) answer caches under its own hash and leaves
    //    `hash` untouched — the reply's absence from the cache is therefore its own doing.
    let other = "Which module owns the refresh loop?";
    let home = home_for(&a, &peer);
    let spool = Spool::new(home.path()).unwrap();
    let seed = ask_q_bounded(home.path(), other, &[], BOUND);
    assert_eq!(seed.status.code(), Some(0), "{}", stderr(&seed));
    let seed_qid = peer.questions().last().unwrap().id.clone();
    let other_hash = question_hash(PROJECT, Some(PATH), other);
    assert!(
        spool.cache_get(&other_hash).unwrap().is_some(),
        "the seed is unthreaded, so it is cached"
    );
    assert!(spool.cache_get(&hash).unwrap().is_none());
    let out = ask_bounded(home.path(), &["--reply-to", &seed_qid], BOUND);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "From the responder cache.");
    assert!(
        spool.cache_get(&hash).unwrap().is_none(),
        "--reply-to: the asker cache is not written"
    );
    assert_eq!(
        ids(&spool, Dir::Cache),
        [other_hash],
        "only the unthreaded seed's entry"
    );
    let reply_qid = peer.questions().last().unwrap().id.clone();
    assert_ne!(reply_qid, seed_qid);
    assert_eq!(
        spool.get(Dir::Done, &reply_qid).unwrap().unwrap().state,
        "answered"
    );
    peer.shutdown();
}

/// `--context` naming a file that does not exist fails in
/// `read_context`, before the question is signed or sent — exit 1, the path and the IO
/// reason on stderr, an untouched spool and a peer that saw nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_file_that_does_not_exist_sends_nothing() {
    let a = id(1);
    let peer = spawn_fake(2, &a, accepts_only(None)).await;
    let home = home_for(&a, &peer);
    let out = ask_bounded(
        home.path(),
        &["--context", "/no/such/file/anywhere.txt"],
        BOUND,
    );
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert_eq!(
        err.trim(),
        "owl: reading /no/such/file/anywhere.txt: No such file or directory (os error 2)"
    );
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert!(
        peer.questions().is_empty(),
        "the failure is before the send"
    );
    let spool = Spool::new(home.path()).unwrap();
    for dir in Dir::ALL {
        assert!(
            ids(&spool, dir).is_empty(),
            "{} must stay empty",
            dir.name()
        );
    }
    peer.shutdown();
}

// ---- The event log a new ask is born with -----------------------------------------

/// The `meta.events` kinds and `by` values of a record, or a panic naming the record.
fn events_of(spool: &Spool, dir: Dir, id: &str) -> Vec<(String, Option<String>)> {
    let rec = spool
        .get(dir, id)
        .unwrap()
        .unwrap_or_else(|| panic!("no {id} in {}", dir.name()));
    rec.meta["events"]
        .as_array()
        .unwrap_or_else(|| panic!("no meta.events on {id}: {}", rec.meta))
        .iter()
        .map(|e| {
            assert!(e["ts"].as_str().is_some_and(|t| t.ends_with('Z')), "{e}");
            (
                e["kind"].as_str().unwrap().to_string(),
                e["by"].as_str().map(str::to_string),
            )
        })
        .collect()
}

/// A `202` files the ask with exactly one birth event, `asked` by the human.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn accepted_ask_is_born_with_an_asked_event() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let qid = accepted_id(&ask(home.path(), &[]));
    let spool = Spool::new(home.path()).unwrap();
    assert_eq!(
        events_of(&spool, Dir::Asks, &qid),
        [("asked".to_string(), Some("human".to_string()))],
        "the asks/ record carries exactly its birth event"
    );
    b.running.shutdown();
}

/// A `200` answers in the same call, so the `done/` question record is born with
/// `asked` then `answer-received`, and the answer it stored carries `answer-received` too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answered_ask_is_born_with_asked_then_answer_received() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", manual())]).await;
    let hash = question_hash(PROJECT, Some(PATH), QUESTION);
    let c = id(3);
    let earlier = Payload::question(&fp(&c), &fp(&b.id), PROJECT, Some(PATH), QUESTION);
    let ans = Payload::answer(&earlier, "From the responder cache.", "fake", 1, false);
    let env = Envelope::sign(&ans, &b.id);
    b.spool()
        .cache_put(&hash, &record(&env, "pending"))
        .unwrap();

    let home = asker_home(&a, &b.id, &[&b.addr.to_string()]);
    let out = ask(home.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let spool = Spool::new(home.path()).unwrap();
    let done = ids(&spool, Dir::Done);
    assert_eq!(done.len(), 1, "the question is filed as answered");
    assert_eq!(
        events_of(&spool, Dir::Done, &done[0]),
        [
            ("asked".to_string(), Some("human".to_string())),
            ("answer-received".to_string(), None),
        ]
    );
    // `pull::store_answer` writes the same birth event on the answer it spooled.
    assert_eq!(
        events_of(&spool, Dir::Inbox, &ans.id),
        [("answer-received".to_string(), None)]
    );
    b.running.shutdown();
}

// ---- The inline-200 path — a peer-chosen answer id must not collide -------------

/// Serves `env` as the `200` inline answer to every question (the responder-cache shape of
/// `inline_answer_caches_only_unthreaded_questions`).
async fn inline_200_peer(a: &Identity, env: &Envelope) -> FakePeer {
    spawn_fake(
        2,
        a,
        FakeScript {
            accept: Some((200, env.raw.clone())),
            accept_headers: vec![("x-owl-signature".to_string(), env.sig.clone())],
            ..well_formed()
        },
    )
    .await
}

/// The peer answered inline with a `200`, but the answer's id (1) equals a record
/// already in the asker's done/, (2) equals its own `in_reply_to` — not in the asker's spool,
/// the only place rule (c) is observable — or (3) starts with `-`: `owl ask` exits non-zero,
/// stderr names the collision, and the asker's whole home is byte-identical.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inline_answer_with_a_colliding_id_is_refused() {
    let a = id(1);
    let b = id(2);
    let c = id(3);
    let earlier = Payload::question(&fp(&c), &fp(&b), PROJECT, Some(PATH), QUESTION);

    // (1) The answer's id equals a record already in the asker's done/.
    let ans = Payload::answer(&earlier, ANSWER, "fake", 1, false);
    let env = Envelope::sign(&ans, &b);
    let peer = inline_200_peer(&a, &env).await;
    let home = home_for(&a, &peer);
    // A finished ask under the answer's id: another record, not this answer.
    let done_env = Envelope::sign(&earlier, &c);
    Spool::new(home.path())
        .unwrap()
        .put(Dir::Done, &ans.id, &record(&done_env, "answered"))
        .unwrap();
    let before = snapshot(home.path(), &[]);
    let out = ask_bounded(home.path(), &[], BOUND);
    assert_ne!(out.status.code(), Some(0), "(1) {}", stdout(&out));
    let err = stderr(&out);
    assert!(
        err.contains(&format!(
            "answer {} collides with a record here; not stored",
            ans.id
        )),
        "(1) {err}"
    );
    assert_eq!(
        snapshot(home.path(), &[]),
        before,
        "(1) the asker's whole home is byte-identical"
    );
    peer.shutdown();

    // (2) The answer's id equals its own `in_reply_to`, an id NOT in the asker's spool.
    let mut ans = Payload::answer(&earlier, ANSWER, "fake", 1, false);
    ans.id = earlier.id.clone();
    assert_eq!(ans.in_reply_to.as_deref(), Some(earlier.id.as_str()));
    let env = Envelope::sign(&ans, &b);
    let peer = inline_200_peer(&a, &env).await;
    let home = home_for(&a, &peer);
    let before = snapshot(home.path(), &[]);
    let out = ask_bounded(home.path(), &[], BOUND);
    assert_ne!(out.status.code(), Some(0), "(2) {}", stdout(&out));
    let err = stderr(&out);
    assert!(
        err.contains("collides with a record here; not stored"),
        "(2) {err}"
    );
    assert_eq!(
        snapshot(home.path(), &[]),
        before,
        "(2) the asker's whole home is byte-identical"
    );
    peer.shutdown();

    // (3) The answer's id starts with `-`.
    let mut ans = Payload::answer(&earlier, ANSWER, "fake", 1, false);
    ans.id = "--help".into();
    let env = Envelope::sign(&ans, &b);
    let peer = inline_200_peer(&a, &env).await;
    let home = home_for(&a, &peer);
    let before = snapshot(home.path(), &[]);
    let out = ask_bounded(home.path(), &[], BOUND);
    assert_ne!(out.status.code(), Some(0), "(3) {}", stdout(&out));
    let err = stderr(&out);
    // Refused earlier: an answer id must be a canonical UUID.
    assert!(err.contains("is not a UUID"), "(3) {err}");
    assert_eq!(
        snapshot(home.path(), &[]),
        before,
        "(3) the asker's whole home is byte-identical"
    );
    peer.shutdown();
}

/// The inline `200` answer is already in the asker's inbox (same raw and sig, e.g.
/// seen) — it is not rewritten; the run exits 0, prints the answer, and the only change in
/// the home is the `done/` ask record (`answered`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inline_answer_already_stored_is_not_rewritten() {
    let a = id(1);
    let b = id(2);
    let c = id(3);
    let earlier = Payload::question(&fp(&c), &fp(&b), PROJECT, Some(PATH), QUESTION);
    let ans = Payload::answer(&earlier, ANSWER, "fake", 1, false);
    let env = Envelope::sign(&ans, &b);
    let peer = inline_200_peer(&a, &env).await;
    let home = home_for(&a, &peer);
    let spool = Spool::new(home.path()).unwrap();
    let mut stored = record(&env, "pending");
    stored.seen = true;
    spool.put(Dir::Inbox, &ans.id, &stored).unwrap();

    let inbox_bytes = std::fs::read(spool.path(Dir::Inbox, &ans.id)).unwrap();
    let before = snapshot(home.path(), &[]);
    let out = ask_bounded(home.path(), &[], BOUND);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), ANSWER);
    assert_eq!(
        std::fs::read(spool.path(Dir::Inbox, &ans.id)).unwrap(),
        inbox_bytes,
        "the inbox record is byte-identical: not rewritten"
    );
    // The full home diff is exactly the done/ ask record, state `answered`.
    let qid = peer.questions()[0].id.clone();
    let after = snapshot(home.path(), &[]);
    let added: Vec<_> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    assert_eq!(
        added,
        [&std::path::PathBuf::from(format!("spool/done/{qid}.json"))],
        "nothing else was added"
    );
    for (k, v) in &before {
        assert_eq!(after.get(k), Some(v), "{k:?} changed");
    }
    assert_eq!(
        spool.get(Dir::Done, &qid).unwrap().unwrap().state,
        "answered"
    );
    peer.shutdown();
}

/// The inline `200` answer's id carries a path — `../done/<D>` names a finished ask
/// of the asker's through inbox/, `../../escape` a file next to the spool. Not a record id:
/// `owl ask` exits non-zero and the asker's whole home is byte-identical (done/D kept, no
/// `escape.json`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inline_answer_id_carrying_a_path_is_refused() {
    let (a, b, c) = (id(1), id(2), id(3));
    let earlier = Payload::question(&fp(&c), &fp(&b), PROJECT, Some(PATH), QUESTION);
    let finished = Payload::question(&fp(&a), &fp(&b), PROJECT, Some(PATH), "finished");
    for bad in [
        format!("../done/{}", finished.id),
        "../../escape".to_string(),
    ] {
        let mut ans = Payload::answer(&earlier, ANSWER, "fake", 1, false);
        ans.id = bad.clone();
        let env = Envelope::sign(&ans, &b);
        let peer = inline_200_peer(&a, &env).await;
        let home = home_for(&a, &peer);
        Spool::new(home.path())
            .unwrap()
            .put(
                Dir::Done,
                &finished.id,
                &record(&Envelope::sign(&finished, &a), "answered"),
            )
            .unwrap();
        let before = snapshot(home.path(), &[]);
        let out = ask_bounded(home.path(), &[], BOUND);
        assert_ne!(out.status.code(), Some(0), "{bad}: {}", stdout(&out));
        let err = stderr(&out);
        // Refused earlier: an answer id must be a canonical UUID.
        assert!(err.contains("is not a UUID"), "{bad}: {err}");
        assert_eq!(
            snapshot(home.path(), &[]),
            before,
            "{bad}: the asker's whole home is byte-identical"
        );
        assert!(!home.path().parent().unwrap().join("escape.json").exists());
        peer.shutdown();
    }
}

/// A peer that answers every question inline with an answer id made by `make_id` from the
/// question's id (`in_reply_to` unrelated), and that records, per question, whether the
/// asker's home already held the question's id when it arrived.
async fn echo_peer(a: &Identity, make_id: fn(&str) -> String) -> (FakePeer, TempDir) {
    let peer = spawn_fake(
        2,
        a,
        FakeScript {
            inline_answer_id: Some(make_id),
            ..well_formed()
        },
    )
    .await;
    let home = home_for(a, &peer);
    *peer.state.asker_home.lock().unwrap() = Some(home.path().to_path_buf());
    (peer, home)
}

/// The inline `200` answer carries the asker's OWN question id — or its upper-case
/// variant — as its id, with `in_reply_to` pointing elsewhere. The question is reserved in
/// `asks/` before it is sent, so that id names a record: refused, exit non-zero, and the
/// asker's whole home is byte-identical (no inbox record, no done/ ask, the reservation
/// gone).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inline_answer_under_the_question_own_id_is_refused() {
    let a = id(1);
    for what in ["same", "upper"] {
        let make_id: fn(&str) -> String = if what == "upper" {
            |q| q.to_ascii_uppercase()
        } else {
            |q| q.to_string()
        };
        let (peer, home) = echo_peer(&a, make_id).await;
        let before = snapshot(home.path(), &[]);
        let out = ask_bounded(home.path(), &[], BOUND);
        assert_ne!(out.status.code(), Some(0), "{what}: {}", stdout(&out));
        let qs = peer.questions();
        assert_eq!(qs.len(), 1, "{what}");
        let aid = make_id(&qs[0].id);
        assert_ne!(aid == qs[0].id, what == "upper", "{what}: the fixture's id");
        let err = stderr(&out);
        // The upper-case variant is refused earlier: not a canonical UUID.
        let want = if what == "upper" {
            format!("answer id {aid:?} is not a UUID")
        } else {
            format!("answer {aid} collides with a record here; not stored")
        };
        assert!(err.contains(&want), "{what}: {err}");
        assert_eq!(
            snapshot(home.path(), &[]),
            before,
            "{what}: the asker's whole home is byte-identical"
        );
        assert_eq!(*peer.state.taken_on_arrival.lock().unwrap(), [true]);
        peer.shutdown();
    }
}

/// The ask is in `asks/` while the question travels — the peer finds its id taken
/// in the asker's home on arrival — for a `202` (kept: the open ask), an ordinary inline
/// `200` (moved on: `done/` holds the ask, `asks/` is empty) and a `403` (taken back: the
/// home is byte-identical).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ask_is_reserved_while_the_question_travels() {
    let a = id(1);
    // 202: the reservation is the open ask.
    let peer = spawn_fake(2, &a, well_formed()).await;
    let home = home_for(&a, &peer);
    *peer.state.asker_home.lock().unwrap() = Some(home.path().to_path_buf());
    let out = ask_bounded(home.path(), &[], BOUND);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let qid = peer.questions()[0].id.clone();
    assert_eq!(*peer.state.taken_on_arrival.lock().unwrap(), [true]);
    let spool = Spool::new(home.path()).unwrap();
    let open = spool.get(Dir::Asks, &qid).unwrap().expect("the open ask");
    assert_eq!(open.state, "waiting");
    assert_eq!(
        open.raw,
        serde_json::to_string(&peer.questions()[0]).unwrap()
    );
    peer.shutdown();

    // 200 with an ordinary answer id: done/ holds the ask, asks/ is empty.
    let (peer, home) = echo_peer(&a, |_| uuid::Uuid::now_v7().to_string()).await;
    let out = ask_bounded(home.path(), &[], BOUND);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), ANSWER);
    let qid = peer.questions()[0].id.clone();
    assert_eq!(*peer.state.taken_on_arrival.lock().unwrap(), [true]);
    let spool = Spool::new(home.path()).unwrap();
    assert_eq!(
        spool.get(Dir::Done, &qid).unwrap().unwrap().state,
        "answered"
    );
    assert!(spool.list(Dir::Asks, |_| true).unwrap().is_empty());
    peer.shutdown();

    // 403: nothing recorded, the reservation is gone again.
    let peer = spawn_fake(
        2,
        &a,
        FakeScript {
            accept: Some((403, json!({ "error": "unavailable" }).to_string())),
            ..well_formed()
        },
    )
    .await;
    let home = home_for(&a, &peer);
    *peer.state.asker_home.lock().unwrap() = Some(home.path().to_path_buf());
    let before = snapshot(home.path(), &[]);
    let out = ask_bounded(home.path(), &[], BOUND);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!(*peer.state.taken_on_arrival.lock().unwrap(), [true]);
    assert_eq!(snapshot(home.path(), &[]), before, "nothing recorded");
    peer.shutdown();
}

/// An inline `200` answer id longer than `MAX_ID_LEN` (128) is refused — exit
/// non-zero, the asker's home byte-identical, no raw I/O error. Exactly 128
/// characters is no UUID, so it is refused too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inline_answer_id_longer_than_128_is_refused() {
    let a = id(1);
    let longs: [fn(&str) -> String; 3] = [
        |_| "a".repeat(129),
        |_| "a".repeat(300),
        |_| "a".repeat(5000),
    ];
    for make_id in longs {
        let (peer, home) = echo_peer(&a, make_id).await;
        let aid = make_id("");
        let before = snapshot(home.path(), &[]);
        let out = ask_bounded(home.path(), &[], BOUND);
        assert_ne!(out.status.code(), Some(0), "{}", stdout(&out));
        let err = stderr(&out);
        // Refused earlier: an answer id must be a canonical UUID.
        assert!(err.contains("is not a UUID"), "{}: {err}", aid.len());
        assert!(!err.contains("too long"), "{err}");
        assert_eq!(snapshot(home.path(), &[]), before, "{}", aid.len());
        peer.shutdown();
    }

    // Exactly 128 characters is no UUID either — refused, nothing stored.
    let (peer, home) = echo_peer(&a, |_| "c".repeat(128)).await;
    let before = snapshot(home.path(), &[]);
    let out = ask_bounded(home.path(), &[], BOUND);
    assert_ne!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(stderr(&out).contains("is not a UUID"), "{}", stderr(&out));
    assert_eq!(snapshot(home.path(), &[]), before, "128");
    peer.shutdown();
}
