//! `owl mcp` is driven as a real child process over piped stdin/stdout on a
//! seeded `$OWLPOST_HOME` (global contacts) plus a fixture repo with `.agents/peers/` (local
//! scope): the handshake, the exact resource URI list, `resources/read`, the error codes, and
//! the per-request reload of the book.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use common::{Peer, fp, id, prepare_home, write_contact_full};
use owlpost::identity::pubkey_string;
use serde_json::{Value, json};
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn spawn(home: &Path, cwd: &Path) -> Server {
        let mut child = Command::new(OWL)
            .arg("mcp")
            .env("OWLPOST_HOME", home)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Server {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, line: &str) {
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }

    /// One reply line, parsed.
    fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "server closed stdout");
        assert!(
            line.ends_with('\n'),
            "reply must be newline-terminated: {line:?}"
        );
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("reply is not JSON: {e}: {line}"))
    }

    fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
        );
        let reply = self.recv();
        assert_eq!(reply["jsonrpc"], "2.0", "{reply}");
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    /// `call` that must succeed; returns `result`.
    fn result(&mut self, id: u64, method: &str, params: Value) -> Value {
        let reply = self.call(id, method, params);
        assert!(reply.get("error").is_none(), "{method}: {reply}");
        reply["result"].clone()
    }

    /// `call` that must fail; returns the error code.
    fn error_code(&mut self, id: u64, method: &str, params: Value) -> i64 {
        let reply = self.call(id, method, params);
        assert!(reply.get("result").is_none(), "{method}: {reply}");
        reply["error"]["code"].as_i64().unwrap()
    }

    fn uris(&mut self, id: u64) -> Vec<String> {
        self.result(id, "resources/list", json!({}))["resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["uri"].as_str().unwrap().to_string())
            .collect()
    }

    /// Closes stdin (EOF) and returns the exit code.
    fn finish(mut self) -> Option<i32> {
        drop(self.stdin);
        self.child.wait().unwrap().code()
    }
}

/// Global book: Ana (1), Zoë (2), "Dr. Nomail (QA)" (3, no e-mail: punctuation runs collapse
/// to one `-`, the trailing `)` is trimmed), Bob (4); the fixture repo's
/// `.agents/peers/bob.json` is a second Bob (5) with the same e-mail — the URI collision.
fn fixture() -> (TempDir, TempDir) {
    let home = tempfile::tempdir().unwrap();
    prepare_home(home.path(), &id(9), false, &[]);
    for (seed, name, emails) in [
        (
            1u8,
            "Ana Kowalska",
            vec!["ana@acme.com", "ana.k@example.net"],
        ),
        (2, "Zoë O'Brien-Łukasz", vec!["zoe@acme.com"]),
        (3, "Dr. Nomail (QA)", vec![]),
        (4, "Bob Smith", vec!["bob@acme.com"]),
    ] {
        write_contact_full(home.path(), &Peer::new(&id(seed), name, None), &[], &emails);
    }
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join(".git")).unwrap();
    let peers = repo.path().join(".agents/peers");
    std::fs::create_dir_all(&peers).unwrap();
    std::fs::write(
        peers.join("bob.json"),
        json!({
            "name": "Bob Smith",
            "emails": ["bob@acme.com"],
            "pubkey": pubkey_string(&id(5).verifying_key()),
        })
        .to_string(),
    )
    .unwrap();
    (home, repo)
}

fn seg(seed: u8) -> String {
    fp(&id(seed)).strip_prefix("owl:").unwrap().to_string()
}

/// Handshake, notification ignored, ping, exit 0 on EOF.
#[test]
fn handshake_ping_and_eof() {
    let (home, repo) = fixture();
    let mut s = Server::spawn(home.path(), repo.path());
    let init = s.result(
        1,
        "initialize",
        json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
    );
    assert_eq!(init["protocolVersion"], "2024-11-05", "echoed: {init}");
    assert_eq!(init["serverInfo"]["name"], "owl");
    assert_eq!(init["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(init["capabilities"]["resources"].is_object(), "{init}");
    // The notification gets no reply: the next line read is the ping's.
    s.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    assert_eq!(s.result(2, "ping", json!({})), json!({}));
    // Resources only: the other list methods answer with empty lists.
    assert_eq!(s.result(3, "tools/list", json!({}))["tools"], json!([]));
    assert_eq!(s.result(4, "prompts/list", json!({}))["prompts"], json!([]));
    assert_eq!(
        s.result(5, "resources/templates/list", json!({}))["resourceTemplates"],
        json!([])
    );
    // A client that names no protocol version gets the server's default.
    let init = s.result(6, "initialize", json!({}));
    assert_eq!(init["protocolVersion"], "2025-06-18");
    assert_eq!(s.finish(), Some(0));
}

/// One resource per contact of the merged book, the exact URI list (slug rules: lower
/// case, accents kept, punctuation runs → one `-`, trimmed; no e-mail; collision suffix),
/// name/description/mimeType.
#[test]
fn resources_list_has_one_uri_per_contact() {
    let (home, repo) = fixture();
    let mut s = Server::spawn(home.path(), repo.path());
    let result = s.result(1, "resources/list", json!({}));
    let resources = result["resources"].as_array().unwrap();
    let uris: Vec<&str> = resources
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    let bob_global = format!("to://bob-smith.bob@acme.com.{}", seg(4));
    let bob_local = format!("to://bob-smith.bob@acme.com.{}", seg(5));
    let (bob_a, bob_b) = if seg(4) < seg(5) {
        (&bob_global, &bob_local)
    } else {
        (&bob_local, &bob_global)
    };
    assert_eq!(
        uris,
        [
            "to://ana-kowalska.ana@acme.com",
            bob_a,
            bob_b,
            "to://dr-nomail-qa",
            "to://zoë-o-brien-łukasz.zoe@acme.com",
        ]
    );
    for r in resources {
        assert_eq!(r["mimeType"], "application/json", "{r}");
        assert_eq!(
            r.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["description", "mimeType", "name", "uri"],
            "{r}"
        );
    }
    assert_eq!(resources[0]["name"], "Ana Kowalska");
    assert_eq!(
        resources[0]["description"],
        format!("{} · global", fp(&id(1)))
    );
    let local = resources
        .iter()
        .find(|r| r["uri"] == bob_local.as_str())
        .unwrap();
    assert_eq!(local["description"], format!("{} · local", fp(&id(5))));
    assert_eq!(local["name"], "Bob Smith");
    // Outside any git repository the local contact is gone and the collision with it.
    let bare = tempfile::tempdir().unwrap();
    let mut s2 = Server::spawn(home.path(), bare.path());
    assert_eq!(
        s2.uris(1),
        [
            "to://ana-kowalska.ana@acme.com",
            "to://bob-smith.bob@acme.com",
            "to://dr-nomail-qa",
            "to://zoë-o-brien-łukasz.zoe@acme.com",
        ]
    );
    assert_eq!(s.finish(), Some(0));
    assert_eq!(s2.finish(), Some(0));
}

/// `resources/read` content shape and key set; the three error paths, each followed by
/// a request that still works.
#[test]
fn read_and_error_codes() {
    let (home, repo) = fixture();
    let mut s = Server::spawn(home.path(), repo.path());
    let uri = "to://ana-kowalska.ana@acme.com";
    let result = s.result(1, "resources/read", json!({"uri": uri}));
    let contents = result["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 1, "{result}");
    assert_eq!(contents[0]["uri"], uri);
    assert_eq!(contents[0]["mimeType"], "application/json");
    let text: Value = serde_json::from_str(contents[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        text.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["emails", "fingerprint", "name"]
    );
    assert_eq!(
        text,
        json!({"name": "Ana Kowalska", "fingerprint": fp(&id(1)), "emails": ["ana@acme.com", "ana.k@example.net"]})
    );
    // The local contact reads too, by its suffixed URI.
    let local = format!("to://bob-smith.bob@acme.com.{}", seg(5));
    let result = s.result(2, "resources/read", json!({"uri": local}));
    let text: Value =
        serde_json::from_str(result["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text["fingerprint"], fp(&id(5)));
    // Unknown URI, unknown method, missing uri param.
    assert_eq!(
        s.error_code(3, "resources/read", json!({"uri": "to://nobody"})),
        -32002
    );
    assert_eq!(s.error_code(4, "resources/subscribe", json!({})), -32601);
    assert_eq!(s.error_code(5, "resources/read", json!({})), -32602);
    // A line that is not JSON: error with id null, then service continues.
    s.send("this is not json");
    let bad = s.recv();
    assert_eq!(bad["id"], Value::Null, "{bad}");
    assert_eq!(bad["error"]["code"], -32700, "{bad}");
    assert!(bad.get("result").is_none());
    // Valid JSON that is not a request object.
    s.send("[1,2,3]");
    let bad = s.recv();
    assert_eq!(bad["id"], Value::Null, "{bad}");
    assert_eq!(bad["error"]["code"], -32600, "{bad}");
    // An empty line is skipped, not answered.
    s.send("");
    assert_eq!(s.result(6, "ping", json!({})), json!({}));
    assert_eq!(s.uris(7).len(), 5);
    assert_eq!(s.finish(), Some(0));
}

/// The book is re-read per request — a contact added between two lists shows up
/// without restarting the child.
#[test]
fn contacts_are_reloaded_per_request() {
    let (home, repo) = fixture();
    let mut s = Server::spawn(home.path(), repo.path());
    let before = s.uris(1);
    assert!(!before.iter().any(|u| u.contains("cara")), "{before:?}");
    assert_eq!(before[3], "to://dr-nomail-qa", "{before:?}");
    write_contact_full(
        home.path(),
        &Peer::new(&id(6), "Cara Díaz", None),
        &[],
        &["cara@acme.com"],
    );
    let after = s.uris(2);
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(after[3], "to://cara-díaz.cara@acme.com", "{after:?}");
    let result = s.result(
        3,
        "resources/read",
        json!({"uri": "to://cara-díaz.cara@acme.com"}),
    );
    let text: Value =
        serde_json::from_str(result["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text["fingerprint"], fp(&id(6)));
    // Removing it again is seen too.
    std::fs::remove_file(
        home.path()
            .join("contacts")
            .join(format!("{}.json", fp(&id(6)))),
    )
    .unwrap();
    assert_eq!(s.uris(4), before);
    assert_eq!(
        s.error_code(
            5,
            "resources/read",
            json!({"uri": "to://cara-díaz.cara@acme.com"})
        ),
        -32002
    );
    assert_eq!(s.finish(), Some(0));
}
