//! The `owl mcp` resource URI is a peer. Every command that takes a `<peer>`
//! resolves `@owl:to://…` as Claude Code passes the mention in a slash command's arguments
//!, every URI `resources/list` returns round-trips to the contact `resources/read`
//! names, and the three older forms keep their order. Real binary throughout;
//! `owl ask` / `owl card` run against an in-process daemon on 127.0.0.1:0.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};

use common::{
    PATH, PROJECT, Peer, claude_home, fp, id, policy, prepare_home, spawn_daemon,
    write_contact_full,
};
use owlpost::contacts::Mode;
use serde_json::{Value, json};
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");
/// Ana Kowalska's URI in [`fixture`] — the shape the `@` typeahead inserts.
const ANA_URI: &str = "to://ana-kowalska.ana@acme.com";
const QUESTION: &str = "Why is the refresh token rotated on every read?";

fn owl(home: &Path, args: &[&str]) -> Output {
    claude_home();
    Command::new(OWL)
        .env_remove("OWLPOST_HOME")
        .arg("--home")
        .arg(home)
        .args(args)
        .current_dir(home)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A home owned by seed 9 whose **global** book holds `(seed, name, emails)` per contact.
fn book(contacts: &[(u8, &str, &[&str])]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    prepare_home(dir.path(), &id(9), false, &[]);
    for (seed, name, emails) in contacts {
        write_contact_full(dir.path(), &Peer::new(&id(*seed), name, None), &[], emails);
    }
    dir
}

/// The inconvenient book: a contact with a name and two e-mails, a Polish name
/// with no e-mail (`ł`/`ó` survive the slug), two contacts sharing name **and** e-mail (the
/// collision suffix) and a nameless, e-mail-less one (a stray policy overlay).
fn fixture() -> TempDir {
    book(&[
        (2, "Ana Kowalska", &["ana@acme.com", "ana.k@example.net"]),
        (3, "Zoë Łukasz-Góra", &[]),
        (4, "Bob Smith", &["bob@acme.com"]),
        (5, "Bob Smith", &["bob@acme.com"]),
        (6, "", &[]),
    ])
}

/// The fingerprint segment a collision suffix carries.
fn fp_seg(seed: u8) -> String {
    fp(&id(seed)).strip_prefix("owl:").unwrap().to_string()
}

/// `owl mcp` as a child process over piped stdin/stdout (the protocol side of the URI round trip).
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Mcp {
    fn spawn(home: &Path) -> Mcp {
        claude_home();
        let mut child = Command::new(OWL)
            .arg("mcp")
            .env("OWLPOST_HOME", home)
            .current_dir(home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Mcp {
            child,
            stdin,
            stdout,
        }
    }

    fn result(&mut self, id: u64, method: &str, params: Value) -> Value {
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.stdin.write_all(format!("{req}\n").as_bytes()).unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        assert!(self.stdout.read_line(&mut line).unwrap() > 0, "{method}");
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert!(reply.get("error").is_none(), "{method}: {reply}");
        reply["result"].clone()
    }

    fn finish(mut self) {
        drop(self.stdin);
        assert_eq!(self.child.wait().unwrap().code(), Some(0));
    }
}

/// The contact `owl contact show <query>` prints, or a panic with the command's stderr.
fn shown(home: &Path, query: &str) -> Value {
    let o = owl(home, &["contact", "show", query]);
    assert_eq!(o.status.code(), Some(0), "{query}: {}", stderr(&o));
    serde_json::from_str(&stdout(&o)).unwrap_or_else(|e| panic!("{query}: {e}: {}", stdout(&o)))
}

fn fingerprint_of(home: &Path, query: &str) -> String {
    shown(home, query)["fingerprint"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Every URI `resources/list` hands Claude Code resolves — in both the `@owl:` mention
/// spelling and bare — to the contact `resources/read` reports for it.
#[test]
fn every_mcp_uri_round_trips_through_contact_show() {
    let home = fixture();
    let mut mcp = Mcp::spawn(home.path());
    let list = mcp.result(1, "resources/list", json!({}));
    let uris: Vec<String> = list["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(uris.len(), 5, "{list}");
    assert!(uris.contains(&ANA_URI.to_string()), "{uris:?}");
    assert!(
        uris.contains(&"to://zoë-łukasz-góra".to_string()),
        "the Polish slug: {uris:?}"
    );
    assert!(
        uris.contains(&format!("to://bob-smith.bob@acme.com.{}", fp_seg(4))),
        "the collision suffix: {uris:?}"
    );
    assert!(
        uris.contains(&format!("to://{}", fp_seg(6))),
        "the nameless contact: {uris:?}"
    );
    for (n, uri) in uris.iter().enumerate() {
        let read = mcp.result(2 + n as u64, "resources/read", json!({"uri": uri}));
        let text: Value =
            serde_json::from_str(read["contents"][0]["text"].as_str().unwrap()).unwrap();
        let expected = text["fingerprint"].as_str().unwrap();
        for query in [uri.clone(), format!("@owl:{uri}")] {
            assert_eq!(fingerprint_of(home.path(), &query), expected, "{query}");
        }
    }
    mcp.finish();
}

/// A URI is exact by construction — a wrong collision suffix, the base of a collided
/// URI, any prefix of a valid URI and `to://` alone are all `no contact matches`.
#[test]
fn no_prefix_fallback_on_a_uri() {
    let home = fixture();
    let wrong_suffix = format!("to://bob-smith.bob@acme.com.{}", fp_seg(7));
    let queries = [
        wrong_suffix.as_str(),
        // The collided base: a valid URI's prefix, and the suffix is what tells the two apart.
        "to://bob-smith.bob@acme.com",
        "to://ana-kowalska.ana@acme.p",
        "to://ana-kowalska",
        "@owl:to://ana-kowalska",
        "to://",
        "@owl:to://",
    ];
    for query in queries {
        let o = owl(home.path(), &["contact", "show", query]);
        assert_eq!(o.status.code(), Some(1), "{query}: {}", stdout(&o));
        assert!(
            stderr(&o).contains(&format!("no contact matches {query:?}")),
            "{query}: {}",
            stderr(&o)
        );
    }
}

/// Fingerprint, exact e-mail and unique case-insensitive name prefix keep their
/// behaviour, and an ambiguous prefix still lists the candidates.
#[test]
fn the_older_three_forms_are_untouched() {
    let home = fixture();
    for (query, seed) in [
        (fp(&id(2)), 2u8),
        ("ana.k@example.net".to_string(), 2),
        ("ANA kow".to_string(), 2),
        ("zoë".to_string(), 3),
    ] {
        assert_eq!(
            fingerprint_of(home.path(), &query),
            fp(&id(seed)),
            "{query}"
        );
    }
    let o = owl(home.path(), &["contact", "show", "Bob"]);
    assert_eq!(o.status.code(), Some(1), "{}", stdout(&o));
    assert!(
        stderr(&o).contains("ambiguous peer \"Bob\": matches Bob Smith, Bob Smith"),
        "{}",
        stderr(&o)
    );
}

/// The URI arm sits **before** the name arm — a contact whose *name* is another
/// contact's URI never steals it — and a name that itself starts with `to://` still resolves
/// through the name arm, because a URI miss falls through.
#[test]
fn the_uri_arm_runs_before_the_name_arm_and_falls_through() {
    let impostor = book(&[
        (2, "Ana Kowalska", &["ana@acme.com"]),
        (7, ANA_URI, &["impostor@acme.com"]),
    ]);
    for query in [ANA_URI.to_string(), format!("@owl:{ANA_URI}")] {
        assert_eq!(
            fingerprint_of(impostor.path(), &query),
            fp(&id(2)),
            "{query} must be Ana's URI, not the impostor's name"
        );
    }
    // The impostor is reachable by its own URI (slug of `to://ana-kowalska.ana@acme.com`).
    assert_eq!(
        fingerprint_of(
            impostor.path(),
            "to://to-ana-kowalska-ana-acme-com.impostor@acme.com"
        ),
        fp(&id(7))
    );

    let named = book(&[(8, "to://x", &["x@acme.com"])]);
    assert_eq!(
        fingerprint_of(named.path(), "to://x"),
        fp(&id(8)),
        "no URI matches `to://x`, so the name arm answers"
    );
    assert_eq!(
        fingerprint_of(named.path(), "to://to-x.x@acme.com"),
        fp(&id(8)),
        "and its own URI resolves too"
    );
}

/// `owl contact show` with both spellings prints exactly what the fingerprint prints.
#[test]
fn contact_show_takes_the_uri_in_both_spellings() {
    let home = fixture();
    let by_fp = shown(home.path(), &fp(&id(2)));
    assert_eq!(shown(home.path(), ANA_URI), by_fp);
    assert_eq!(shown(home.path(), &format!("@owl:{ANA_URI}")), by_fp);
}

/// `owl contact remove <uri>` deletes the same contact file the fingerprint would.
#[test]
fn contact_remove_takes_the_uri() {
    let home = fixture();
    let file = home
        .path()
        .join("contacts")
        .join(format!("{}.json", fp(&id(2))));
    assert!(file.exists());
    let o = owl(
        home.path(),
        &["contact", "remove", &format!("@owl:{ANA_URI}")],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o).trim(), "removed Ana Kowalska (global)");
    assert!(!file.exists(), "the contact file is gone");
    let gone = owl(home.path(), &["contact", "show", ANA_URI]);
    assert_eq!(gone.status.code(), Some(1), "{}", stdout(&gone));
}

/// `owl allow <uri>` and `owl deny <uri>` write the policy of the contact the
/// fingerprint names.
#[test]
fn allow_and_deny_take_the_uri() {
    for (cmd, expected) in [("allow", "manual"), ("deny", "never")] {
        let home = fixture();
        let o = owl(home.path(), &[cmd, &format!("@owl:{ANA_URI}"), "--json"]);
        assert_eq!(o.status.code(), Some(0), "{cmd}: {}", stderr(&o));
        let v: Value = serde_json::from_str(&stdout(&o)).unwrap();
        assert_eq!(v["fingerprint"], fp(&id(2)), "{cmd}: {v}");
        assert_eq!(v["peer"], "Ana Kowalska", "{cmd}: {v}");
        assert_eq!(v["policy"], expected, "{cmd}: {v}");
        assert_eq!(
            shown(home.path(), ANA_URI)["policy"]["mode"],
            expected,
            "{cmd}: the policy landed on that contact"
        );
    }
}

/// `owl ask <uri> "<question>"` (the incident's own shape) and
/// `owl ask --file <path> --peer <uri> "<question>"` both reach the peer the fingerprint
/// names; the mention travels as one argument.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ask_takes_the_uri_as_a_positional_and_as_peer() {
    let a = id(1);
    let b = spawn_daemon(
        2,
        true,
        &[Peer::new(&a, "Ana", Some(policy(Mode::Manual, None)))],
    )
    .await;
    let home = tempfile::tempdir().unwrap();
    prepare_home(home.path(), &a, true, &[]);
    write_contact_full(
        home.path(),
        &Peer::new(&b.id, "Bea", None),
        &[&b.addr.to_string()],
        &["bea@example.org"],
    );
    let uri = "@owl:to://bea.bea@example.org";
    std::fs::write(home.path().join("session.rs"), "fn main() {}\n").unwrap();

    let positional = owl(
        home.path(),
        &["ask", uri, QUESTION, "--project", PROJECT, "--no-cache"],
    );
    assert_eq!(positional.status.code(), Some(0), "{}", stderr(&positional));
    assert!(
        stdout(&positional).starts_with("accepted "),
        "{}",
        stdout(&positional)
    );

    let with_flag = owl(
        home.path(),
        &[
            "ask",
            "--file",
            "session.rs",
            "--peer",
            uri,
            QUESTION,
            "--project",
            PROJECT,
            "--no-cache",
        ],
    );
    assert_eq!(with_flag.status.code(), Some(0), "{}", stderr(&with_flag));
    assert!(
        stdout(&with_flag).starts_with("accepted "),
        "{}",
        stdout(&with_flag)
    );

    // Both questions reached Bea's daemon, addressed to her fingerprint.
    let spool = owlpost::spool::Spool::new(home.path()).unwrap();
    let asks = spool.list(owlpost::spool::Dir::Asks, |_| true).unwrap();
    assert_eq!(asks.len(), 2, "{asks:?}");
    for (_, rec) in &asks {
        assert_eq!(rec.meta["peer"], b.fp());
    }
    b.running.shutdown();
}

/// `owl card <uri>` fetches the card of the peer the fingerprint names.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn card_takes_the_uri() {
    let a = id(1);
    let b = spawn_daemon(2, false, &[Peer::new(&a, "Ana", None)]).await;
    let home = tempfile::tempdir().unwrap();
    prepare_home(home.path(), &a, true, &[]);
    write_contact_full(
        home.path(),
        &Peer::new(&b.id, "Bea", None),
        &[&b.addr.to_string()],
        &["bea@example.org"],
    );
    for query in ["@owl:to://bea.bea@example.org", "to://bea.bea@example.org"] {
        let o = owl(home.path(), &["card", query]);
        assert_eq!(o.status.code(), Some(0), "{query}: {}", stderr(&o));
        let card: Value = serde_json::from_str(&stdout(&o)).unwrap();
        assert_eq!(card["name"], "Bea", "{query}: {card}");
        assert!(
            stdout(&o).contains(&b.fp()),
            "{query}: the card is Bea's: {}",
            stdout(&o)
        );
    }
    b.running.shutdown();
}

/// A path is not part of the URI: the positional `[path]` still works behind a mention.
#[test]
fn a_uri_positional_leaves_the_path_alone() {
    let home = fixture();
    // `owl ask` needs a reachable peer, so this row only pins the parse: an unreachable
    // contact is exit 2 `offline`, never `no contact matches`.
    let o = owl(
        home.path(),
        &[
            "ask",
            &format!("@owl:{ANA_URI}"),
            PATH,
            QUESTION,
            "--project",
            PROJECT,
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{}: {}", stdout(&o), stderr(&o));
    assert!(!stderr(&o).contains("no contact matches"), "{}", stderr(&o));
}
