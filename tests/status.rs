//! CLI half: `owl status [<id>]` as a subprocess against in-process daemons.
//!
//! A (seed 1) asks; B (seed 2) is a daemon that holds A's question for its owner's consent; C
//! (seed 3) is a peer nobody listens for. Every test has its own temp home, every daemon binds
//! 127.0.0.1:0, and nothing reaches the network or a harness — `owl status` only fetches the
//! peer's A2A Task.

mod common;

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::{Path as AxPath, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use axum_server::Handle;
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use common::{
    PATH, PROJECT, Peer, TestDaemon, fp, id, key, policy, prepare_home_with, question, record,
    spawn_daemon, write_contact_full,
};
use owlpost::contacts::Mode;
use owlpost::envelope::{self, Envelope, Payload};
use owlpost::identity::Identity;
use owlpost::spool::{Dir, Spool};
use serde_json::Value;
use tempfile::TempDir;

const HELD: &str = "Why is the refresh token rotated on every read?";
const CONSENT: &str = "waiting for the owner's consent";

// ---- fixtures ----------------------------------------------------------------------------------

/// `host:port` nobody listens on (bound, read, released).
fn closed_port() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().to_string()
}

/// An endpoint that accepts every connection, counts it and closes it at once: the TLS
/// handshake fails, so the peer reads as `offline`, but unlike a closed port the dials can be
/// counted. The accept loop lives for the rest of the test binary.
fn counting_endpoint() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let dials = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&dials);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(s) => {
                    counter.fetch_add(1, Ordering::SeqCst);
                    drop(s);
                }
                Err(_) => break,
            }
        }
    });
    (addr, dials)
}

// ---- a peer whose Task route fails --------------------------------------------------------
// The real daemon never answers `GET /v1/questions/{id}` with a `5xx`, so the non-offline
// error arm of `owl status` needs a scripted peer: B's pinned mTLS, no route but the Task
// one, a `500` for every fetch, and a server-side counter so no test has to infer the
// client's dialling from timing.

/// The `error` detail the failing peer returns, and what `client::fetch_task` turns it into
/// (`error_message`: the status line plus the JSON `error` field).
const TASK_FAILURE_DETAIL: &str = "the task store is down";
const TASK_FAILURE: &str = "peer returned 500 Internal Server Error: the task store is down";

struct FailingPeer {
    id: Identity,
    addr: SocketAddr,
    fetches: Arc<AtomicUsize>,
    handle: Handle<SocketAddr>,
}

impl FailingPeer {
    /// How many `GET /v1/questions/{id}` this peer has served.
    fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
    fn shutdown(&self) {
        self.handle.shutdown();
    }
}

async fn failing_task(
    State(fetches): State<Arc<AtomicUsize>>,
    AxPath(_id): AxPath<String>,
) -> (StatusCode, Json<Value>) {
    fetches.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": TASK_FAILURE_DETAIL })),
    )
}

/// A peer with `seed`'s identity that admits only `asker`'s key and fails every Task fetch.
async fn spawn_failing_peer(seed: u8, asker: &Identity) -> FailingPeer {
    let id = id(seed);
    let allowed = [key(asker)].into_iter().collect();
    let cfg = owlpost::tls::server_config(&id, allowed, false).unwrap();
    let fetches = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/v1/questions/{id}", get(failing_task))
        .with_state(Arc::clone(&fetches));
    let handle = Handle::new();
    let server = axum_server::bind("127.0.0.1:0".parse().unwrap())
        .acceptor(RustlsAcceptor::new(RustlsConfig::from_config(cfg)))
        .handle(handle.clone());
    tokio::spawn(server.serve(app.into_make_service()));
    let addr = handle.listening().await.expect("failing peer bound");
    FailingPeer {
        id,
        addr,
        fetches,
        handle,
    }
}

/// A's home: key and config, no contacts and no daemon of its own (so `owl status` reaches
/// peers over their endpoints, never over iroh).
fn asker_home(a: &Identity) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    prepare_home_with(dir.path(), a, &[], |_| {});
    dir
}

/// A contact for `peer` named `name`, reachable at `endpoint`.
fn contact(
    home: &Path,
    peer: &Identity,
    name: &str,
    endpoint: &str,
    policy: Option<owlpost::contacts::Policy>,
) {
    write_contact_full(home, &Peer::new(peer, name, policy), &[endpoint], &[]);
}

/// A signed question A→`b` filed in `home`'s `asks/` as `owl ask` leaves it on `202`.
fn file_ask(home: &Path, a: &Identity, b: &Identity, text: &str) -> Payload {
    let q = question(a, b, text);
    let env = Envelope::sign(&q, a);
    let mut rec = record(&env, "waiting");
    rec.meta = serde_json::json!({
        "peer": fp(b),
        "hash": envelope::question_hash(PROJECT, Some(PATH), text),
    });
    Spool::new(home)
        .unwrap()
        .put(Dir::Asks, &q.id, &rec)
        .unwrap();
    q
}

/// Posts the very bytes of a filed ask to `b` over the real HTTP path, so `b` holds the
/// question like any arriving one: `consent`, because `b` has no policy for A yet.
async fn post_ask(b: &TestDaemon, a: &Identity, home: &Path, q: &Payload) {
    let rec = Spool::new(home)
        .unwrap()
        .get(Dir::Asks, &q.id)
        .unwrap()
        .expect("the ask was filed");
    let env = Envelope {
        raw: rec.raw,
        sig: rec.sig,
    };
    let resp = common::post_envelope(&common::client(Some(a), &b.id), b, &env).await;
    assert_eq!(resp.status().as_u16(), 202, "B accepted the question");
    assert_eq!(
        b.spool().get(Dir::Inbox, &q.id).unwrap().unwrap().state,
        "consent",
        "B holds it for the owner's consent"
    );
}

/// A's home with two open asks: one to the online B (held for consent) and one to C, whose
/// endpoint is a closed port.
struct TwoPeers {
    home: TempDir,
    b: TestDaemon,
    qb: Payload,
    qc: Payload,
    c: Identity,
}

async fn two_peers() -> TwoPeers {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", None)]).await;
    let c = id(3);
    let home = asker_home(&a);
    contact(home.path(), &b.id, "Bea", &b.addr.to_string(), None);
    contact(home.path(), &c, "Cy", &closed_port(), None);
    let qb = file_ask(home.path(), &a, &b.id, HELD);
    post_ask(&b, &a, home.path(), &qb).await;
    let qc = file_ask(home.path(), &a, &c, "is anyone there?");
    TwoPeers { home, b, qb, qc, c }
}

// ---- running `owl` -----------------------------------------------------------------------------

/// `owl --home <home> [--json] status [<id>]` run from `home`. `OWLPOST_CLAUDE_HOME` is pinned
/// (through [`common::claude_home`]) so no live Claude session on this machine is ever read.
fn status(home: &Path, args: &[&str]) -> Output {
    common::claude_home();
    let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
    c.env_remove("OWLPOST_HOME")
        .arg("--home")
        .arg(home)
        .current_dir(home)
        .stdin(Stdio::null());
    c.args(args).arg("status");
    // `status`'s own positional id comes after the subcommand.
    c.output().unwrap()
}

fn status_id(home: &Path, id: &str) -> Output {
    common::claude_home();
    Command::new(env!("CARGO_BIN_EXE_owl"))
        .env_remove("OWLPOST_HOME")
        .arg("--home")
        .arg(home)
        .current_dir(home)
        .stdin(Stdio::null())
        .args(["status", id])
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(o: &Output) -> String {
    assert_eq!(o.status.code(), Some(0), "stderr: {}", stderr(o));
    stdout(o)
}

// ---- table parsing -------------------------------------------------------------------------------
// `print_table` pads every cell and joins with two spaces, so a run of two or more spaces
// separates cells while the single spaces inside a STATE text are kept.

fn cells(line: &str) -> Vec<String> {
    line.split("  ")
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .collect()
}

fn header(out: &str) -> Vec<String> {
    cells(out.lines().next().expect("a header line"))
}

/// Every line after the header.
fn data_rows(out: &str) -> Vec<Vec<String>> {
    out.lines().skip(1).map(cells).collect()
}

/// The row whose ID cell is `id`.
fn row_for(out: &str, id: &str) -> Vec<String> {
    data_rows(out)
        .into_iter()
        .find(|r| r.first().is_some_and(|c| c == id))
        .unwrap_or_else(|| panic!("no row for {id} in:\n{out}"))
}

fn state_of(out: &str, id: &str) -> String {
    row_for(out, id)[3].clone()
}

/// The element of `owl --json status`'s array with this `id`.
fn element(out: &str, id: &str) -> Value {
    let v: Value = serde_json::from_str(out).unwrap_or_else(|e| panic!("not JSON ({e}):\n{out}"));
    let arr = v
        .as_array()
        .unwrap_or_else(|| panic!("not an array:\n{out}"));
    arr.iter()
        .find(|e| e["id"] == Value::String(id.to_string()))
        .unwrap_or_else(|| panic!("no element for {id} in:\n{out}"))
        .clone()
}

// ---- tests -----------------------------------------------------------------------------------------

/// One row per open ask — the online peer's own words, `offline` for the one nobody
/// answers for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn table_shows_the_peers_state_and_offline() {
    let f = two_peers().await;
    let out = ok(&status(f.home.path(), &[]));
    assert_eq!(header(&out), ["ID", "PEER", "PATH", "STATE", "SINCE"]);
    assert_eq!(data_rows(&out).len(), 2, "{out}");
    let b_row = row_for(&out, &f.qb.id);
    assert_eq!(b_row[0], f.qb.id);
    assert_eq!(b_row[1], "Bea");
    assert_eq!(b_row[2], PATH);
    assert_eq!(b_row[3], CONSENT);
    assert!(b_row[4].ends_with('s'), "an age in seconds: {b_row:?}");
    let c_row = row_for(&out, &f.qc.id);
    assert_eq!(c_row[1], "Cy");
    assert_eq!(c_row[2], PATH);
    assert_eq!(c_row[3], "offline");
    // Nothing was closed: both asks are still open.
    let spool = Spool::new(f.home.path()).unwrap();
    for q in [&f.qb, &f.qc] {
        assert_eq!(
            spool.get(Dir::Asks, &q.id).unwrap().unwrap().state,
            "waiting"
        );
        assert!(spool.get(Dir::Done, &q.id).unwrap().is_none());
    }
    f.b.running.shutdown();
}

/// `owl status <id>` reports that one ask and no other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_id_prints_one_row() {
    let f = two_peers().await;
    let out = ok(&status_id(f.home.path(), &f.qb.id));
    assert_eq!(header(&out), ["ID", "PEER", "PATH", "STATE", "SINCE"]);
    let rows = data_rows(&out);
    assert_eq!(rows.len(), 1, "{out}");
    assert_eq!(rows[0][0], f.qb.id);
    assert_eq!(rows[0][3], CONSENT);
    assert!(
        !out.contains(&f.qc.id),
        "the other ask is not reported: {out}"
    );
    f.b.running.shutdown();
}

/// `owl --json status` prints the Task objects; an ask without one becomes `{id, peer, error}`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn json_prints_the_task_objects() {
    let f = two_peers().await;
    let out = ok(&status(f.home.path(), &["--json"]));
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v.as_array().map(Vec::len), Some(2), "{out}");
    let task = element(&out, &f.qb.id);
    assert_eq!(task["id"], f.qb.id.as_str());
    assert_eq!(task["status"]["state"], envelope::TASK_STATE_SUBMITTED);
    assert_eq!(task["status"]["message"]["parts"][0]["text"], CONSENT);
    assert_eq!(task["metadata"]["owlpost"]["path"], PATH);
    assert_eq!(
        element(&out, &f.qc.id),
        serde_json::json!({ "id": f.qc.id, "peer": fp(&f.c), "error": "offline" })
    );
    f.b.running.shutdown();
}

/// Nothing open at all: exit 4 and a message that names no id.
#[test]
fn no_open_asks_exits_4() {
    let home = asker_home(&id(1));
    let out = status(home.path(), &[]);
    assert_eq!(out.status.code(), Some(4), "stdout: {}", stdout(&out));
    assert_eq!(stderr(&out).trim(), "owl: no open questions");
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
}

/// An id that is not open, while other asks are: exit 4 and a message that names the id.
#[test]
fn unknown_id_exits_4_naming_the_id() {
    let a = id(1);
    let home = asker_home(&a);
    let b = id(2);
    contact(home.path(), &b, "Bea", &closed_port(), None);
    let open = file_ask(home.path(), &a, &b, "an open question");
    let out = status_id(home.path(), "q-nobody-asked");
    assert_eq!(out.status.code(), Some(4), "stdout: {}", stdout(&out));
    assert_eq!(stderr(&out).trim(), "owl: no open question q-nobody-asked");
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    // The other ask is untouched (and never dialled).
    assert_eq!(
        Spool::new(home.path())
            .unwrap()
            .get(Dir::Asks, &open.id)
            .unwrap()
            .unwrap()
            .state,
        "waiting"
    );
}

/// A `REJECTED` Task is the end of the ask: the peer's words are printed and the record moves
/// to `done/` as `declined`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejected_task_moves_the_ask_to_done_declined() {
    let a = id(1);
    let b = spawn_daemon(2, true, &[Peer::new(&a, "Ana", None)]).await;
    let home = asker_home(&a);
    contact(home.path(), &b.id, "Bea", &b.addr.to_string(), None);
    let q = file_ask(home.path(), &a, &b.id, HELD);
    post_ask(&b, &a, home.path(), &q).await;
    // What `owl deny` does to a held question (`cli::deny::deny_held` → `answer::finish`).
    let bs = b.spool();
    let held = bs.get(Dir::Inbox, &q.id).unwrap().unwrap();
    owlpost::answer::finish(
        &bs,
        &q.id,
        held,
        "denied",
        &[("previous_state", serde_json::json!("consent"))],
        owlpost::events::Ev::by("denied", "human"),
    )
    .unwrap();

    let out = ok(&status(home.path(), &[]));
    assert_eq!(state_of(&out, &q.id), "the owner declined");
    let spool = Spool::new(home.path()).unwrap();
    assert!(
        spool.get(Dir::Asks, &q.id).unwrap().is_none(),
        "the ask left asks/"
    );
    assert_eq!(
        spool.get(Dir::Done, &q.id).unwrap().unwrap().state,
        "declined"
    );
    b.running.shutdown();
}

/// The peer is up but holds no record of the id (its responder is on and its policy for A is
/// not `never`, so this is a real `404`): `not found`, and the ask stays open.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_to_the_peer_shows_not_found() {
    let a = id(1);
    let b = spawn_daemon(
        2,
        true,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
    )
    .await;
    let home = asker_home(&a);
    contact(home.path(), &b.id, "Bea", &b.addr.to_string(), None);
    // Filed locally and never posted: B has never seen this question.
    let q = file_ask(home.path(), &a, &b.id, "a question B never received");

    let out = ok(&status(home.path(), &[]));
    assert_eq!(state_of(&out, &q.id), "not found");
    let spool = Spool::new(home.path()).unwrap();
    assert_eq!(
        spool.get(Dir::Asks, &q.id).unwrap().unwrap().state,
        "waiting"
    );
    assert!(spool.get(Dir::Done, &q.id).unwrap().is_none());
    b.running.shutdown();
}

/// The reachability verdict is per peer, not per ask: two open asks to the same unreachable
/// peer are one dial.
#[test]
fn an_offline_peer_is_dialled_once_for_all_its_asks() {
    let a = id(1);
    let b = id(2);
    let home = asker_home(&a);
    let (endpoint, dials) = counting_endpoint();
    contact(home.path(), &b, "Bea", &endpoint, None);
    let first = file_ask(home.path(), &a, &b, "the first question");
    let second = file_ask(home.path(), &a, &b, "the second question");

    let out = ok(&status(home.path(), &[]));
    assert_eq!(data_rows(&out).len(), 2, "{out}");
    assert_eq!(state_of(&out, &first.id), "offline");
    assert_eq!(state_of(&out, &second.id), "offline");
    assert_eq!(dials.load(Ordering::SeqCst), 1, "one dial for both asks");
    let spool = Spool::new(home.path()).unwrap();
    for q in [&first, &second] {
        assert_eq!(
            spool.get(Dir::Asks, &q.id).unwrap().unwrap().state,
            "waiting"
        );
    }
}

/// An ask to a fingerprint that is in no contact file: nothing to dial, and the ask stays open.
#[test]
fn ask_to_an_unknown_contact_says_so() {
    let a = id(1);
    let stranger = id(9);
    let home = asker_home(&a);
    let q = file_ask(home.path(), &a, &stranger, "who are you?");

    let out = ok(&status(home.path(), &[]));
    let row = row_for(&out, &q.id);
    assert_eq!(row[1], fp(&stranger), "no name to show: the fingerprint");
    assert_eq!(row[3], "unknown contact");
    let spool = Spool::new(home.path()).unwrap();
    assert_eq!(
        spool.get(Dir::Asks, &q.id).unwrap().unwrap().state,
        "waiting"
    );
    assert!(spool.get(Dir::Done, &q.id).unwrap().is_none());
}

/// The other side of the `offline` split: the peer is REACHABLE but its Task route
/// fails. The peer's own error is printed instead of `offline`, the ask stays open, and —
/// unlike `offline` — the verdict is not cached per peer, so every open ask is fetched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failing_task_route_prints_the_error_and_is_not_cached_per_peer() {
    let a = id(1);
    let peer = spawn_failing_peer(4, &a).await;
    let home = asker_home(&a);
    contact(home.path(), &peer.id, "Bea", &peer.addr.to_string(), None);
    let first = file_ask(home.path(), &a, &peer.id, "the first question");
    let second = file_ask(home.path(), &a, &peer.id, "the second question");

    let out = ok(&status(home.path(), &[]));
    assert_eq!(data_rows(&out).len(), 2, "{out}");
    for q in [&first, &second] {
        let row = row_for(&out, &q.id);
        assert_eq!(row[1], "Bea");
        assert_eq!(row[2], PATH);
        assert_eq!(row[3], TASK_FAILURE, "the peer's own error, not `offline`");
    }
    // The behavioural difference from `offline`: no per-peer short circuit, so the second
    // ask was fetched too.
    assert_eq!(peer.fetches(), 2, "one Task fetch per open ask");

    let out = ok(&status(home.path(), &["--json"]));
    for q in [&first, &second] {
        assert_eq!(
            element(&out, &q.id),
            serde_json::json!({ "id": q.id, "peer": fp(&peer.id), "error": TASK_FAILURE })
        );
    }
    assert_eq!(peer.fetches(), 4, "the `--json` run fetched both again");

    // Nothing was closed: an error is not an answer.
    let spool = Spool::new(home.path()).unwrap();
    for q in [&first, &second] {
        assert_eq!(
            spool.get(Dir::Asks, &q.id).unwrap().unwrap().state,
            "waiting"
        );
        assert!(spool.get(Dir::Done, &q.id).unwrap().is_none());
    }
    peer.shutdown();
}

/// `owl status` records the peer's Task state on the still-open ask as one
/// `state-seen` event (`by: "peer"`, `detail.state` and `detail.text`), and a second run with
/// the same state collapses into the first rather than appending a twin.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_records_the_peers_task_state_once() {
    let t = two_peers().await;
    let spool = Spool::new(t.home.path()).unwrap();
    let events = |id: &str| -> Vec<Value> {
        spool.get(Dir::Asks, id).unwrap().unwrap().meta["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    assert!(
        events(&t.qb.id).is_empty(),
        "the filed ask starts with no log"
    );

    let out = ok(&status(t.home.path(), &[]));
    assert!(out.contains(CONSENT), "the peer was reached: {out}");
    let first = events(&t.qb.id);
    assert_eq!(first.len(), 1, "one state-seen per state: {first:?}");
    assert_eq!(first[0]["kind"], "state-seen");
    assert_eq!(first[0]["by"], "peer");
    assert_eq!(first[0]["detail"]["state"], envelope::TASK_STATE_SUBMITTED);
    assert_eq!(first[0]["detail"]["text"], CONSENT);
    // The unreachable peer's ask never grew one.
    assert!(
        events(&t.qc.id).is_empty(),
        "an offline peer reports nothing"
    );

    ok(&status(t.home.path(), &[]));
    assert_eq!(
        events(&t.qb.id).len(),
        1,
        "the same state does not append a second event"
    );
    t.b.running.shutdown();
}
