//! `owl presence` and `owl ping` against the real binary with a temp home.
//! `presence` reads `daemon.status` fixtures written with `pull::write_status`; `ping`
//! probes a spawned in-process daemon (online), a closed port (offline) and an unknown peer.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use common::{Peer, fp, id, prepare_home_with, write_contact_full};
use owlpost::contacts::ContactBook;
use owlpost::envelope;
use owlpost::identity::Identity;
use owlpost::pull::{self, PeerProbe, PullStatus};
use serde_json::{Value, json};
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");

// ------------------------------------------------------------------ fixture home

struct Home {
    dir: TempDir,
    me: Identity,
    anna: Identity,
    brooke: Identity,
    czarek: Identity,
}

impl Home {
    /// A home whose book knows three contacts (no endpoints yet) and whose identity exists
    /// (`ping` needs it for the pinned client certificate).
    fn new() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let (me, anna, brooke, czarek) = (id(11), id(12), id(13), id(14));
        prepare_home_with(
            dir.path(),
            &me,
            &[
                Peer::new(&anna, "Anna", None),
                Peer::new(&brooke, "Brooke", None),
                Peer::new(&czarek, "Czarek", None),
            ],
            |_| {},
        );
        Home {
            dir,
            me,
            anna,
            brooke,
            czarek,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(OWL)
            .env_remove("OWLPOST_HOME")
            .arg("--home")
            .arg(self.path())
            .current_dir(self.path())
            .args(args)
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

// ------------------------------------------------------------------ presence

/// The exact `--json` object: one row per contact in the book's order, the never-probed
/// contact all null, a probe entry for a non-contact not listed; the human first line has
/// the counts.
#[test]
fn presence_lists_the_book_against_daemon_status() {
    let h = Home::new();
    pull::write_status(
        h.path(),
        &PullStatus {
            last_pull_at: "2026-10-04T10:00:00Z".into(),
            open_asks: 2,
            peers_probed: 1,
            peers: BTreeMap::from([
                (
                    fp(&h.anna),
                    PeerProbe {
                        online: true,
                        probed_at: "2026-10-04T10:00:00Z".into(),
                        last_seen: Some("2026-10-04T10:00:00Z".into()),
                    },
                ),
                (
                    fp(&h.brooke),
                    PeerProbe {
                        online: false,
                        probed_at: "2026-10-04T10:00:05Z".into(),
                        last_seen: None,
                    },
                ),
                // Probed, but not a contact: never listed.
                (
                    "owl:stranger".to_string(),
                    PeerProbe {
                        online: true,
                        probed_at: "2026-10-04T10:00:06Z".into(),
                        last_seen: Some("2026-10-04T10:00:06Z".into()),
                    },
                ),
            ]),
        },
    )
    .unwrap();

    let (code, out, err) = h.run(&["presence", "--json"]);
    assert_eq!(code, 0, "{err}");
    let got: Value = serde_json::from_str(&out).unwrap();
    let book = ContactBook::load(h.path(), h.path()).unwrap();
    assert_eq!(book.contacts.len(), 3);
    let expected_peers: Vec<Value> = book
        .contacts
        .iter()
        .map(|c| match c.name.as_str() {
            "Anna" => json!({
                "fingerprint": c.fingerprint,
                "name": "Anna",
                "online": true,
                "probed_at": "2026-10-04T10:00:00Z",
                "last_seen": "2026-10-04T10:00:00Z",
            }),
            "Brooke" => json!({
                "fingerprint": c.fingerprint,
                "name": "Brooke",
                "online": false,
                "probed_at": "2026-10-04T10:00:05Z",
                "last_seen": null,
            }),
            name => {
                assert_eq!(name, "Czarek");
                json!({
                    "fingerprint": fp(&h.czarek),
                    "name": "Czarek",
                    "online": null,
                    "probed_at": null,
                    "last_seen": null,
                })
            }
        })
        .collect();
    assert_eq!(
        got,
        json!({
            "last_pull_at": "2026-10-04T10:00:00Z",
            "open_asks": 2,
            "peers_probed": 1,
            "peers": expected_peers,
        })
    );

    let (code, out, err) = h.run(&["presence"]);
    assert_eq!(code, 0, "{err}");
    let first = out.lines().next().expect("a summary line");
    assert!(first.starts_with("last pull "), "{first}");
    assert!(first.contains("2 open ask(s)"), "{first}");
    assert!(first.contains("1 peer(s) probed"), "{first}");
    // The table: header, then one row per contact in the book's order. PROBED is an age
    // measured at print time, so only its `<age> ago` shape is pinned; STATUS and LAST SEEN
    // are exact.
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1 + 1 + 3, "{out}");
    let header = lines[1];
    for col in ["PEER", "STATUS", "PROBED", "LAST SEEN"] {
        assert!(header.contains(col), "header: {header}");
    }
    let expected: Vec<(&str, &str, &str)> = book
        .contacts
        .iter()
        .map(|c| match c.name.as_str() {
            "Anna" => ("Anna", "online", "2026-10-04T10:00:00Z"),
            "Brooke" => ("Brooke", "offline", "-"),
            _ => ("Czarek", "-", "-"),
        })
        .collect();
    for (row, (name, status, last_seen)) in lines[2..].iter().zip(&expected) {
        let tokens: Vec<&str> = row.split_whitespace().collect();
        assert_eq!(tokens[0], *name, "{row}");
        assert_eq!(tokens[1], *status, "{row}");
        if *status == "-" {
            assert_eq!(tokens, [*name, "-", "-", "-"], "{row}");
        } else {
            assert_eq!(tokens.len(), 5, "{row}");
            assert_eq!(tokens[3], "ago", "{row}");
            assert_eq!(tokens[4], *last_seen, "{row}");
        }
    }
}

#[test]
fn presence_without_daemon_status_is_exit_4() {
    let h = Home::new();
    let (code, _, err) = h.run(&["presence"]);
    assert_eq!(code, 4, "{err}");
    assert!(
        err.contains(
            "no daemon status yet (daemon.status missing) — is the daemon running? see owl install"
        ),
        "{err}"
    );
}

/// A status file written before the `peers` key existed: the exact `--json` object, every
/// contact row all-null.
#[test]
fn presence_reads_an_old_status_without_peers() {
    let h = Home::new();
    std::fs::write(
        h.path().join(pull::STATUS_FILE),
        r#"{"last_pull_at":"2026-10-04T10:00:00Z","open_asks":1,"peers_probed":0}"#,
    )
    .unwrap();
    let (code, out, err) = h.run(&["presence", "--json"]);
    assert_eq!(code, 0, "{err}");
    let got: Value = serde_json::from_str(&out).unwrap();
    let book = ContactBook::load(h.path(), h.path()).unwrap();
    let expected_peers: Vec<Value> = book
        .contacts
        .iter()
        .map(|c| {
            json!({
                "fingerprint": c.fingerprint,
                "name": c.name,
                "online": null,
                "probed_at": null,
                "last_seen": null,
            })
        })
        .collect();
    assert_eq!(
        got,
        json!({
            "last_pull_at": "2026-10-04T10:00:00Z",
            "open_asks": 1,
            "peers_probed": 0,
            "peers": expected_peers,
        })
    );
}

// ------------------------------------------------------------------ ping

/// Anna's daemon is up and her contact points at it: exit 0, online, no `error` key.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ping_online() {
    let h = Home::new();
    // Seed 12 is Anna: the spawned daemon IS the peer, and it knows us (mTLS admission).
    let anna = common::spawn_daemon_with(12, &[Peer::new(&h.me, "Me", None)], |_| {}).await;
    write_contact_full(
        h.path(),
        &Peer::new(&h.anna, "Anna", None),
        &[&anna.addr.to_string()],
        &[],
    );
    let (code, out, err) = h.run(&["ping", "anna", "--json"]);
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let keys: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["fingerprint", "name", "online", "probed_at"]
            .into_iter()
            .collect(),
        "{v}"
    );
    assert_eq!(v["fingerprint"], fp(&h.anna));
    assert_eq!(v["name"], "Anna");
    assert_eq!(v["online"], true);
    assert!(
        v["probed_at"].as_str().is_some_and(|t| t.ends_with('Z')),
        "{v}"
    );
    let (code, out, err) = h.run(&["ping", "anna"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.trim(), "Anna is online", "{out}");
    anna.running.shutdown();
}

/// Brooke's only endpoint is a closed port and there is no local daemon: the line/object
/// prints first, then exit 2 with the offline error.
#[test]
fn ping_offline_is_exit_2_with_the_error() {
    let h = Home::new();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().to_string();
    drop(l);
    write_contact_full(
        h.path(),
        &Peer::new(&h.brooke, "Brooke", None),
        &[&port],
        &[],
    );
    let (code, out, err) = h.run(&["ping", "brooke", "--json"]);
    assert_eq!(code, 2, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let keys: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["error", "fingerprint", "name", "online", "probed_at"]
            .into_iter()
            .collect(),
        "{v}"
    );
    assert_eq!(v["fingerprint"], fp(&h.brooke));
    assert_eq!(v["name"], "Brooke");
    assert_eq!(v["online"], false);
    // The error carries the client error's own "offline: " prefix; this precondition is
    // what makes the later `!out.contains("offline: offline")` check meaningful.
    assert!(
        v["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("offline: ")),
        "{v}"
    );
    assert!(
        envelope::parse_rfc3339_to_unix(v["probed_at"].as_str().unwrap()).is_some(),
        "probed_at is RFC 3339: {v}"
    );
    assert!(err.contains("offline: Brooke"), "{err}");

    // The human line prints the reason once: the client error's own "offline: " prefix is
    // stripped from it.
    let (code, out, err) = h.run(&["ping", "brooke"]);
    assert_eq!(code, 2, "{err}");
    assert!(out.starts_with("Brooke is offline: "), "{out}");
    assert!(!out.contains("offline: offline"), "{out}");
}

#[test]
fn ping_unknown_peer_is_exit_1() {
    let h = Home::new();
    let (code, _, err) = h.run(&["ping", "nobody"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("no contact matches"), "{err}");
}
