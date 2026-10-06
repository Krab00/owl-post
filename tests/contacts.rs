use std::path::{Path, PathBuf};

use owlpost::contacts::{ContactBook, KeyStanding, Mode, Policy, Scope, key_standing};
use owlpost::identity::{Identity, fingerprint, pubkey_string};

fn pk(seed: u8) -> String {
    pubkey_string(&Identity::from_seed([seed; 32]).verifying_key())
}

fn fp(seed: u8) -> String {
    fingerprint(&Identity::from_seed([seed; 32]).verifying_key())
}

fn peer_json(name: &str, email: &str, seed: u8) -> String {
    serde_json::json!({
        "name": name,
        "emails": [email],
        "pubkey": pk(seed),
        "endpoints": [format!("{}.example.org:7411", name.to_lowercase())],
    })
    .to_string()
}

/// Fixture git repo: `<root>/.git/`, `.agents/peers/{martin,mason}.json`, nested `src/deep/`.
/// Returns (root, nested subdir).
fn fixture_repo(base: &Path) -> (PathBuf, PathBuf) {
    let root = base.join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let peers = root.join(".agents").join("peers");
    std::fs::create_dir_all(&peers).unwrap();
    std::fs::write(
        peers.join("martin.json"),
        peer_json("Martin", "martin@company.com", 1),
    )
    .unwrap();
    std::fs::write(
        peers.join("mason.json"),
        peer_json("Mason", "mason@company.com", 2),
    )
    .unwrap();
    let sub = root.join("src").join("deep");
    std::fs::create_dir_all(&sub).unwrap();
    (root, sub)
}

fn overlay(home: &Path, seed: u8, name: &str, mode: &str) {
    let dir = home.join("contacts");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{}.json", fp(seed))),
        serde_json::json!({
            "name": name,
            "emails": ["other@example.org"],
            "pubkey": pk(seed),
            "endpoints": ["other.example.org:1"],
            "source": "global",
            "policy": { "mode": mode, "scope": { "projects": ["*"] } },
            "added_at": "2026-09-01T10:00:00Z",
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn repo_provider_finds_peers_from_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 2);
    let martin = book.resolve(&fp(1)).unwrap();
    assert_eq!(martin.name, "Martin");
    assert_eq!(martin.emails, ["martin@company.com"]);
    assert_eq!(martin.endpoints, ["martin.example.org:7411"]);
    assert_eq!(martin.pubkey, pk(1));
    assert_eq!(martin.fingerprint(), fp(1));
    assert_eq!(martin.source, "local");
    assert!(martin.policy.is_none());
    let mason = book.resolve(&fp(2)).unwrap();
    assert_eq!(mason.name, "Mason");
    assert_eq!(mason.source, "local");
    assert!(book.contacts.iter().all(|c| c.source == "local"));
}

#[test]
fn no_git_root_means_no_repo_contacts() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    // Peers dir exists but there is no `.git` anywhere up the tree of `sub`.
    let (root, sub) = fixture_repo(tmp.path());
    std::fs::remove_dir(root.join(".git")).unwrap();
    // The tempdir itself lives under /tmp (or similar) which is not a git repo.
    let book = ContactBook::load(&home, &sub).unwrap();
    assert!(book.contacts.is_empty(), "{:?}", book.contacts);
}

#[test]
fn local_overlay_only_contributes_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    overlay(&home, 1, "Wrong Name", "auto");
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 2, "overlay must not add a contact");
    let c = book.resolve(&fp(1)).unwrap();
    assert_eq!(c.name, "Martin");
    assert_eq!(c.emails, ["martin@company.com"]);
    assert_eq!(c.endpoints, ["martin.example.org:7411"]);
    assert_eq!(c.source, "local");
    assert_eq!(c.added_at, None, "overlay's added_at must not leak");
    assert_eq!(c.policy.as_ref().unwrap().mode, Mode::Auto);
    assert_eq!(book.policy_for(&fp(1)).unwrap().mode, Mode::Auto);
    assert!(book.policy_for(&fp(2)).is_none());
    assert!(
        book.resolve("Wrong").is_err(),
        "overlay name must not resolve"
    );
}

/// A committed `.agents/peers/` file cannot grant itself a policy: its `policy` key is
/// dropped at load (the peer is held for consent), a global overlay still sets one, and
/// `set_policy` (what `owl allow` calls) still works on such a contact.
#[test]
fn repo_peer_file_policy_is_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    let mut v: serde_json::Value =
        serde_json::from_str(&peer_json("Martin", "martin@company.com", 1)).unwrap();
    v["policy"] = serde_json::json!({ "mode": "auto", "scope": { "projects": ["*"] } });
    std::fs::write(root.join(".agents/peers/martin.json"), v.to_string()).unwrap();
    let mut book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.resolve(&fp(1)).unwrap().name, "Martin");
    assert!(book.policy_for(&fp(1)).is_none(), "repo policy dropped");
    // `owl allow` writes the global overlay; the reloaded book reads it.
    let manual = Policy {
        mode: Mode::Manual,
        scope: Scope::default(),
        rate_limit_per_hour: None,
    };
    book.set_policy(&home, &fp(1), manual.clone()).unwrap();
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.policy_for(&fp(1)), Some(&manual));
    // A hand-written global overlay wins over the repo file's `auto` as well.
    overlay(&home, 1, "", "never");
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.policy_for(&fp(1)).unwrap().mode, Mode::Never);
}

#[test]
fn local_only_contact_is_full_contact() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    overlay(&home, 3, "Owen", "manual");
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 3);
    let owen = book.resolve("owen").unwrap();
    assert_eq!(owen.name, "Owen");
    assert_eq!(owen.emails, ["other@example.org"]);
    assert_eq!(owen.endpoints, ["other.example.org:1"]);
    assert_eq!(owen.source, "global");
    assert_eq!(owen.fingerprint(), fp(3));
    assert_eq!(owen.added_at.as_deref(), Some("2026-09-01T10:00:00Z"));
    assert_eq!(owen.policy.as_ref().unwrap().mode, Mode::Manual);
    // Source is decided by the provider, not by the file.
    let dir = home.join("contacts");
    std::fs::write(
        dir.join("lying.json"),
        format!(r#"{{"name":"Liar","pubkey":"{}","source":"local"}}"#, pk(4)),
    )
    .unwrap();
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.resolve("Liar").unwrap().source, "global");
    // Local (repo) entries are listed before global ones.
    let sources: Vec<&str> = book.contacts.iter().map(|c| c.source.as_str()).collect();
    assert_eq!(sources, ["local", "local", "global", "global"]);
}

#[test]
fn resolve_orders_and_ambiguity() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    // A third peer whose *name* equals Martin's email, to prove email beats name.
    std::fs::write(
        root.join(".agents/peers/z.json"),
        peer_json("martin@company.com", "z@company.com", 3),
    )
    .unwrap();
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 3);
    // by fingerprint
    assert_eq!(book.resolve(&fp(2)).unwrap().name, "Mason");
    // by email (beats the exact-name match of peer 3)
    assert_eq!(
        book.resolve("martin@company.com").unwrap().fingerprint(),
        fp(1)
    );
    assert_eq!(book.resolve("z@company.com").unwrap().fingerprint(), fp(3));
    // by unique name prefix, case-insensitive
    assert_eq!(book.resolve("mas").unwrap().name, "Mason");
    assert_eq!(book.resolve("MAS").unwrap().name, "Mason");
    // "Mart" is a prefix of both "Martin" and the peer named "martin@company.com".
    assert!(book.resolve("Mart").is_err());
    assert_eq!(book.resolve("mason").unwrap().name, "Mason");
    // ambiguous prefix lists both names
    let err = book.resolve("ma").unwrap_err().to_string();
    assert!(err.contains("Martin"), "{err}");
    assert!(err.contains("Mason"), "{err}");
    assert!(err.contains("martin@company.com"), "{err}");
    assert!(err.to_lowercase().contains("ambiguous"), "{err}");
    // negatives: wrong fingerprint, wrong email, prefix of nothing, non-prefix substring
    assert!(book.resolve(&fp(9)).is_err());
    assert!(book.resolve("nobody@company.com").is_err());
    assert!(book.resolve("xyz").is_err());
    assert!(book.resolve("arek").is_err(), "substring is not a prefix");
    assert!(book.resolve(&fp(1)[..10]).is_err(), "partial fingerprint");
}

#[test]
fn resolve_unknown_is_error() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let book = ContactBook::load(&home, &sub).unwrap();
    let err = book.resolve("nobody").unwrap_err().to_string();
    assert!(err.contains("nobody"), "{err}");
    let empty = ContactBook::default();
    assert!(empty.resolve("").is_err());
    assert!(empty.resolve(&fp(1)).is_err());
}

#[test]
fn malformed_file_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    let peers = root.join(".agents/peers");
    std::fs::write(peers.join("broken.json"), "{not json").unwrap();
    std::fs::write(
        peers.join("badkey.json"),
        r#"{"name":"Bad","pubkey":"ed25519:AAAA"}"#,
    )
    .unwrap();
    std::fs::write(peers.join("nokey.json"), r#"{"name":"NoKey"}"#).unwrap();
    std::fs::write(peers.join("README.md"), "not a contact").unwrap();
    let book = ContactBook::load(&home, &sub).unwrap();
    let mut names: Vec<&str> = book.contacts.iter().map(|c| c.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Martin", "Mason"]);
    // Same for the local provider.
    std::fs::create_dir_all(home.join("contacts")).unwrap();
    std::fs::write(home.join("contacts/x.json"), "[]").unwrap();
    overlay(&home, 3, "Owen", "never");
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 3);
    assert_eq!(
        book.resolve("Owen").unwrap().policy.as_ref().unwrap().mode,
        Mode::Never
    );
}

#[test]
fn set_policy_writes_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let mut book = ContactBook::load(&home, &sub).unwrap();
    let policy = Policy {
        mode: Mode::Auto,
        scope: Scope::default(),
        rate_limit_per_hour: Some(20),
    };
    book.set_policy(&home, &fp(1), policy.clone()).unwrap();
    assert_eq!(book.policy_for(&fp(1)), Some(&policy));
    let path = home.join("contacts").join(format!("{}.json", fp(1)));
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["added_at", "policy", "pubkey", "source"], "{v}");
    assert_eq!(v["pubkey"], pk(1));
    assert_eq!(v["source"], "global");
    assert_eq!(v["policy"]["mode"], "auto");
    assert_eq!(v["policy"]["rate_limit_per_hour"], 20);
    assert_eq!(v["policy"]["scope"]["projects"], serde_json::json!(["*"]));
    let added = v["added_at"].as_str().unwrap();
    assert_eq!(added.len(), 20, "{added}");
    assert!(added.starts_with("20") && added.ends_with('Z'), "{added}");
    // Reload: repo name kept, policy from overlay.
    let book = ContactBook::load(&home, &sub).unwrap();
    let c = book.resolve(&fp(1)).unwrap();
    assert_eq!(c.name, "Martin");
    assert_eq!(c.policy.as_ref(), Some(&policy));
    // Update keeps the existing file's fields and swaps the policy.
    let mut book = book;
    let never = Policy {
        mode: Mode::Never,
        scope: Scope::default(),
        rate_limit_per_hour: None,
    };
    book.set_policy(&home, &fp(1), never.clone()).unwrap();
    let v2: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v2["added_at"], added);
    assert_eq!(v2["policy"]["mode"], "never");
    assert!(v2["policy"].get("rate_limit_per_hour").is_none(), "{v2}");
    // Local-only contact: file gains policy, keeps name/emails/endpoints.
    overlay(&home, 3, "Owen", "manual");
    let mut book = ContactBook::load(&home, &sub).unwrap();
    book.set_policy(&home, &fp(3), never).unwrap();
    let v3: serde_json::Value = serde_json::from_slice(
        &std::fs::read(home.join("contacts").join(format!("{}.json", fp(3)))).unwrap(),
    )
    .unwrap();
    assert_eq!(v3["name"], "Owen");
    assert_eq!(v3["policy"]["mode"], "never");
    assert!(v3.get("fingerprint").is_none(), "{v3}");
    // Unknown fingerprint is an error and writes nothing.
    assert!(
        book.set_policy(
            &home,
            &fp(9),
            Policy {
                mode: Mode::Auto,
                scope: Scope::default(),
                rate_limit_per_hour: None
            }
        )
        .is_err()
    );
    assert!(
        !home
            .join("contacts")
            .join(format!("{}.json", fp(9)))
            .exists()
    );
}

fn auto() -> Policy {
    Policy {
        mode: Mode::Auto,
        scope: Scope::default(),
        rate_limit_per_hour: None,
    }
}

#[test]
fn set_policy_rejects_non_object_overlay_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let dir = home.join("contacts");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}.json", fp(1)));
    std::fs::write(&path, "[]").unwrap();
    let mut book = ContactBook::load(&home, &sub).unwrap();
    let err = book
        .set_policy(&home, &fp(1), auto())
        .unwrap_err()
        .to_string();
    assert!(err.contains("not a JSON object"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[]",
        "file untouched"
    );
    assert!(
        book.policy_for(&fp(1)).is_none(),
        "in-memory policy untouched"
    );
    // A string is not an object either.
    std::fs::write(&path, "\"x\"").unwrap();
    assert!(book.set_policy(&home, &fp(1), auto()).is_err());
}

#[test]
fn fingerprint_tier_beats_email_tier() {
    // Peer 3's email is literally peer 1's fingerprint string.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    std::fs::write(
        root.join(".agents/peers/aaa.json"),
        peer_json("Zed", &fp(1), 3),
    )
    .unwrap();
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 3);
    assert_eq!(
        book.contacts[0].name, "Zed",
        "sorted first, so a naive scan would hit it"
    );
    let c = book.resolve(&fp(1)).unwrap();
    assert_eq!(c.name, "Martin");
    assert_eq!(c.fingerprint(), fp(1));
}

#[test]
fn prefix_tier_matches_names_only() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let book = ContactBook::load(&home, &sub).unwrap();
    // "martin@" is a prefix of Martin's email but of no name.
    assert!(book.resolve("martin@").is_err());
    assert!(book.resolve("martin@company").is_err());
    // Exact email still works.
    assert_eq!(book.resolve("martin@company.com").unwrap().name, "Martin");
}

#[test]
fn email_match_is_exact_case_sensitive() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.resolve("mason@company.com").unwrap().name, "Mason");
    assert!(book.resolve("Mason@company.com").is_err());
    assert!(book.resolve("MASON@COMPANY.COM").is_err());
    assert!(book.resolve(" mason@company.com").is_err());
}

#[test]
fn git_file_marks_worktree_root() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    std::fs::remove_dir(root.join(".git")).unwrap();
    std::fs::write(root.join(".git"), "gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
    assert_eq!(
        owlpost::contacts::repo::find_git_root(&sub).as_deref(),
        Some(root.as_path())
    );
    let book = ContactBook::load(&home, &sub).unwrap();
    assert_eq!(book.contacts.len(), 2);
    assert!(book.contacts.iter().all(|c| c.source == "local"));
}

#[test]
fn set_policy_on_slug_named_local_contact_writes_fp_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let dir = home.join("contacts");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("owen.json"),
        serde_json::json!({
            "name": "Owen", "emails": ["owen@example.org"], "pubkey": pk(3),
            "endpoints": ["owen.example.org:7411"],
        })
        .to_string(),
    )
    .unwrap();
    let mut book = ContactBook::load(&home, &sub).unwrap();
    book.set_policy(&home, &fp(3), auto()).unwrap();
    let path = dir.join(format!("{}.json", fp(3)));
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "added_at",
            "emails",
            "endpoints",
            "name",
            "policy",
            "pubkey",
            "source"
        ],
        "{v}"
    );
    assert_eq!(v["name"], "Owen");
    assert_eq!(v["emails"], serde_json::json!(["owen@example.org"]));
    assert_eq!(v["endpoints"], serde_json::json!(["owen.example.org:7411"]));
    assert_eq!(v["pubkey"], pk(3));
    assert_eq!(v["source"], "global");
    assert_eq!(v["policy"]["mode"], "auto");
    assert!(v.get("fingerprint").is_none(), "{v}");
    assert!(dir.join("owen.json").exists(), "slug file left alone");
}

#[test]
fn non_json_extensions_are_ignored_silently() {
    // Loading in-process: stderr can't be captured here, so the CLI twin in tests/cli.rs
    // pins "no warning"; this one pins "not loaded".
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    let peers = root.join(".agents/peers");
    std::fs::write(peers.join("upper.JSON"), peer_json("Upper", "u@x.org", 5)).unwrap();
    std::fs::write(peers.join("old.json.bak"), peer_json("Bak", "b@x.org", 6)).unwrap();
    std::fs::write(peers.join("noext"), peer_json("NoExt", "n@x.org", 7)).unwrap();
    let book = ContactBook::load(&home, &sub).unwrap();
    let mut names: Vec<&str> = book.contacts.iter().map(|c| c.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Martin", "Mason"]);
}

#[test]
fn contacts_are_ordered_by_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = tmp.path().join("repo2");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let peers = root.join(".agents/peers");
    std::fs::create_dir_all(&peers).unwrap();
    // Filenames sort z < ... no: "a" < "b", but names sort the other way round.
    std::fs::write(peers.join("a.json"), peer_json("Zoe", "z@x.org", 1)).unwrap();
    std::fs::write(peers.join("b.json"), peer_json("Adam", "a@x.org", 2)).unwrap();
    let book = ContactBook::load(&home, &root).unwrap();
    let names: Vec<&str> = book.contacts.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Zoe", "Adam"]);
    // Same rule for the local provider.
    let dir = home.join("contacts");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("1.json"), peer_json("Yara", "y@x.org", 3)).unwrap();
    std::fs::write(dir.join("0.json"), peer_json("Bea", "b@x.org", 4)).unwrap();
    let book = ContactBook::load(&home, &root).unwrap();
    let names: Vec<&str> = book.contacts.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Zoe", "Adam", "Bea", "Yara"]);
}

// ---------- Scopes, `owl add`, `contact list --global|--local`, `contact remove` ----------

const OWL: &str = env!("CARGO_BIN_EXE_owl");

/// Runs `owl --home <home> <args>` from `cwd`, optionally feeding `stdin`.
fn owl(home: &Path, cwd: &Path, args: &[&str], stdin: Option<&str>) -> std::process::Output {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(OWL);
    cmd.arg("--home")
        .arg(home)
        .args(args)
        .current_dir(cwd)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    if let Some(s) = stdin {
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn out(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Table rows of `owl contact list` (header skipped), whitespace-split.
fn list_rows(home: &Path, cwd: &Path, extra: &[&str]) -> Vec<Vec<String>> {
    let o = owl(home, cwd, &[&["contact", "list"][..], extra].concat(), None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    out(&o)
        .lines()
        .skip(1)
        .map(|l| l.split_whitespace().map(str::to_string).collect())
        .collect()
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// A temp dir with nothing git-like above it (the tempdir root itself).
fn plain_dir(base: &Path) -> PathBuf {
    let d = base.join("plain");
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn add_file_writes_global_slug_and_lists_as_global() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    let file = tmp.path().join("owen.json");
    std::fs::write(&file, peer_json("Owen Nolan", "owen@company.com", 3)).unwrap();
    let o = owl(&home, &sub, &["add", file.to_str().unwrap()], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), format!("added Owen Nolan {} (global)\n", fp(3)));
    let path = home.join("contacts").join("owen-nolan.json");
    let v = read_json(&path);
    assert_eq!(
        v,
        serde_json::json!({
            "name": "Owen Nolan",
            "emails": ["owen@company.com"],
            "pubkey": pk(3),
            "endpoints": ["owen nolan.example.org:7411"],
        })
    );
    assert!(v.get("policy").is_none(), "adding never sets a policy");
    assert!(!home.join("contacts").join("owen-nolan.json.tmp").exists());
    let rows = list_rows(&home, &sub, &[]);
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[2], ["Owen", "Nolan", &fp(3), "global", "-"]);
    // `contact show` JSON carries the scope too.
    let o = owl(&home, &sub, &["contact", "show", "Owen"], None);
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["source"], "global");
    assert_eq!(v["fingerprint"], fp(3));
}

#[test]
fn add_local_writes_into_repo_peers_and_lists_as_local() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    let file = tmp.path().join("owen.json");
    std::fs::write(&file, peer_json("Owen", "owen@company.com", 3)).unwrap();
    // From a nested subdir: the file lands at the git root.
    let o = owl(
        &home,
        &sub,
        &["add", file.to_str().unwrap(), "--local"],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), format!("added Owen {} (local)\n", fp(3)));
    let path = root.join(".agents/peers/owen.json");
    assert_eq!(read_json(&path)["pubkey"], pk(3));
    assert!(!home.join("contacts").exists(), "global book untouched");
    let rows = list_rows(&home, &sub, &[]);
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[2], ["Owen", &fp(3), "local", "-"]);
}

#[test]
fn add_reads_inline_json_and_stdin() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = plain_dir(tmp.path());
    // Inline JSON as one argument (leading whitespace allowed).
    let inline = format!("  {}", peer_json("Owen", "owen@x.org", 3));
    let o = owl(&home, &cwd, &["add", &inline], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), format!("added Owen {} (global)\n", fp(3)));
    assert!(home.join("contacts/owen.json").exists());
    // `-` reads stdin.
    let o = owl(
        &home,
        &cwd,
        &["add", "-"],
        Some(&peer_json("Bea", "bea@x.org", 4)),
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), format!("added Bea {} (global)\n", fp(4)));
    assert!(home.join("contacts/bea.json").exists());
    // Only `name` and `pubkey`: optional lists stay absent in the written file.
    let o = owl(
        &home,
        &cwd,
        &["add", &format!(r#"{{"name":"Cy","pubkey":"{}"}}"#, pk(5))],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v = read_json(&home.join("contacts/cy.json"));
    assert_eq!(v, serde_json::json!({"name": "Cy", "pubkey": pk(5)}));
    let rows = list_rows(&home, &cwd, &[]);
    let names: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
    assert_eq!(names, ["Bea", "Cy", "Owen"]);
    assert!(rows.iter().all(|r| r[2] == "global"), "{rows:?}");
}

#[test]
fn add_missing_path_is_a_user_error() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = plain_dir(tmp.path());
    let o = owl(&home, &cwd, &["add", "nope.json"], None);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("reading peer file nope.json"),
        "{}",
        err(&o)
    );
    assert!(!home.join("contacts").exists());
}

#[test]
fn add_refuses_pubkey_already_in_either_scope_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    // Peer 1 lives in the local scope (fixture repo).
    let dup_local = peer_json("Martin Again", "again@x.org", 1);
    for flags in [&[][..], &["--local"][..]] {
        let o = owl(
            &home,
            &sub,
            &[&["add", &dup_local][..], flags].concat(),
            None,
        );
        assert_eq!(o.status.code(), Some(1), "{flags:?}");
        assert_eq!(err(&o), "owl: already a contact: Martin (local)\n");
        assert!(!home.join("contacts").exists(), "{flags:?}");
        assert!(!root.join(".agents/peers/martin-again.json").exists());
    }
    // Peer 3 lives in the global scope.
    let o = owl(
        &home,
        &sub,
        &["add", &peer_json("Owen", "owen@x.org", 3)],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let dup_global = peer_json("Owen Two", "two@x.org", 3);
    for flags in [&[][..], &["--local"][..]] {
        let o = owl(
            &home,
            &sub,
            &[&["add", &dup_global][..], flags].concat(),
            None,
        );
        assert_eq!(o.status.code(), Some(1), "{flags:?}");
        assert_eq!(err(&o), "owl: already a contact: Owen (global)\n");
        assert!(!home.join("contacts/owen-two.json").exists());
        assert!(!root.join(".agents/peers/owen-two.json").exists());
    }
    // Same slug for a *new* key: the existing file is never replaced.
    let o = owl(
        &home,
        &sub,
        &["add", &peer_json("Owen", "ola2@x.org", 4)],
        None,
    );
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(
        err(&o).contains("owen.json") && err(&o).contains("already exists"),
        "{}",
        err(&o)
    );
    assert_eq!(read_json(&home.join("contacts/owen.json"))["pubkey"], pk(3));
    // A new key under a new name is fine (the duplicate check is by key, not by name).
    let o = owl(
        &home,
        &sub,
        &["add", &peer_json("Owen B", "ola2@x.org", 4)],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), format!("added Owen B {} (global)\n", fp(4)));
}

#[test]
fn add_rejects_missing_or_invalid_fields_naming_the_field() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    let cases = [
        (format!(r#"{{"pubkey":"{}"}}"#, pk(3)), "`name`"),
        (r#"{"name":"Owen"}"#.to_string(), "`pubkey`"),
        (format!(r#"{{"name":"","pubkey":"{}"}}"#, pk(3)), "`name`"),
        (
            format!(r#"{{"name":["Owen"],"pubkey":"{}"}}"#, pk(3)),
            "`name`",
        ),
        (
            r#"{"name":"Owen","pubkey":"ed25519:AAAA"}"#.to_string(),
            "`pubkey`",
        ),
        (
            r#"{"name":"Owen","pubkey":"garbage"}"#.to_string(),
            "`pubkey`",
        ),
        (
            format!(r#"{{"name":"Owen","pubkey":"{}","emails":"x"}}"#, pk(3)),
            "`emails`",
        ),
        (
            format!(r#"{{"name":"Owen","pubkey":"{}","endpoints":[1]}}"#, pk(3)),
            "`endpoints`",
        ),
        ("{not json".to_string(), "not valid JSON"),
        ("[]".to_string(), "must be a JSON object"),
        ("\"x\"".to_string(), "must be a JSON object"),
    ];
    for (text, needle) in &cases {
        for (flags, via_stdin) in [(&[][..], false), (&["--local"][..], false), (&[][..], true)] {
            // Only text starting with `{` is inline JSON; anything else is a path.
            if !via_stdin && !text.starts_with('{') {
                continue;
            }
            let args: Vec<&str> = if via_stdin {
                vec!["add", "-"]
            } else {
                vec!["add", text]
            };
            let o = owl(
                &home,
                &sub,
                &[&args[..], flags].concat(),
                via_stdin.then_some(text.as_str()),
            );
            assert_eq!(
                o.status.code(),
                Some(1),
                "{text} {flags:?} stdin={via_stdin}"
            );
            assert!(err(&o).contains(needle), "{text}: {}", err(&o));
        }
    }
    assert!(!home.join("contacts").exists(), "nothing written to global");
    let mut peers: Vec<_> = std::fs::read_dir(root.join(".agents/peers"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    peers.sort();
    assert_eq!(
        peers,
        ["martin.json", "mason.json"],
        "nothing written to local"
    );
}

#[test]
fn add_local_outside_a_git_repo_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = plain_dir(tmp.path());
    let o = owl(
        &home,
        &cwd,
        &["add", &peer_json("Owen", "o@x", 3), "--local"],
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(e.contains("--local") && e.contains("git repository"), "{e}");
    assert!(!cwd.join(".agents").exists());
    assert!(!home.join("contacts").exists());
}

#[test]
fn add_blocked_by_directory_at_destination_leaves_no_tmp() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    // Global: a directory sits where `owen.json` must go.
    std::fs::create_dir_all(home.join("contacts/owen.json")).unwrap();
    let o = owl(&home, &sub, &["add", &peer_json("Owen", "o@x", 3)], None);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("owen.json"), "{}", err(&o));
    assert!(home.join("contacts/owen.json").is_dir(), "left alone");
    assert!(
        !home.join("contacts/owen.json.tmp").exists(),
        "no temp file left"
    );
    // Local: same for `.agents/peers/owen.json`.
    std::fs::create_dir_all(root.join(".agents/peers/owen.json")).unwrap();
    let o = owl(
        &home,
        &sub,
        &["add", &peer_json("Owen", "o@x", 3), "--local"],
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(!root.join(".agents/peers/owen.json.tmp").exists());
    assert!(root.join(".agents/peers/owen.json").is_dir());
}

#[test]
fn contact_list_scope_flags_list_exactly_one_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    overlay(&home, 3, "Owen", "manual"); // global contact
    overlay(&home, 1, "Wrong", "auto"); // policy overlay for local Martin
    let all = list_rows(&home, &sub, &[]);
    assert_eq!(
        all,
        [
            vec!["Martin", &fp(1), "local", "auto"],
            vec!["Mason", &fp(2), "local", "-"],
            vec!["Owen", &fp(3), "global", "manual"],
        ]
    );
    let global = list_rows(&home, &sub, &["--global"]);
    assert_eq!(global, [["Owen", &fp(3), "global", "manual"]]);
    let local = list_rows(&home, &sub, &["--local"]);
    assert_eq!(
        local,
        [
            ["Martin", &fp(1), "local", "auto"],
            ["Mason", &fp(2), "local", "-"],
        ]
    );
    // --json honours the filter too.
    let o = owl(&home, &sub, &["contact", "list", "--local", "--json"], None);
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    let sources: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["source"].as_str().unwrap())
        .collect();
    assert_eq!(sources, ["local", "local"]);
    let o = owl(
        &home,
        &sub,
        &["contact", "list", "--global", "--json"],
        None,
    );
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["name"], "Owen");
    // Outside a repo: --local is an empty table; --global lists every global file, and the
    // overlay is a full contact now (no local Martin to overlay), so it shows as "Wrong".
    let cwd = plain_dir(tmp.path());
    assert!(list_rows(&home, &cwd, &["--local"]).is_empty());
    assert_eq!(
        list_rows(&home, &cwd, &["--global"]),
        [
            ["Wrong", &fp(1), "global", "auto"],
            ["Owen", &fp(3), "global", "manual"]
        ]
    );
}

#[test]
fn contact_list_global_and_local_together_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    for order in [["--global", "--local"], ["--local", "--global"]] {
        let o = owl(
            &home,
            &sub,
            &[&["contact", "list"][..], &order[..]].concat(),
            None,
        );
        assert_eq!(o.status.code(), Some(2), "{order:?}: {}", err(&o));
        assert!(out(&o).is_empty());
        let e = err(&o);
        assert!(e.contains("--global") && e.contains("--local"), "{e}");
    }
}

#[test]
fn contact_remove_deletes_the_global_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    let o = owl(
        &home,
        &sub,
        &["add", &peer_json("Owen Nolan", "owen@x.org", 3)],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    overlay(&home, 4, "Bea", "never"); // a fingerprint-named global file
    let path = home.join("contacts/owen-nolan.json");
    assert!(path.exists());
    // By name prefix.
    let o = owl(&home, &sub, &["contact", "remove", "owen"], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "removed Owen Nolan (global)\n");
    assert!(!path.exists());
    // By fingerprint, file named by fingerprint.
    let o = owl(&home, &sub, &["contact", "remove", &fp(4)], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "removed Bea (global)\n");
    assert!(
        !home
            .join("contacts")
            .join(format!("{}.json", fp(4)))
            .exists()
    );
    // Local files untouched, global gone from the list.
    assert!(root.join(".agents/peers/martin.json").exists());
    assert!(root.join(".agents/peers/mason.json").exists());
    assert_eq!(list_rows(&home, &sub, &[]).len(), 2);
    // Removing again: unknown peer, exit 1, no scope hint.
    let o = owl(&home, &sub, &["contact", "remove", "owen"], None);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("no contact matches"), "{}", err(&o));
    assert!(!err(&o).contains("--local"), "{}", err(&o));
}

#[test]
fn contact_remove_local_deletes_the_repo_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    overlay(&home, 1, "", "auto"); // policy overlay for Martin lives in global
    overlay(&home, 3, "Owen", "manual");
    let o = owl(
        &home,
        &sub,
        &["contact", "remove", "martin@company.com", "--local"],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "removed Martin (local)\n");
    assert!(!root.join(".agents/peers/martin.json").exists());
    assert!(root.join(".agents/peers/mason.json").exists());
    assert!(
        !home
            .join("contacts")
            .join(format!("{}.json", fp(1)))
            .exists(),
        "the policy overlay goes with its contact"
    );
    assert!(
        home.join("contacts")
            .join(format!("{}.json", fp(3)))
            .exists(),
        "other global files untouched"
    );
    // Ambiguous prefix in the scope is an error and deletes nothing.
    std::fs::write(
        root.join(".agents/peers/massimo.json"),
        peer_json("Massimo", "massimo@x.org", 5),
    )
    .unwrap();
    let o = owl(&home, &sub, &["contact", "remove", "mas", "--local"], None);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("ambiguous"), "{}", err(&o));
    assert!(root.join(".agents/peers/mason.json").exists());
    assert!(root.join(".agents/peers/massimo.json").exists());
}

#[test]
fn contact_remove_hints_the_other_scope_in_both_directions() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    overlay(&home, 3, "Owen", "manual");
    // Local peer, default (global) scope → hint `use --local`.
    let o = owl(&home, &sub, &["contact", "remove", "Martin"], None);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), "owl: Martin is a local contact; use --local\n");
    assert!(root.join(".agents/peers/martin.json").exists());
    // Global peer, --local → hint `drop --local`.
    let o = owl(&home, &sub, &["contact", "remove", "Owen", "--local"], None);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), "owl: Owen is a global contact; drop --local\n");
    assert!(
        home.join("contacts")
            .join(format!("{}.json", fp(3)))
            .exists()
    );
    // A global policy overlay for a local contact is not a global contact: removing the
    // local peer without --local still hints, and the overlay stays.
    overlay(&home, 1, "", "auto");
    let o = owl(&home, &sub, &["contact", "remove", &fp(1)], None);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), "owl: Martin is a local contact; use --local\n");
    assert!(
        home.join("contacts")
            .join(format!("{}.json", fp(1)))
            .exists()
    );
}

#[test]
fn contact_remove_local_outside_a_git_repo_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = plain_dir(tmp.path());
    overlay(&home, 3, "Owen", "manual");
    // An unknown peer: the missing repo is the error.
    let o = owl(
        &home,
        &cwd,
        &["contact", "remove", "Nobody", "--local"],
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("git repository"), "{}", err(&o));
    // A global peer: the scope hint is more useful than the missing repo.
    let o = owl(&home, &cwd, &["contact", "remove", "Owen", "--local"], None);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), "owl: Owen is a global contact; drop --local\n");
    assert!(
        home.join("contacts")
            .join(format!("{}.json", fp(3)))
            .exists()
    );
}

// ----- library-level scope helpers -----

#[test]
fn load_scope_filters_the_merged_book() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    overlay(&home, 3, "Owen", "manual");
    overlay(&home, 1, "Wrong", "auto");
    let g = ContactBook::load_scope(&home, &sub, "global").unwrap();
    let names: Vec<&str> = g.contacts.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Owen"]);
    let l = ContactBook::load_scope(&home, &sub, "local").unwrap();
    let names: Vec<&str> = l.contacts.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Martin", "Mason"]);
    assert_eq!(
        l.resolve("Martin").unwrap().policy.as_ref().unwrap().mode,
        Mode::Auto
    );
    assert!(
        ContactBook::load_scope(&home, &sub, "other")
            .unwrap()
            .contacts
            .is_empty()
    );
}

#[test]
fn policy_survives_reload_whatever_the_filename_order() {
    // Global files merge in filename order: `owl:<fp>.json` sorts after "mason" and before
    // "paul", so both orders of (contact file, policy overlay) must keep name AND policy.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = plain_dir(tmp.path());
    for (name, seed) in [("Mason", 2u8), ("Paul", 5)] {
        let o = owl(
            &home,
            &cwd,
            &["add", &peer_json(name, "x@x.org", seed)],
            None,
        );
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        let mut book = ContactBook::load(&home, &cwd).unwrap();
        book.set_policy(&home, &fp(seed), auto()).unwrap();
        assert!(
            home.join("contacts")
                .join(format!("{}.json", fp(seed)))
                .exists()
        );
        let book = ContactBook::load(&home, &cwd).unwrap();
        let c = book.resolve(name).unwrap();
        assert_eq!(c.name, name);
        assert_eq!(c.emails, ["x@x.org"], "{name}");
        assert_eq!(
            c.policy.as_ref().map(|p| p.mode),
            Some(Mode::Auto),
            "{name}"
        );
        assert_eq!(book.policy_for(&fp(seed)).map(|p| p.mode), Some(Mode::Auto));
        assert_eq!(
            book.contacts
                .iter()
                .filter(|c| c.fingerprint == fp(seed))
                .count(),
            1,
            "one merged row for {name}"
        );
    }
    let rows = list_rows(&home, &cwd, &[]);
    assert_eq!(
        rows,
        [
            ["Mason", &fp(2), "global", "auto"],
            ["Paul", &fp(5), "global", "auto"],
        ]
    );
    let o = owl(&home, &cwd, &["contact", "show", "Paul"], None);
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["policy"]["mode"], "auto");
    assert_eq!(v["name"], "Paul");
    // A *bare* overlay (pubkey + policy only, as `owl allow` writes for a local contact)
    // next to a slug-named contact file, in both filename orders: the contact fields come
    // from the contact file, the policy from the overlay.
    let home2 = tmp.path().join("home2");
    let dir = home2.join("contacts");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, seed) in [("Mason", 2u8), ("Paul", 5)] {
        std::fs::write(
            dir.join(format!("{}.json", name.to_lowercase())),
            peer_json(name, "y@x.org", seed),
        )
        .unwrap();
        std::fs::write(
            dir.join(format!("{}.json", fp(seed))),
            serde_json::json!({"pubkey": pk(seed), "policy": {"mode": "never"}}).to_string(),
        )
        .unwrap();
    }
    let book = ContactBook::load(&home2, &cwd).unwrap();
    assert_eq!(book.contacts.len(), 2, "{:?}", book.contacts);
    for (name, seed) in [("Mason", 2u8), ("Paul", 5)] {
        let c = book.resolve(name).unwrap();
        assert_eq!(c.name, name);
        assert_eq!(c.emails, ["y@x.org"], "{name}");
        assert_eq!(c.fingerprint, fp(seed));
        assert_eq!(
            c.policy.as_ref().map(|p| p.mode),
            Some(Mode::Never),
            "{name}"
        );
    }
}

#[test]
fn allow_then_list_keeps_policy_for_name_after_owl_prefix() {
    // The CLI path the bug was seen on: add → allow --always → list/show.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = plain_dir(tmp.path());
    let o = owl(
        &home,
        &cwd,
        &["add", &peer_json("Paul Nolan", "p@x.org", 5)],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = owl(
        &home,
        &cwd,
        &["allow", &fp(5), "--always", "--i-verified-the-fingerprint"],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(
        list_rows(&home, &cwd, &[]),
        [["Paul", "Nolan", &fp(5), "global", "auto"]]
    );
    let o = owl(&home, &cwd, &["contact", "show", &fp(5)], None);
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v["policy"]["mode"], "auto", "{v}");
    assert_eq!(v["name"], "Paul Nolan");
}

#[test]
fn contact_remove_after_allow_deletes_contact_and_overlay_in_both_scopes() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    let paul = peer_json("Paul Nolan", "p@x.org", 5);
    // Global: add → allow → remove.
    let o = owl(&home, &sub, &["add", &paul], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = owl(
        &home,
        &sub,
        &["allow", &fp(5), "--always", "--i-verified-the-fingerprint"],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let overlay_path = home.join("contacts").join(format!("{}.json", fp(5)));
    assert!(overlay_path.exists());
    let o = owl(&home, &sub, &["contact", "remove", "Paul"], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "removed Paul Nolan (global)\n");
    assert!(!home.join("contacts/paul-nolan.json").exists());
    assert!(
        !overlay_path.exists(),
        "policy overlay removed with the contact"
    );
    assert_eq!(
        list_rows(&home, &sub, &["--global"]),
        Vec::<Vec<String>>::new()
    );
    // The same key can be added again: no ghost entry.
    let o = owl(&home, &sub, &["add", &paul], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = owl(&home, &sub, &["contact", "remove", "Paul"], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    // Local: add --local → allow → remove --local.
    let o = owl(&home, &sub, &["add", &paul, "--local"], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = owl(&home, &sub, &["allow", &fp(5), "--always"], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(overlay_path.exists());
    assert_eq!(
        list_rows(&home, &sub, &[])[2],
        ["Paul", "Nolan", &fp(5), "local", "auto"]
    );
    let o = owl(&home, &sub, &["contact", "remove", "Paul", "--local"], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "removed Paul Nolan (local)\n");
    assert!(!root.join(".agents/peers/paul-nolan.json").exists());
    assert!(
        !overlay_path.exists(),
        "overlay removed with the local contact"
    );
    assert!(root.join(".agents/peers/martin.json").exists());
    assert!(root.join(".agents/peers/mason.json").exists());
    assert_eq!(list_rows(&home, &sub, &[]).len(), 2);
    assert!(list_rows(&home, &sub, &["--global"]).is_empty());
    let o = owl(&home, &sub, &["add", &paul, "--local"], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), format!("added Paul Nolan {} (local)\n", fp(5)));
}

#[test]
fn contact_remove_propagates_a_failed_unlink() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = plain_dir(tmp.path());
    let o = owl(&home, &cwd, &["add", &peer_json("Owen", "o@x", 3)], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let dir = home.join("contacts");
    let path = dir.join("owen.json");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let o = owl(&home, &cwd, &["contact", "remove", "Owen"], None);
    let restore = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
    restore.unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(
        err(&o).contains("removing") && err(&o).contains("owen.json"),
        "{}",
        err(&o)
    );
    assert!(out(&o).is_empty(), "no `removed` line on failure");
    assert!(path.exists(), "guard engaged: the file is still there");
}

#[test]
fn contact_remove_local_failed_unlink_is_not_masked_by_the_scope_hint() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    let peers = root.join(".agents/peers");
    std::fs::set_permissions(&peers, std::fs::Permissions::from_mode(0o555)).unwrap();
    let o = owl(
        &home,
        &sub,
        &["contact", "remove", "Martin", "--local"],
        None,
    );
    std::fs::set_permissions(&peers, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    let e = err(&o);
    assert!(e.contains("removing") && e.contains("martin.json"), "{e}");
    assert!(
        !e.contains("use --local"),
        "the real error, not the scope hint: {e}"
    );
    assert!(peers.join("martin.json").exists(), "guard engaged");
}

#[test]
fn add_rejects_mixed_type_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cwd = plain_dir(tmp.path());
    for (text, field) in [
        (
            format!(
                r#"{{"name":"Owen","pubkey":"{}","emails":["a@x",1]}}"#,
                pk(3)
            ),
            "`emails`",
        ),
        (
            format!(
                r#"{{"name":"Owen","pubkey":"{}","endpoints":["h:1",{{}}]}}"#,
                pk(3)
            ),
            "`endpoints`",
        ),
    ] {
        let o = owl(&home, &cwd, &["add", &text], None);
        assert_eq!(o.status.code(), Some(1), "{text}");
        assert!(
            err(&o).contains(field) && err(&o).contains("array of strings"),
            "{}",
            err(&o)
        );
    }
    assert!(!home.join("contacts").exists());
}

#[test]
fn library_remove_resolves_in_scope_only_and_returns_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (root, sub) = fixture_repo(tmp.path());
    overlay(&home, 3, "Owen", "manual");
    overlay(&home, 1, "", "auto"); // overlay for local Martin
    // Global scope does not see Martin (overlay is not a contact).
    let e = ContactBook::remove(&home, &sub, "global", "Martin")
        .unwrap_err()
        .to_string();
    assert!(e.contains("no contact matches"), "{e}");
    assert!(
        home.join("contacts")
            .join(format!("{}.json", fp(1)))
            .exists()
    );
    // Local scope: file + overlay removed, paths returned.
    let (name, paths) = ContactBook::remove(&home, &sub, "local", &fp(1)).unwrap();
    assert_eq!(name, "Martin");
    assert_eq!(
        paths,
        [
            root.join(".agents/peers/martin.json"),
            home.join("contacts").join(format!("{}.json", fp(1))),
        ]
    );
    assert!(!paths[0].exists() && !paths[1].exists());
    // Global: Owen.
    let (name, paths) = ContactBook::remove(&home, &sub, "global", "owen").unwrap();
    assert_eq!(name, "Owen");
    assert_eq!(
        paths,
        [home.join("contacts").join(format!("{}.json", fp(3)))]
    );
    // Outside a repo, local is an error before anything is resolved.
    let cwd = plain_dir(tmp.path());
    let e = ContactBook::remove(&home, &cwd, "local", "Mason")
        .unwrap_err()
        .to_string();
    assert!(e.contains("git repository"), "{e}");
    assert!(root.join(".agents/peers/mason.json").exists());
}

// ---------- `contact list --json` feeds the /owlpost:contacts picker ----------

#[test]
fn list_json_carries_picker_fields_for_both_scopes() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    // One local contact without a policy...
    let peers = root.join(".agents").join("peers");
    std::fs::create_dir_all(&peers).unwrap();
    std::fs::write(
        peers.join("martin.json"),
        peer_json("Martin", "martin@company.com", 1),
    )
    .unwrap();
    // ...and one global contact with a policy.
    overlay(&home, 3, "Owen", "auto");

    let o = owl(&home, &root, &["contact", "list", "--json"], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
    let list = v.as_array().expect("a JSON array");
    assert_eq!(list.len(), 2, "{v}");

    for c in list {
        for key in ["name", "fingerprint", "source", "emails", "endpoints"] {
            assert!(c.get(key).is_some(), "{key} missing in {c}");
        }
        assert!(c["emails"].is_array() && c["endpoints"].is_array(), "{c}");
    }
    let by_name = |n: &str| {
        list.iter()
            .find(|c| c["name"] == n)
            .unwrap_or_else(|| panic!("{n} not listed: {v}"))
    };
    let martin = by_name("Martin");
    assert_eq!(martin["fingerprint"], fp(1));
    assert_eq!(martin["source"], "local");
    assert_eq!(martin["emails"], serde_json::json!(["martin@company.com"]));
    assert_eq!(
        martin["endpoints"],
        serde_json::json!(["martin.example.org:7411"])
    );
    // No policy → the key is absent; the picker renders it as `-`.
    assert!(martin.get("policy").is_none(), "{martin}");

    let owen = by_name("Owen");
    assert_eq!(owen["fingerprint"], fp(3));
    assert_eq!(owen["source"], "global");
    assert_eq!(owen["emails"], serde_json::json!(["other@example.org"]));
    assert_eq!(
        owen["endpoints"],
        serde_json::json!(["other.example.org:1"])
    );
    assert_eq!(owen["policy"]["mode"], "auto");
}

// ---------------------------------------------------------------- Key standing

/// `key_standing` answers "how did this key get into my book?" for every shape a
/// fingerprint can have in the merged book, and each of the three §5 wordings is pinned
/// verbatim. The fixtures take the inconvenient shape on purpose: one key that has a repo
/// peer file *and* a global overlay, and one key whose only file is a nameless overlay.
#[test]
fn key_standing_follows_the_merged_scope_and_pins_the_three_wordings() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (_root, sub) = fixture_repo(tmp.path());
    // seed 3: global only. seed 1: repo peer file (Martin) *and* a global policy overlay,
    // which §5 merges to `source: "local"`. seed 4: a bare overlay, no name.
    overlay(&home, 3, "Owen", "manual");
    overlay(&home, 1, "Wrong Name", "auto");
    overlay(&home, 4, "", "auto");
    let book = ContactBook::load(&home, &sub).unwrap();

    assert_eq!(
        key_standing(&book, &fp(3)),
        KeyStanding::Contact {
            name: "Owen".into(),
            source: "global".into()
        }
    );
    assert_eq!(
        key_standing(&book, &fp(2)),
        KeyStanding::Contact {
            name: "Mason".into(),
            source: "local".into()
        },
        "a repo peer file with no overlay"
    );
    assert_eq!(
        key_standing(&book, &fp(1)),
        KeyStanding::Contact {
            name: "Martin".into(),
            source: "local".into()
        },
        "overlay + repo file for one key: local wins, the repo name wins"
    );
    // A key no contact file holds any more: the contact was removed after the question came.
    assert_eq!(key_standing(&book, &fp(9)), KeyStanding::Unknown);
    assert_eq!(
        key_standing(&book, "owl:notafingerprint"),
        KeyStanding::Unknown
    );
    // A bare overlay still counts as a contact, with an empty name.
    assert_eq!(
        key_standing(&book, &fp(4)),
        KeyStanding::Contact {
            name: String::new(),
            source: "global".into()
        }
    );

    // The `owl inbox --json` values.
    assert_eq!(key_standing(&book, &fp(3)).as_str(), "contact");
    assert_eq!(key_standing(&book, &fp(1)).as_str(), "contact");
    assert_eq!(key_standing(&book, &fp(4)).as_str(), "contact");
    assert_eq!(key_standing(&book, &fp(9)).as_str(), "unknown");

    // The three wordings, verbatim.
    assert_eq!(
        key_standing(&book, &fp(1)).wording(&fp(1)),
        "known key: contact \"Martin\" — repo peer file (.agents/peers/, reviewed in a PR)"
    );
    assert_eq!(
        key_standing(&book, &fp(3)).wording(&fp(3)),
        "known key: contact \"Owen\" — added by hand (global book; fingerprint not verified through a PR)"
    );
    assert_eq!(
        key_standing(&book, &fp(9)).wording(&fp(9)),
        "unknown key: no longer in your contacts — deny, or re-add the peer file before allowing"
    );
    // The nameless overlay shows the fingerprint in place of the name.
    assert_eq!(
        key_standing(&book, &fp(4)).wording(&fp(4)),
        format!(
            "known key: contact \"{}\" — added by hand (global book; fingerprint not verified through a PR)",
            fp(4)
        )
    );
}
