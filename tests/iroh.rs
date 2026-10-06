//! The iroh transport. Two in-process daemons in temp homes, a plain-HTTP
//! iroh relay on 127.0.0.1:0 started in each test (never n0's relays, no DNS discovery), and
//! `owl ask` as a subprocess reaching the peer through the asker's daemon forward route.
//!
//! Every contact book here has **empty `endpoints`**, so nothing can travel over HTTPS unless
//! a test says so explicitly.

mod common;

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderValue, Method};
use common::*;
use iroh::endpoint::{ApplicationClose, ConnectionError, VarInt};
use owlpost::config::{Config, Harness};
use owlpost::contacts::{ContactBook, Mode};
use owlpost::daemon;
use owlpost::identity::{Identity, pubkey_string};
use owlpost::iroh::{self as owl_iroh, ALPN, CLOSE_UNKNOWN_KEY, Reply};
use owlpost::server::SIGNATURE_HEADER;
use owlpost::spool::{Dir, Spool};

const QUESTION: &str = "Where is the retry policy defined?";
const FAKE_HARNESS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/fake-harness.sh"
);
/// Bound for every wait on another task's or process's write.
const WAIT: Duration = Duration::from_secs(20);

fn owl(home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
    c.env_remove("OWLPOST_HOME")
        .arg("--home")
        .arg(home)
        .current_dir(home)
        .stdin(Stdio::null());
    c
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

/// `owl ask Bea <PATH> <q> --project <PROJECT> <extra…>` from the asker's home.
fn ask_q(home: &Path, q: &str, extra: &[&str]) -> Output {
    owl(home)
        .args(["ask", "Bea", PATH, q, "--project", PROJECT])
        .args(extra)
        .output()
        .unwrap()
}

fn ask(home: &Path) -> Output {
    ask_q(home, QUESTION, &[])
}

fn accepted_id(out: &Output) -> String {
    let line = ok(out);
    line.trim()
        .strip_prefix("accepted ")
        .unwrap_or_else(|| panic!("stdout {line:?}"))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
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

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn relay_config(url: &str) -> impl FnOnce(&mut Config) + '_ {
    move |cfg| cfg.relay_urls = Some(vec![url.to_string()])
}

/// B: responder (when `enabled`) with the fake harness and `projects[PROJECT]` mapped to a
/// temp checkout; `peers` is its contact book.
async fn spawn_responder_with(
    relay_url: Option<&str>,
    peers: &[Peer<'_>],
    checkout: &Path,
    enabled: bool,
) -> TestDaemon {
    let checkout = checkout.to_string_lossy().into_owned();
    spawn_daemon_with(2, peers, |cfg| {
        cfg.responder.enabled = enabled;
        cfg.responder.harness = "fake".into();
        cfg.harnesses.insert(
            "fake".into(),
            Harness {
                cmd: vec![FAKE_HARNESS.into(), "{prompt}".into()],
                answer_path: "raw".into(),
                enabled: true,
                disabled_reason: None,
                env: Default::default(),
                model: None,
            },
        );
        cfg.projects.insert(PROJECT.into(), checkout);
        // `None` = no relay at all (`relay_urls = []`), never the n0 defaults.
        cfg.relay_urls = Some(relay_url.into_iter().map(String::from).collect());
    })
    .await
}

/// `spawn_responder_with` for the common case: enabled, Ana in the book without a policy
/// (questions are held for consent).
async fn spawn_responder(relay_url: Option<&str>, a: &Identity, checkout: &Path) -> TestDaemon {
    spawn_responder_with(relay_url, &[Peer::new(a, "Ana", None)], checkout, true).await
}

/// A: asker daemon (pull every `pull_secs`) with Bea in the book at `endpoints`, and
/// `daemon.addr` written so `owl ask` finds the forward route.
async fn spawn_asker_with(
    relay_url: Option<&str>,
    b: &Identity,
    endpoints: &[&str],
    pull_secs: u64,
) -> TestDaemon {
    let d = spawn_daemon_with(1, &[], |cfg| {
        cfg.name = "Ana".into();
        cfg.responder.enabled = false;
        cfg.pull_interval_secs = pull_secs;
        cfg.relay_urls = Some(relay_url.into_iter().map(String::from).collect());
    })
    .await;
    write_contact_full(
        d.home(),
        &Peer::new(b, "Bea", None),
        endpoints,
        &["bea@example.org"],
    );
    daemon::write_addr_file(d.home(), d.addr).unwrap();
    d
}

/// `spawn_asker_with` pulling every second.
async fn spawn_asker(relay_url: Option<&str>, b: &Identity, endpoints: &[&str]) -> TestDaemon {
    spawn_asker_with(relay_url, b, endpoints, 1).await
}

/// One request from `from`'s own test endpoint to daemon `to` over iroh.
async fn request(
    from: &iroh::Endpoint,
    to: &TestDaemon,
    relay_url: &str,
    method: Method,
    path: &str,
    body: Vec<u8>,
    sig: Option<&str>,
) -> anyhow::Result<Reply> {
    let addr = owl_iroh::peer_addr(
        &pubkey_string(&to.id.verifying_key()),
        Some(&[relay_url.to_string()]),
    )
    .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    if let Some(s) = sig {
        headers.insert(SIGNATURE_HEADER, HeaderValue::from_str(s).unwrap());
    }
    owl_iroh::request(from, addr, method, path, headers, body.into()).await
}

fn error_of(reply: &Reply) -> (u16, String) {
    let v: serde_json::Value = serde_json::from_slice(&reply.body).unwrap_or_default();
    (
        reply.status,
        v.get("error")
            .and_then(|e| e.as_str())
            .unwrap_or_default()
            .to_string(),
    )
}

/// The shared fixture never touches n0's relays or DNS discovery: a plain `spawn_daemon`
/// binds its iroh endpoint with `relay_urls = []`, no relay, no address lookup.
#[tokio::test]
async fn fixture_daemons_have_no_relay_and_no_discovery() {
    let d = spawn_daemon(2, true, &[]).await;
    assert_eq!(Config::load(d.home()).unwrap().relay_urls, Some(vec![]));
    let ep = d.running.iroh();
    assert_eq!(ep.addr().relay_urls().count(), 0);
    assert!(
        ep.address_lookup().unwrap().is_empty(),
        "no public discovery"
    );
    assert_eq!(owl_iroh::home_relay(ep), None);
    assert_eq!(ep.id().as_bytes(), d.id.verifying_key().as_bytes());
    d.running.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn question_and_answer_over_iroh() {
    let (relay_url, _relay) = relay().await;
    let checkout = tempfile::tempdir().unwrap();
    std::fs::write(checkout.path().join("README.md"), "fixture\n").unwrap();
    let (a_id, b_id) = (id(1), id(2));
    let b = spawn_responder(Some(&relay_url), &a_id, checkout.path()).await;
    let a = spawn_asker(Some(&relay_url), &b_id, &[]).await;
    wait_online(&a).await;
    wait_online(&b).await;
    let (a_spool, b_spool) = (a.spool(), b.spool());
    // The premise: neither side knows a host:port for the other.
    let book = ContactBook::load(a.home(), a.home()).unwrap();
    assert!(book.resolve("Bea").unwrap().endpoints.is_empty());
    let book = ContactBook::load(b.home(), b.home()).unwrap();
    assert!(book.resolve("Ana").unwrap().endpoints.is_empty());

    // A asks over iroh (through A's daemon): `accepted <id>`; B holds it for consent.
    let qid = accepted_id(&ask(a.home()));
    assert_eq!(state(&a_spool, Dir::Asks, &qid).as_deref(), Some("waiting"));
    wait_for("the question in B's inbox", || {
        state(&b_spool, Dir::Inbox, &qid).as_deref() == Some("consent")
    });

    // B: allow once, draft with the fake harness, send.
    let out = ok(&owl(b.home())
        .args(["allow", "Ana", "--once"])
        .output()
        .unwrap());
    assert!(out.contains("released 1"), "{out}");
    let out = ok(&owl(b.home())
        .args(["draft", &qid, "--harness", "fake"])
        .output()
        .unwrap());
    assert!(out.contains("retry policy"), "{out}");
    assert_eq!(
        state(&b_spool, Dir::Inbox, &qid).as_deref(),
        Some("drafted")
    );
    let out = ok(&owl(b.home()).args(["send", &qid]).output().unwrap());
    let aid = out
        .lines()
        .find_map(|l| l.strip_prefix("sent "))
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or_else(|| panic!("send stdout {out:?}"))
        .to_string();

    // A's daemon pulls the answer over iroh, stores it, acks it; B moves it to done/.
    wait_for("the answer in A's inbox", || {
        state(&a_spool, Dir::Inbox, &aid).is_some()
    });
    wait_for("A's ask moved to done", || {
        state(&a_spool, Dir::Done, &qid).as_deref() == Some("answered")
    });
    wait_for("B's outbox entry acked into done", || {
        state(&b_spool, Dir::Done, &aid).as_deref() == Some("acked")
    });
    assert!(state(&b_spool, Dir::Outbox, &aid).is_none());
    let rec = a_spool
        .get(Dir::Inbox, &aid)
        .unwrap()
        .expect("answer stored");
    assert!(rec.raw.contains("retry policy"), "{}", rec.raw);
    assert_eq!(rec.meta["peer"], b.fp());

    a.running.shutdown();
    b.running.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_key_is_refused() {
    let (relay_url, _relay) = relay().await;
    let checkout = tempfile::tempdir().unwrap();
    let (a_id, c_id) = (id(1), id(3));
    let b = spawn_responder(Some(&relay_url), &a_id, checkout.path()).await;
    wait_online(&b).await;
    let contacts_dir = b.home().join("contacts");
    let before = std::fs::read_dir(&contacts_dir)
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            (p.clone(), std::fs::read(&p).unwrap())
        })
        .collect::<Vec<_>>();
    assert_eq!(before.len(), 1, "only Ana is known");

    // C is not in B's book. The QUIC handshake completes (iroh authenticates the key), then
    // B closes the connection with the `unknown key` code before accepting any stream.
    let c = owl_iroh::endpoint(&c_id, Some(std::slice::from_ref(&relay_url)))
        .await
        .unwrap();
    let addr = owl_iroh::peer_addr(
        &pubkey_string(&b.id.verifying_key()),
        Some(std::slice::from_ref(&relay_url)),
    )
    .unwrap();
    let conn = tokio::time::timeout(WAIT, c.connect(addr.clone(), ALPN))
        .await
        .expect("dial within the bound")
        .expect("handshake");
    let closed = tokio::time::timeout(WAIT, conn.closed())
        .await
        .expect("closed within the bound");
    assert_eq!(
        closed,
        ConnectionError::ApplicationClosed(ApplicationClose {
            error_code: CLOSE_UNKNOWN_KEY,
            reason: b"unknown key".as_slice().into(),
        })
    );
    assert_eq!(CLOSE_UNKNOWN_KEY, VarInt::from_u32(1));

    // A well-formed question signed by C on a fresh connection: no HTTP response of any
    // status comes back (the router never saw it), only the close reason.
    let q = signed(&c_id, &b.id, "let me in?");
    let err = request(
        &c,
        &b,
        &relay_url,
        Method::POST,
        "/v1/questions",
        q.raw.clone().into_bytes(),
        Some(&q.sig),
    )
    .await
    .expect_err("no request is answered for an unknown key");
    let err = format!("{err:#}");
    assert!(err.contains("unknown key"), "{err}");

    // Nothing was stored or learned: five empty spool dirs, no seen-ids, same contact book.
    let spool = b.spool();
    for dir in Dir::ALL {
        assert!(
            ids(&spool, dir).is_empty(),
            "{} must stay empty",
            dir.name()
        );
    }
    assert!(!b.home().join("seen-ids.txt").exists());
    let after = std::fs::read_dir(&contacts_dir)
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            (p.clone(), std::fs::read(&p).unwrap())
        })
        .collect::<Vec<_>>();
    assert_eq!(after, before, "no TOFU on the iroh path");
    let book = ContactBook::load(b.home(), b.home()).unwrap();
    assert_eq!(book.contacts.len(), 1);
    assert_eq!(book.contacts[0].fingerprint, fp(&a_id));

    // The known key on the same daemon is served (the refusal is about the key, not iroh).
    let a = owl_iroh::endpoint(&a_id, Some(std::slice::from_ref(&relay_url)))
        .await
        .unwrap();
    let reply = request(&a, &b, &relay_url, Method::GET, "/v1/outbox", vec![], None)
        .await
        .unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body.as_ref(), b"[]");
    // The owner-only forward route is not mounted on the iroh listener: 404 for any key.
    let reply = request(
        &a,
        &b,
        &relay_url,
        Method::GET,
        &format!("/v1/local/{}/v1/outbox", fp(&a_id)),
        vec![],
        None,
    )
    .await
    .unwrap();
    assert_eq!(error_of(&reply), (404, "not found".to_string()));
    c.close().await;
    a.close().await;
    b.running.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signature_and_replay_rules_apply_over_iroh() {
    let (relay_url, _relay) = relay().await;
    let (a_id, c_id) = (id(1), id(3));
    // Ana: manual, 3 questions per hour (the same threshold as `tests/server.rs`).
    let b = spawn_daemon_with(
        2,
        &[Peer::new(&a_id, "Ana", Some(policy(Mode::Manual, Some(3))))],
        relay_config(&relay_url),
    )
    .await;
    wait_online(&b).await;
    let a = owl_iroh::endpoint(&a_id, Some(std::slice::from_ref(&relay_url)))
        .await
        .unwrap();
    let post = |raw: Vec<u8>, sig: String| {
        let (a, b, relay_url) = (&a, &b, &relay_url);
        async move {
            request(
                a,
                b,
                relay_url,
                Method::POST,
                "/v1/questions",
                raw,
                Some(&sig),
            )
            .await
            .unwrap()
        }
    };

    // Wrong signature: C signs A's payload → the HTTPS suite's `400 bad signature`.
    let q0 = question(&a_id, &b.id, "q0");
    let raw0 = q0.to_signed_bytes();
    let forged = owlpost::envelope::Envelope::sign(&q0, &c_id).sig;
    assert_eq!(
        error_of(&post(raw0.clone(), forged).await),
        (400, "bad signature".to_string())
    );
    assert!(
        ids(&b.spool(), Dir::Inbox).is_empty(),
        "rejected before any write"
    );
    assert!(!b.home().join("seen-ids.txt").exists());

    // Valid → 202; the same id again → the HTTPS suite's `409 duplicate id`.
    let env0 = signed(&a_id, &b.id, "q0");
    let reply = post(env0.raw.clone().into_bytes(), env0.sig.clone()).await;
    assert_eq!(
        reply.status,
        202,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(
        error_of(&post(env0.raw.clone().into_bytes(), env0.sig.clone()).await),
        (409, "duplicate id".to_string())
    );

    // Rate limit: the 4th distinct question in the hour → `429 rate limited`, Retry-After
    // 1200 s (3/h), exactly as over HTTPS; the 4th is not spooled.
    for i in 1..3 {
        let env = signed(&a_id, &b.id, &format!("q{i}"));
        let reply = post(env.raw.into_bytes(), env.sig).await;
        assert_eq!(reply.status, 202, "question {i}");
    }
    let env3 = signed(&a_id, &b.id, "q3");
    let reply = post(env3.raw.clone().into_bytes(), env3.sig.clone()).await;
    assert_eq!(error_of(&reply), (429, "rate limited".to_string()));
    assert_eq!(
        reply
            .headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok()),
        Some("1200")
    );
    assert_eq!(ids(&b.spool(), Dir::Inbox).len(), 3);
    // A rate-limited id is not burned: still 429, never 409.
    let reply = post(env3.raw.into_bytes(), env3.sig).await;
    assert_eq!(error_of(&reply), (429, "rate limited".to_string()));
    a.close().await;
    b.running.shutdown();
}

/// A TCP listener that records every connection attempt: after the run, `accept` drains
/// the kernel backlog, so a client that connected (and even hung up) is still counted.
struct Decoy(TcpListener);

impl Decoy {
    fn bind() -> Decoy {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        Decoy(l)
    }

    fn addr(&self) -> SocketAddr {
        self.0.local_addr().unwrap()
    }

    fn attempts(&self) -> usize {
        let mut n = 0;
        while self.0.accept().is_ok() {
            n += 1;
        }
        n
    }
}

/// `host:port` nobody listens on (bound, read, released).
fn closed_port() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn iroh_first_then_endpoints() {
    let (relay_url, _relay) = relay().await;
    let checkout = tempfile::tempdir().unwrap();
    let (a_id, b_id) = (id(1), id(2));

    // 1. iroh available, `endpoints` holds one address that records attempts: the question
    //    arrives over iroh and the address is never dialed.
    let decoy = Decoy::bind();
    // The decoy is a live listener: a dial would succeed at TCP level and be counted.
    drop(std::net::TcpStream::connect(decoy.addr()).expect("the decoy accepts connections"));
    assert_eq!(decoy.attempts(), 1, "the counter sees a completed connect");
    let b = spawn_responder(Some(&relay_url), &a_id, checkout.path()).await;
    let a = spawn_asker(Some(&relay_url), &b_id, &[&decoy.addr().to_string()]).await;
    wait_online(&a).await;
    wait_online(&b).await;
    let qid = accepted_id(&ask(a.home()));
    wait_for("the question in B's inbox", || {
        state(&b.spool(), Dir::Inbox, &qid).is_some()
    });
    assert_eq!(decoy.attempts(), 0, "the endpoint was never dialed");
    a.running.shutdown();
    b.running.shutdown();

    // 2. iroh unavailable (no relay on either side: the peer has no address at all) and a
    //    reachable endpoint: `owl ask` succeeds over HTTPS.
    let b = spawn_responder(None, &a_id, checkout.path()).await;
    assert_eq!(
        Config::load(b.home()).unwrap().relay_urls,
        Some(vec![]),
        "no relay: `relay_urls = []`"
    );
    let a = spawn_asker(None, &b_id, &[&b.addr.to_string()]).await;
    assert_eq!(Config::load(a.home()).unwrap().relay_urls, Some(vec![]));
    let qid = accepted_id(&ask(a.home()));
    wait_for("the question in B's inbox", || {
        state(&b.spool(), Dir::Inbox, &qid).is_some()
    });
    a.running.shutdown();
    b.running.shutdown();

    // 3. Both unavailable: offline, in the same wording as before iroh, one reason per
    //    transport (iroh first, then the endpoint).
    let port = closed_port();
    let a = spawn_asker(None, &b_id, &[&port]).await;
    let out = ask(a.home());
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    let err = err.trim();
    // Same wording as before iroh, with the iroh reason (no relay → the peer has no
    // address) first and the endpoint's reason second.
    let prefix = format!(
        "owl: offline: no endpoint of Bea reachable (iroh: dial: No addressing information available; {port}: "
    );
    assert_eq!(&err[..prefix.len().min(err.len())], prefix, "{err}");
    assert!(err.ends_with(')'), "{err}");
    assert!(stdout(&out).is_empty());
    for dir in Dir::ALL {
        assert!(
            ids(&a.spool(), dir).is_empty(),
            "{} must stay empty",
            dir.name()
        );
    }
    a.running.shutdown();
}

/// `owl ask --wait` over iroh: the CLI's own `fetch_outbox` + `ack` go through the forward
/// route (`Iroh::ViaDaemon`); the asker daemon's pull loop is parked (1 h) so only the CLI
/// polls, and the decoy endpoint proves nothing went over HTTPS.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_over_iroh_via_daemon() {
    let (relay_url, _relay) = relay().await;
    let checkout = tempfile::tempdir().unwrap();
    std::fs::write(checkout.path().join("README.md"), "fixture\n").unwrap();
    let (a_id, b_id) = (id(1), id(2));
    let decoy = Decoy::bind();
    let b = spawn_responder(Some(&relay_url), &a_id, checkout.path()).await;
    let a = spawn_asker_with(Some(&relay_url), &b_id, &[&decoy.addr().to_string()], 3600).await;
    wait_online(&a).await;
    wait_online(&b).await;
    let (a_spool, b_spool) = (a.spool(), b.spool());
    let b_home = b.home().to_path_buf();
    let answerer = std::thread::spawn(move || {
        let spool = Spool::new(&b_home).unwrap();
        let start = Instant::now();
        let qid = loop {
            if let Some((qid, rec)) = spool.list(Dir::Inbox, |_| true).unwrap().into_iter().next()
                && rec.state == "consent"
            {
                break qid;
            }
            assert!(start.elapsed() < WAIT, "no question reached B");
            std::thread::sleep(Duration::from_millis(50));
        };
        ok(&owl(&b_home)
            .args(["allow", "Ana", "--once"])
            .output()
            .unwrap());
        ok(&owl(&b_home)
            .args(["draft", &qid, "--harness", "fake"])
            .output()
            .unwrap());
        let out = ok(&owl(&b_home).args(["send", &qid]).output().unwrap());
        let aid = out
            .lines()
            .find_map(|l| l.strip_prefix("sent "))
            .and_then(|l| l.split_whitespace().next())
            .unwrap()
            .to_string();
        (qid, aid)
    });
    let out = ask_q(a.home(), QUESTION, &["--wait", "20"]);
    let (qid, aid) = answerer.join().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("retry policy"), "{}", stdout(&out));
    assert!(
        stderr(&out).contains(&format!("accepted {qid}")),
        "{}",
        stderr(&out)
    );
    assert!(!stderr(&out).contains("warning"), "{}", stderr(&out));
    assert_eq!(
        state(&a_spool, Dir::Done, &qid).as_deref(),
        Some("answered")
    );
    assert!(state(&a_spool, Dir::Inbox, &aid).is_some());
    wait_for("B's outbox acked into done", || {
        state(&b_spool, Dir::Done, &aid).as_deref() == Some("acked")
    });
    assert!(state(&b_spool, Dir::Outbox, &aid).is_none());
    assert_eq!(decoy.attempts(), 0, "HTTPS endpoint never dialed");
    a.running.shutdown();
    b.running.shutdown();
}

/// The peer's own `429` / `403` through the forward route are final: the same exit codes
/// and wording as over HTTPS, and no fallback to the (decoy) endpoint. Only a `502` from
/// the forward route means "try `endpoints`".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_status_via_forward_is_final() {
    let (relay_url, _relay) = relay().await;
    let checkout = tempfile::tempdir().unwrap();
    let (a_id, b_id) = (id(1), id(2));
    let decoy = Decoy::bind();
    let b = spawn_responder_with(
        Some(&relay_url),
        &[Peer::new(&a_id, "Ana", Some(policy(Mode::Manual, Some(1))))],
        checkout.path(),
        true,
    )
    .await;
    let a = spawn_asker_with(Some(&relay_url), &b_id, &[&decoy.addr().to_string()], 3600).await;
    wait_online(&a).await;
    wait_online(&b).await;
    let first = accepted_id(&ask_q(a.home(), QUESTION, &[]));
    let out = ask_q(a.home(), "Second question?", &[]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.trim().starts_with("owl: rate limited, retry after "),
        "{err}"
    );
    assert!(err.trim().ends_with('s'), "{err}");
    let secs: u64 = err
        .split("retry after ")
        .nth(1)
        .and_then(|s| s.trim_end().strip_suffix('s'))
        .and_then(|s| s.parse().ok())
        .unwrap();
    assert!((1..=3600).contains(&secs), "{secs}");
    assert_eq!(ids(&a.spool(), Dir::Asks), [first]);
    assert_eq!(decoy.attempts(), 0, "429 over iroh is final: no HTTPS dial");
    a.running.shutdown();
    b.running.shutdown();

    // Responder disabled → 403 → `unavailable`, still no fallback.
    let decoy = Decoy::bind();
    let b = spawn_responder_with(
        Some(&relay_url),
        &[Peer::new(&a_id, "Ana", None)],
        checkout.path(),
        false,
    )
    .await;
    let a = spawn_asker_with(Some(&relay_url), &b_id, &[&decoy.addr().to_string()], 3600).await;
    wait_online(&a).await;
    wait_online(&b).await;
    let out = ask_q(a.home(), QUESTION, &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        "owl: unavailable: Bea does not answer questions (policy never or responder disabled)"
    );
    assert_eq!(decoy.attempts(), 0, "403 over iroh is final: no HTTPS dial");
    a.running.shutdown();
    b.running.shutdown();
}

/// Over iroh: `GET /v1/questions/{id}` is served on the iroh listener (a direct
/// request from the asker's key) and reached through the daemon's forward route — `owl status`
/// on a home whose contact book holds no `host:port` prints the peer's own words. Another
/// pinned key asking for the same id, and an unknown id, are `404` there too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_route_over_iroh_and_the_forward_route() {
    let (relay_url, _relay) = relay().await;
    let checkout = tempfile::tempdir().unwrap();
    std::fs::write(checkout.path().join("README.md"), "fixture\n").unwrap();
    let (a_id, b_id, c_id) = (id(1), id(2), id(3));
    let b = spawn_responder(Some(&relay_url), &a_id, checkout.path()).await;
    // Cid is pinned on B as well, so the 404 below is about WHOSE question it is, not about
    // an unknown key (which would be refused at the handshake instead).
    write_contact(
        b.home(),
        &Peer::new(&c_id, "Cid", Some(policy(Mode::Manual, None))),
    );
    let a = spawn_asker(Some(&relay_url), &b_id, &[]).await;
    wait_online(&a).await;
    wait_online(&b).await;
    let book = ContactBook::load(a.home(), a.home()).unwrap();
    assert!(
        book.resolve("Bea").unwrap().endpoints.is_empty(),
        "the premise: no host:port for Bea, so everything below travels over iroh"
    );

    let qid = accepted_id(&ask(a.home()));
    wait_for("the question in B's inbox", || {
        state(&b.spool(), Dir::Inbox, &qid).as_deref() == Some("consent")
    });

    // --- through the forward route: `owl status` from A's home.
    let out = ok(&owl(a.home()).args(["status"]).output().unwrap());
    let row = out
        .lines()
        .find(|l| l.starts_with(&qid))
        .unwrap_or_else(|| panic!("no row for {qid} in:\n{out}"));
    assert!(
        row.contains("waiting for the owner's consent"),
        "the peer's own words, fetched over iroh: {row}"
    );

    // --- straight at B's iroh listener, as A.
    let a_ep = owl_iroh::endpoint(&a_id, Some(std::slice::from_ref(&relay_url)))
        .await
        .unwrap();
    let reply = request(
        &a_ep,
        &b,
        &relay_url,
        Method::GET,
        &format!("/v1/questions/{qid}"),
        vec![],
        None,
    )
    .await
    .unwrap();
    assert_eq!(reply.status, 200);
    let task: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(task["id"], qid);
    assert_eq!(task["status"]["state"], "TASK_STATE_SUBMITTED");
    assert_eq!(
        task["status"]["message"]["parts"][0]["text"],
        "waiting for the owner's consent"
    );
    assert_eq!(task["metadata"]["owlpost"]["from"], fp(&a_id));

    // Cid, pinned but not the asker, gets 404 for the very same id.
    let c_ep = owl_iroh::endpoint(&c_id, Some(std::slice::from_ref(&relay_url)))
        .await
        .unwrap();
    let reply = request(
        &c_ep,
        &b,
        &relay_url,
        Method::GET,
        &format!("/v1/questions/{qid}"),
        vec![],
        None,
    )
    .await
    .unwrap();
    assert_eq!(error_of(&reply), (404, "not found".to_string()));

    // An id nobody asked: 404 for the asker too.
    let reply = request(
        &a_ep,
        &b,
        &relay_url,
        Method::GET,
        "/v1/questions/0191c7a0-0000-7000-8000-0000000000ff",
        vec![],
        None,
    )
    .await
    .unwrap();
    assert_eq!(error_of(&reply), (404, "not found".to_string()));

    a_ep.close().await;
    c_ep.close().await;
    a.running.shutdown();
    b.running.shutdown();
}
