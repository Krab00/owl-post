use std::process::Command;

fn owl() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
    c.env_remove("OWLPOST_HOME");
    c
}

#[test]
fn version() {
    let out = owl().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "owl 0.1.0");
}

#[test]
fn help_lists_all_subcommands() {
    let out = owl().arg("--help").output().unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    // Parse the "Commands:" block: first token of each indented line up to the blank line.
    let listed: Vec<&str> = help
        .split("Commands:\n")
        .nth(1)
        .expect("Commands: block")
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .filter(|t| *t != "help")
        .collect();
    let expected = [
        "init",
        "whoami",
        "card",
        "presence",
        "ping",
        "contact",
        "add",
        "allow",
        "deny",
        "ask",
        "request",
        "call",
        "status",
        "inbox",
        "show",
        "draft",
        "edit",
        "send",
        "reject",
        "route",
        "thread",
        "archive",
        "unarchive",
        "delete",
        "undo",
        "history",
        "watch",
        "daemon",
        "install",
        "uninstall",
        "doctor",
        "harness",
        "project",
        "mcp",
        "setup",
        "update",
    ];
    assert_eq!(listed, expected, "help:\n{help}");
    for flag in ["--home", "--json", "--quiet"] {
        assert!(help.contains(flag), "missing global flag {flag}");
    }
}

#[test]
fn whoami_on_empty_home_asks_for_init() {
    let home = tempfile::tempdir().unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("owl init"), "stderr: {err}");
    assert!(out.stdout.is_empty());
}

#[test]
fn whoami_honours_owlpost_home_env() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_owl"))
        .env("OWLPOST_HOME", home.path())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("owl init"));
}

#[test]
fn whoami_with_config_but_no_key_asks_for_init() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.json"), "{}").unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("owl init"));
}

#[test]
fn whoami_with_key_but_no_config_asks_for_init() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("key"), "k").unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("owl init"));
}

#[test]
fn whoami_with_config_and_garbage_key_fails_without_init_hint() {
    // A 1-byte key is a load error, not a missing identity.
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.json"), "{}").unwrap();
    std::fs::write(home.path().join("key"), "k").unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("32 bytes"), "stderr: {err}");
    assert!(!err.contains("owl init"), "stderr: {err}");
}

fn run_init(home: &std::path::Path) -> std::process::Output {
    owl()
        .args(["--home"])
        .arg(home)
        .args(["init", "--name", "Test", "--email", "t@example.org"])
        .output()
        .unwrap()
}

fn is_fingerprint(s: &str) -> bool {
    // ^owl:[a-z2-7]{16}$
    s.len() == 20
        && s.starts_with("owl:")
        && s[4..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

#[test]
fn init_creates_key_and_config_and_prints_fingerprint() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path().join("fresh"); // init must create the home dir
    let out = run_init(&home);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout: {stdout}");
    assert!(is_fingerprint(lines[0]), "stdout: {stdout}");
    assert_eq!(std::fs::read(home.join("key")).unwrap().len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join("key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("config.json")).unwrap()).unwrap();
    assert_eq!(cfg["name"], "Test");
    assert_eq!(cfg["emails"], serde_json::json!(["t@example.org"]));
}

#[test]
fn init_accepts_repeated_email_and_no_name() {
    let home = tempfile::tempdir().unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .args(["init", "--email", "a@x.org", "--email", "b@x.org"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(cfg["name"], "");
    assert_eq!(cfg["emails"], serde_json::json!(["a@x.org", "b@x.org"]));
}

#[test]
fn init_twice_refuses_and_keeps_key() {
    let home = tempfile::tempdir().unwrap();
    assert!(run_init(home.path()).status.success());
    let key = std::fs::read(home.path().join("key")).unwrap();
    let cfg = std::fs::read(home.path().join("config.json")).unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .args(["init", "--name", "Other"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already exists"), "stderr: {err}");
    assert!(out.stdout.is_empty());
    assert_eq!(std::fs::read(home.path().join("key")).unwrap(), key);
    assert_eq!(
        std::fs::read(home.path().join("config.json")).unwrap(),
        cfg,
        "config untouched"
    );
}

#[test]
fn whoami_json_after_init() {
    let home = tempfile::tempdir().unwrap();
    let init = run_init(home.path());
    assert!(init.status.success());
    let fp = String::from_utf8_lossy(&init.stdout).trim().to_string();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .args(["whoami", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let obj = v.as_object().unwrap();
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["endpoints", "fingerprint", "name", "pubkey"]);
    assert_eq!(v["fingerprint"], fp);
    assert_eq!(v["name"], "Test");
    assert_eq!(v["endpoints"], serde_json::json!([]));
    let pk = v["pubkey"].as_str().unwrap();
    assert!(pk.starts_with("ed25519:"), "{pk}");
    assert_eq!(pk.len(), 8 + 43, "{pk}");
    assert!(!pk.ends_with('='), "{pk}");
}

#[test]
fn whoami_text_after_init() {
    let home = tempfile::tempdir().unwrap();
    let init = run_init(home.path());
    let fp = String::from_utf8_lossy(&init.stdout).trim().to_string();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!("fingerprint: {fp}\n")),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("pubkey: ed25519:"), "stdout: {stdout}");
    assert!(stdout.contains("name: Test\n"), "stdout: {stdout}");
    assert!(stdout.contains("endpoints: "), "stdout: {stdout}");
}

fn seed_endpoints(home: &std::path::Path) {
    let path = home.join("config.json");
    let mut cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    cfg["endpoints"] = serde_json::json!(["a:1", "b:2"]);
    std::fs::write(&path, serde_json::to_vec(&cfg).unwrap()).unwrap();
}

#[test]
fn whoami_prints_configured_endpoints() {
    let home = tempfile::tempdir().unwrap();
    assert!(run_init(home.path()).status.success());
    seed_endpoints(home.path());
    let text = owl()
        .args(["--home"])
        .arg(home.path())
        .arg("whoami")
        .output()
        .unwrap();
    assert_eq!(text.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(
        stdout.lines().any(|l| l == "endpoints: a:1, b:2"),
        "stdout: {stdout}"
    );
    let json = owl()
        .args(["--home"])
        .arg(home.path())
        .args(["whoami", "--json"])
        .output()
        .unwrap();
    assert_eq!(json.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(v["endpoints"], serde_json::json!(["a:1", "b:2"]));
}

/// `owl card` exists now; without an identity it refuses like `whoami` does.
#[test]
fn card_without_identity_exits_1() {
    let home = tempfile::tempdir().unwrap();
    for args in [vec!["card"], vec!["card", "someone"]] {
        let out = owl()
            .args(["--home"])
            .arg(home.path())
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("owl init"), "{args:?}: {err}");
        assert!(!err.contains("not implemented"), "{args:?}: {err}");
    }
}

#[test]
fn unknown_subcommand_is_usage_error() {
    let out = owl().arg("bogus").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

// ---- owl contact ----

#[test]
fn contact_export_matches_whoami() {
    let home = tempfile::tempdir().unwrap();
    // Random name so a hard-coded "Test" in the binary cannot pass.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("Peer-{nonce}");
    let email = format!("p{nonce}@example.org");
    let init = owl()
        .args(["--home"])
        .arg(home.path())
        .args(["init", "--name", &name, "--email", &email])
        .output()
        .unwrap();
    assert!(init.status.success());
    seed_endpoints(home.path());
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .args(["contact", "export"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["emails", "endpoints", "name", "pubkey"]);
    assert_eq!(v["name"], name);
    assert_eq!(v["emails"], serde_json::json!([email]));
    assert_eq!(v["endpoints"], serde_json::json!(["a:1", "b:2"]));
    let who = owl()
        .args(["--home"])
        .arg(home.path())
        .args(["whoami", "--json"])
        .output()
        .unwrap();
    let w: serde_json::Value = serde_json::from_slice(&who.stdout).unwrap();
    assert!(w["pubkey"].as_str().unwrap().starts_with("ed25519:"));
    assert_eq!(v["pubkey"], w["pubkey"]);
    assert_eq!(v["name"], w["name"]);
}

#[test]
fn contact_export_without_init_asks_for_init() {
    let home = tempfile::tempdir().unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .args(["contact", "export"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("owl init"));
    assert!(out.stdout.is_empty());
}

fn peer(seed: u8) -> (String, String) {
    use owlpost::identity::{Identity, fingerprint, pubkey_string};
    let vk = Identity::from_seed([seed; 32]).verifying_key();
    (pubkey_string(&vk), fingerprint(&vk))
}

/// Fixture repo with two peers; returns (nested subdir, [(name, fingerprint); 2]).
fn fixture_repo(base: &std::path::Path) -> (std::path::PathBuf, [(String, String); 2]) {
    let root = base.join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let peers = root.join(".agents").join("peers");
    std::fs::create_dir_all(&peers).unwrap();
    let mut out = Vec::new();
    for (name, seed) in [("Martin", 1u8), ("Mason", 2)] {
        let (pk, fp) = peer(seed);
        std::fs::write(
            peers.join(format!("{}.json", name.to_lowercase())),
            serde_json::json!({
                "name": name, "emails": [format!("{}@x.org", name.to_lowercase())],
                "pubkey": pk, "endpoints": [],
            })
            .to_string(),
        )
        .unwrap();
        out.push((name.to_string(), fp));
    }
    let sub = root.join("src").join("deep");
    std::fs::create_dir_all(&sub).unwrap();
    (sub, [out.remove(0), out.remove(0)])
}

#[test]
fn contact_list_in_fixture_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home"); // no init needed for list
    let (sub, peers) = fixture_repo(tmp.path());
    let list = |extra: &[&str]| {
        let out = owl()
            .args(["--home"])
            .arg(&home)
            .args(["contact", "list"])
            .args(extra)
            .current_dir(&sub)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let rows = |s: &str| -> Vec<Vec<String>> {
        s.lines()
            .skip(1)
            .map(|l| l.split_whitespace().map(str::to_string).collect())
            .collect()
    };
    let text = list(&[]);
    let header: Vec<&str> = text.lines().next().unwrap().split_whitespace().collect();
    assert_eq!(header, ["NAME", "FINGERPRINT", "SOURCE", "POLICY"]);
    let r = rows(&text);
    assert_eq!(r.len(), 2, "stdout: {text}");
    for (row, (name, fp)) in r.iter().zip(&peers) {
        assert_eq!(row, &[name.clone(), fp.clone(), "local".into(), "-".into()]);
    }
    // Overlay for Martin → policy column shows auto; Mason unchanged.
    let (pk1, fp1) = peer(1);
    std::fs::create_dir_all(home.join("contacts")).unwrap();
    std::fs::write(
        home.join("contacts").join(format!("{fp1}.json")),
        serde_json::json!({"pubkey": pk1, "policy": {"mode": "auto"}, "source": "global"})
            .to_string(),
    )
    .unwrap();
    let text = list(&[]);
    let r = rows(&text);
    assert_eq!(r.len(), 2, "stdout: {text}");
    assert_eq!(r[0], ["Martin", &peers[0].1, "local", "auto"]);
    assert_eq!(r[1], ["Mason", &peers[1].1, "local", "-"]);
    // --json
    let v: serde_json::Value = serde_json::from_str(&list(&["--json"])).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["name"], "Martin");
    assert_eq!(arr[0]["fingerprint"], fp1);
    assert_eq!(arr[0]["source"], "local");
    assert_eq!(arr[0]["policy"]["mode"], "auto");
    assert_eq!(arr[1]["name"], "Mason");
    assert!(arr[1].get("policy").is_none(), "{v}");
}

#[test]
fn contact_list_outside_repo_lists_local_only() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (pk3, fp3) = peer(3);
    std::fs::create_dir_all(home.join("contacts")).unwrap();
    std::fs::write(
        home.join("contacts").join(format!("{fp3}.json")),
        serde_json::json!({"name": "Owen", "pubkey": pk3, "policy": {"mode": "never"}}).to_string(),
    )
    .unwrap();
    let out = owl()
        .args(["--home"])
        .arg(&home)
        .args(["contact", "list"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8(out.stdout).unwrap();
    let rows: Vec<Vec<&str>> = text
        .lines()
        .skip(1)
        .map(|l| l.split_whitespace().collect())
        .collect();
    assert_eq!(rows, [["Owen", fp3.as_str(), "global", "never"]]);
}

#[test]
fn contact_show_resolves_and_reports_ambiguity() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (sub, peers) = fixture_repo(tmp.path());
    let show = |peer: &str| {
        owl()
            .args(["--home"])
            .arg(&home)
            .args(["contact", "show", peer])
            .current_dir(&sub)
            .output()
            .unwrap()
    };
    let ok = show("mason@x.org");
    assert_eq!(ok.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(v["name"], "Mason");
    assert_eq!(v["fingerprint"], peers[1].1);
    assert_eq!(v["source"], "local");
    let amb = show("ma");
    assert_eq!(amb.status.code(), Some(1));
    let err = String::from_utf8_lossy(&amb.stderr);
    assert!(
        err.contains("Martin") && err.contains("Mason"),
        "stderr: {err}"
    );
    assert!(amb.stdout.is_empty());
    let none = show("nobody");
    assert_eq!(none.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&none.stderr).contains("nobody"));
}

#[test]
fn contact_list_warns_on_malformed_peer_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (sub, _) = fixture_repo(tmp.path());
    let bad = tmp.path().join("repo/.agents/peers/bad.json");
    std::fs::write(&bad, "{oops").unwrap();
    let out = owl()
        .args(["--home"])
        .arg(&home)
        .args(["contact", "list"])
        .current_dir(&sub)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.starts_with("owl: warning: skipping "), "stderr: {err}");
    assert!(err.contains("bad.json"), "stderr: {err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 3);
}

#[test]
fn contact_export_with_config_only_or_key_only_asks_for_init() {
    let cfg_only = tempfile::tempdir().unwrap();
    std::fs::write(cfg_only.path().join("config.json"), "{}").unwrap();
    let key_only = tempfile::tempdir().unwrap();
    assert!(run_init(key_only.path()).status.success());
    std::fs::remove_file(key_only.path().join("config.json")).unwrap();
    for home in [cfg_only.path(), key_only.path()] {
        let out = owl()
            .args(["--home"])
            .arg(home)
            .args(["contact", "export"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", home.display());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("owl init"), "stderr: {err}");
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn contact_list_empty_book_prints_header_only() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    // No repo (no .git above), no local contacts dir.
    let cwd = tmp.path().join("plain");
    std::fs::create_dir_all(&cwd).unwrap();
    let out = owl()
        .args(["--home"])
        .arg(&home)
        .args(["contact", "list"])
        .current_dir(&cwd)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "NAME                 FINGERPRINT          SOURCE POLICY\n"
    );
    assert!(out.stderr.is_empty());
    let json = owl()
        .args(["--home"])
        .arg(&home)
        .args(["contact", "list", "--json"])
        .current_dir(&cwd)
        .output()
        .unwrap();
    assert_eq!(json.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&json.stdout).trim(), "[]");
    assert!(json.stderr.is_empty());
}

#[test]
fn contact_list_ignores_non_json_extensions_without_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (sub, _) = fixture_repo(tmp.path());
    let peers = tmp.path().join("repo/.agents/peers");
    std::fs::write(peers.join("junk.JSON"), "{not json").unwrap();
    std::fs::write(peers.join("junk.json.bak"), "{not json").unwrap();
    std::fs::write(peers.join("junk.bak"), "{not json").unwrap();
    let out = owl()
        .args(["--home"])
        .arg(&home)
        .args(["contact", "list"])
        .current_dir(&sub)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(
        out.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 3);
}

#[test]
fn contact_list_rows_follow_filename_order() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let peers = root.join(".agents/peers");
    std::fs::create_dir_all(&peers).unwrap();
    let (pk1, fp1) = peer(1);
    let (pk2, fp2) = peer(2);
    std::fs::write(
        peers.join("a.json"),
        serde_json::json!({"name": "Zoe", "pubkey": pk1}).to_string(),
    )
    .unwrap();
    std::fs::write(
        peers.join("b.json"),
        serde_json::json!({"name": "Adam", "pubkey": pk2}).to_string(),
    )
    .unwrap();
    let out = owl()
        .args(["--home"])
        .arg(&home)
        .args(["contact", "list"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8(out.stdout).unwrap();
    let rows: Vec<Vec<&str>> = text
        .lines()
        .skip(1)
        .map(|l| l.split_whitespace().collect())
        .collect();
    assert_eq!(
        rows,
        [
            ["Zoe", fp1.as_str(), "local", "-"],
            ["Adam", fp2.as_str(), "local", "-"]
        ]
    );
}

#[test]
fn setup_dry_run_lists_steps_and_skips_init_when_key_exists() {
    let home = tempfile::tempdir().unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .args([
            "setup",
            "--dry-run",
            "--plugin-source",
            "/repo/plugins/claude-code",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("would run: owl init"), "{s}");
    assert!(s.contains("would run: owl install"), "{s}");
    assert!(
        s.contains("claude plugin marketplace add /repo/plugins/claude-code")
            || s.contains("claude not on PATH"),
        "{s}"
    );
    std::fs::write(home.path().join("key"), b"x").unwrap();
    let out = owl()
        .args(["--home"])
        .arg(home.path())
        .args(["setup", "--dry-run"])
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("owl init skipped"), "{s}");
    assert!(!s.contains("would run: owl init"), "{s}");
}

#[test]
fn update_dry_run_lists_steps() {
    let out = owl()
        .args(["update", "--dry-run", "--source", "/repo"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("cargo install --path /repo --locked --root <tmpdir>"),
        "{s}"
    );
    // The file to be replaced is the running binary, named before the unit step.
    let me = std::fs::canonicalize(env!("CARGO_BIN_EXE_owl")).unwrap();
    assert!(
        s.contains(&format!("would replace {}\n", me.display())),
        "{s}"
    );
    assert!(
        s.find("would replace").unwrap() < s.find("owl uninstall && owl install").unwrap(),
        "{s}"
    );
    assert!(s.contains("owl uninstall && owl install"), "{s}");
    // `OWLPOST_UPDATE_ACTIVE`: empty means unset (the running binary), a path is used as is.
    let out = owl()
        .env("OWLPOST_UPDATE_ACTIVE", "")
        .args(["update", "--dry-run", "--source", "/repo"])
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains(&format!("would replace {}\n", me.display())),
        "{s}"
    );
    let out = owl()
        .env("OWLPOST_UPDATE_ACTIVE", "/elsewhere/owl")
        .args(["update", "--dry-run", "--source", "/repo"])
        .output()
        .unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("would replace /elsewhere/owl\n"), "{s}");
    let out = owl().args(["update", "--dry-run"]).output().unwrap();
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("install.sh -o"), "{s}");
    assert!(s.contains("--prefix \"<tmpdir>\""), "{s}");
    assert!(
        s.contains(&format!("would replace {}\n", me.display())),
        "{s}"
    );
}
