//! `owl doctor` prints one line per check and exits 0 with a running daemon, exits 1 with
//! `fail` on the daemon line once the daemon is stopped. The pull line reads the pull loop's
//! `daemon.status`. The mcp line asks a fake `claude` on a PATH of its
//! own whether the `owl` server is registered; it warns, never fails.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use owlpost::daemon;
use owlpost::envelope::{now_unix, unix_to_rfc3339};
use owlpost::pull::{self, PullStatus, STATUS_FILE};

use common::{fake_claude, spawn_daemon_with, write_contact};

const MCP_ADD: &str = "claude mcp add --scope user owl -- owl mcp";

/// The pull loop's first tick runs right after spawn; bounded wait (≤ 5 s) for its status file.
fn wait_for_status(home: &Path) {
    let start = std::time::Instant::now();
    while !home.join(STATUS_FILE).exists() {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "pull loop never wrote {STATUS_FILE}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// `owl doctor` with `PATH` = exactly `bin` (a `fake_claude` dir, or an empty one).
fn owl_on(home: &Path, bin: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
    c.env_remove("OWLPOST_HOME")
        // No daemon unit under this HOME: the `binary` check sees PATH only.
        .env("HOME", "/nonexistent-owlpost-home")
        .env("PATH", bin)
        .arg("--home")
        .arg(home);
    c
}

/// Puts the built `owl` on `bin` (a `fake_claude` dir), so the `binary` check is `ok`.
fn with_owl(bin: &Path) {
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_owl"), bin.join("owl")).unwrap();
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// The `<status> <name>:` prefix of every line, in order.
fn heads(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .map(|l| {
            let (status, rest) = l.split_once(' ').expect("status");
            let name = rest.trim_start().split_once(':').expect("name").0;
            (status.to_string(), name.to_string())
        })
        .collect()
}

fn line<'a>(stdout: &'a str, name: &str) -> &'a str {
    stdout
        .lines()
        .find(|l| l.split_whitespace().nth(1) == Some(&format!("{name}:")))
        .unwrap_or_else(|| panic!("no {name} line in:\n{stdout}"))
}

/// Splits `ok   pull: last pull <N>s ago, <rest>` into (`N`, `rest`).
fn pull_age(pull_line: &str) -> (u64, &str) {
    let tail = pull_line
        .strip_prefix("ok   pull: last pull ")
        .unwrap_or_else(|| panic!("pull line {pull_line:?}"));
    let (age, rest) = tail
        .split_once("s ago, ")
        .unwrap_or_else(|| panic!("pull line {pull_line:?}"));
    (age.parse().unwrap(), rest)
}

/// Only the `fake` harness is configured, so the check passes offline on any machine.
fn config_for_doctor(cfg: &mut owlpost::config::Config) {
    cfg.harnesses.retain(|k, _| k == "fake");
    cfg.responder.harness = "fake".into();
    cfg.endpoints = vec!["127.0.0.1:7411".into()];
}

#[tokio::test(flavor = "multi_thread")]
async fn doctor_reports_each_check() {
    // A fake `claude` reporting the `owl` MCP server as registered, on a PATH of its own.
    let fake = tempfile::tempdir().unwrap();
    let (bin, _) = fake_claude(fake.path(), 0);
    with_owl(&bin);
    // A local relay (never n0's) so the iroh line is the connected `ok` form.
    let (relay_url, _relay) = common::relay().await;
    let d = spawn_daemon_with(1, &[], |cfg| {
        config_for_doctor(cfg);
        cfg.relay_urls = Some(vec![relay_url.clone()]);
    })
    .await;
    common::wait_online(&d).await;
    daemon::write_addr_file(d.home(), d.addr).unwrap();
    wait_for_status(d.home());

    let out = owl_on(d.home(), &bin).arg("doctor").output().unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout:\n{stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        heads(&stdout),
        [
            ("ok", "key"),
            ("ok", "config"),
            ("ok", "endpoints"),
            ("ok", "harness"),
            ("ok", "binary"),
            ("ok", "mcp"),
            ("ok", "daemon"),
            ("ok", "iroh"),
            ("ok", "pull"),
        ]
        .map(|(s, n)| (s.to_string(), n.to_string())),
        "{stdout}"
    );
    assert!(line(&stdout, "key").contains(&d.fp()), "{stdout}");
    // PATH's `owl` is this very file (through the symlink) → ok naming it.
    assert_eq!(
        line(&stdout, "binary"),
        format!(
            "ok   binary: {}",
            std::fs::canonicalize(env!("CARGO_BIN_EXE_owl"))
                .unwrap()
                .display()
        ),
        "{stdout}"
    );
    // The mcp line, verbatim, with the fake `claude mcp get owl` exiting 0.
    assert_eq!(
        line(&stdout, "mcp"),
        "ok   mcp: owl registered in Claude Code (user scope)",
        "{stdout}"
    );
    // The connected iroh line, verbatim: iroh's short id (hex of the first five key
    // bytes) and the relay URL the daemon reported in its card.
    let short: String = d.id.verifying_key().as_bytes()[..5]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        line(&stdout, "iroh"),
        format!("ok   iroh: {short}, relay {relay_url}"),
        "{stdout}"
    );
    assert!(line(&stdout, "config").contains("config.json"), "{stdout}");
    assert!(
        line(&stdout, "endpoints").contains("127.0.0.1:7411 -> 127.0.0.1"),
        "{stdout}"
    );
    assert!(line(&stdout, "harness").contains("fake: "), "{stdout}");
    assert!(
        line(&stdout, "harness").contains("fake-harness.sh"),
        "{stdout}"
    );
    assert!(
        line(&stdout, "daemon").contains(&format!("reachable at {}", d.addr)),
        "{stdout}"
    );
    assert!(line(&stdout, "daemon").contains(&d.fp()), "{stdout}");
    // The daemon's pull loop ran on start-up and wrote `daemon.status`, so the pull line is
    // already `ok`; the first assertion above waited for that file.
    assert!(
        line(&stdout, "pull").starts_with("ok   pull: last pull "),
        "{stdout}"
    );
    assert!(
        line(&stdout, "pull").ends_with("0 open ask(s), 0 peer(s) probed"),
        "{stdout}"
    );
    assert!(stderr.is_empty(), "{stderr}");

    // `claude mcp get owl` exits 1 → warn ending with the fix; no `claude` on
    // PATH → `warn mcp: claude not on PATH`. Neither is a failure: exit stays 0, and --json
    // carries the same check.
    let missing = tempfile::tempdir().unwrap();
    let (missing_bin, log) = fake_claude(missing.path(), 1);
    let out = owl_on(d.home(), &missing_bin)
        .arg("doctor")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{stdout}\n{stderr}");
    assert!(line(&stdout, "mcp").starts_with("warn mcp: "), "{stdout}");
    assert!(line(&stdout, "mcp").ends_with(MCP_ADD), "{stdout}");
    // That PATH holds no `owl`: the binary check warns and names this file, exit stays 0.
    assert_eq!(
        line(&stdout, "binary"),
        format!(
            "warn binary: no owl on PATH, this owl is {}",
            std::fs::canonicalize(env!("CARGO_BIN_EXE_owl"))
                .unwrap()
                .display()
        ),
        "{stdout}"
    );
    // Another `owl` first on PATH: warn naming both, in text and in --json.
    let other = tempfile::tempdir().unwrap();
    let other_owl = other.path().join("owl");
    std::fs::write(&other_owl, "#!/bin/sh\n").unwrap();
    let two = std::env::join_paths([other.path(), &bin]).unwrap();
    let out = owl_on(d.home(), Path::new(&two))
        .arg("doctor")
        .output()
        .unwrap();
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert_eq!(
        line(&stdout, "binary"),
        format!(
            "warn binary: PATH resolves {}, this owl is {}",
            std::fs::canonicalize(&other_owl).unwrap().display(),
            std::fs::canonicalize(env!("CARGO_BIN_EXE_owl"))
                .unwrap()
                .display()
        ),
        "{stdout}"
    );
    let out = owl_on(d.home(), Path::new(&two))
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&text(&out).0).unwrap();
    let binary = json
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c.get("check").and_then(|v| v.as_str()) == Some("binary"))
        .expect("binary row in --json");
    assert_eq!(binary["status"], "warn");
    assert!(
        binary["detail"]
            .as_str()
            .unwrap()
            .starts_with("PATH resolves "),
        "{json}"
    );
    assert_eq!(
        common::fake_calls(&log),
        ["claude mcp get owl"],
        "doctor only asks, never registers"
    );
    let out = owl_on(d.home(), &missing_bin)
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_str(&text(&out).0).unwrap();
    let mcp = json
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c.get("check").and_then(|v| v.as_str()) == Some("mcp"))
        .expect("mcp check in --json");
    assert_eq!(mcp.get("status").and_then(|v| v.as_str()), Some("warn"));
    assert!(
        mcp.get("detail")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.ends_with(MCP_ADD)),
        "{mcp}"
    );
    let empty = tempfile::tempdir().unwrap();
    let out = owl_on(d.home(), empty.path())
        .arg("doctor")
        .output()
        .unwrap();
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert_eq!(
        line(&stdout, "mcp"),
        "warn mcp: claude not on PATH",
        "{stdout}"
    );
    assert_eq!(heads(&stdout)[5], ("warn".to_string(), "mcp".to_string()));
    let out = owl_on(d.home(), empty.path())
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_str(&text(&out).0).unwrap();
    let mcp = json
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c.get("check").and_then(|v| v.as_str()) == Some("mcp"))
        .expect("mcp check in --json");
    assert_eq!(mcp.get("status").and_then(|v| v.as_str()), Some("warn"));
    assert_eq!(
        mcp.get("detail").and_then(|v| v.as_str()),
        Some("claude not on PATH"),
        "{mcp}"
    );

    // Without the status file the last line is a warning, not a failure (exit 0).
    std::fs::remove_file(d.home().join(STATUS_FILE)).unwrap();
    let out = owl_on(d.home(), &bin).arg("doctor").output().unwrap();
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        line(&stdout, "pull").starts_with("warn pull: never pulled (no daemon.status)"),
        "{stdout}"
    );
    // The stale threshold is 2 × pull_interval_secs: an 11 s old pull is fine at the default
    // 60 s interval, stale at 5 s.
    let eleven_ago = unix_to_rfc3339(now_unix() - 11);
    pull::write_status(
        d.home(),
        &PullStatus {
            last_pull_at: eleven_ago,
            open_asks: 3,
            peers_probed: 2,
            peers: Default::default(),
        },
    )
    .unwrap();
    let out = owl_on(d.home(), &bin).arg("doctor").output().unwrap();
    // The age is measured at print time, so a slow runner may read 12s or more: pin the
    // prefix, the counters and a tolerant age window instead of the exact second.
    let (stdout, _) = text(&out);
    let (age, rest) = pull_age(line(&stdout, "pull"));
    assert!((11..=30).contains(&age), "age {age}s:\n{stdout}");
    assert_eq!(rest, "3 open ask(s), 2 peer(s) probed", "{stdout}");
    let mut cfg = owlpost::config::Config::load(d.home()).unwrap();
    cfg.pull_interval_secs = 5;
    cfg.save(d.home()).unwrap();
    let out = owl_on(d.home(), &bin).arg("doctor").output().unwrap();
    let (stdout, _) = text(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "warn is not a failure: {stdout}"
    );
    assert!(
        line(&stdout, "pull").starts_with("warn pull: last pull 11s ago (interval 5s)"),
        "{stdout}"
    );
    cfg.pull_interval_secs = 60;
    cfg.save(d.home()).unwrap();

    // --json: one object per check with the same content.
    let out = owl_on(d.home(), &bin)
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_str(&text(&out).0).unwrap();
    let arr = json.as_array().expect("array");
    let names: Vec<&str> = arr
        .iter()
        .map(|c| c.get("check").and_then(|v| v.as_str()).unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "key",
            "config",
            "endpoints",
            "harness",
            "binary",
            "mcp",
            "daemon",
            "iroh",
            "pull"
        ]
    );
    assert!(
        arr.iter()
            .all(|c| c.get("status").and_then(|v| v.as_str()) == Some("ok")),
        "{json}"
    );
    assert!(
        arr[6]
            .get("detail")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("reachable")),
        "{json}"
    );

    // Daemon stopped, nothing else changed: exit 1, fail on the daemon line only.
    d.running.shutdown();
    let addr = d.addr;
    let common::TestDaemon { dir, running, .. } = d;
    running.wait().await.unwrap();
    let home = dir.path();
    let out = owl_on(home, &bin).arg("doctor").output().unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout:\n{stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        heads(&stdout),
        [
            ("ok", "key"),
            ("ok", "config"),
            ("ok", "endpoints"),
            ("ok", "harness"),
            ("ok", "binary"),
            ("ok", "mcp"),
            ("fail", "daemon"),
            ("warn", "iroh"),
            ("ok", "pull"),
        ]
        .map(|(s, n)| (s.to_string(), n.to_string())),
        "{stdout}"
    );
    assert!(
        line(&stdout, "daemon").contains(&addr.to_string()),
        "{stdout}"
    );
    assert_eq!(
        line(&stdout, "iroh"),
        "warn iroh: unknown (daemon unreachable)"
    );
    assert!(stderr.contains("1 check(s) failed"), "{stderr}");
    // --json carries the failure too, still exit 1.
    let out = owl_on(home, &bin)
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_str(&text(&out).0).unwrap();
    assert_eq!(
        json.get(6)
            .and_then(|c| c.get("status"))
            .and_then(|v| v.as_str()),
        Some("fail")
    );

    // No daemon.addr at all: fail names the missing file.
    std::fs::remove_file(home.join("daemon.addr")).unwrap();
    let out = owl_on(home, &bin).arg("doctor").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(line(&text(&out).0, "daemon").contains("daemon.addr missing"));
}

#[tokio::test(flavor = "multi_thread")]
async fn doctor_fails_on_foreign_daemon_and_missing_pieces() {
    // A fake `claude` reporting the `owl` MCP server as registered, on a PATH of its own.
    let fake = tempfile::tempdir().unwrap();
    let (bin, _) = fake_claude(fake.path(), 0);
    // daemon.addr points at somebody else's daemon: reachable, but not ours (pin mismatch).
    let mine = spawn_daemon_with(1, &[], config_for_doctor).await;
    let other = spawn_daemon_with(2, &[], config_for_doctor).await;
    daemon::write_addr_file(mine.home(), other.addr).unwrap();
    let out = owl_on(mine.home(), &bin).arg("doctor").output().unwrap();
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    let daemon_line = line(&stdout, "daemon");
    assert!(daemon_line.starts_with("fail"), "{stdout}");
    // Our own key is the TLS pin, so a foreign daemon is refused during the handshake.
    assert!(daemon_line.contains("invalid peer certificate"), "{stdout}");
    assert!(daemon_line.contains(&other.addr.to_string()), "{stdout}");
    mine.running.shutdown();
    other.running.shutdown();

    // Empty home: key fails, config warns (defaults), endpoints ok (iroh reaches the daemon),
    // harness lines for the default set, daemon fails, iroh unknown, pull warns — and the
    // harness line for `fake` still resolves.
    let empty = tempfile::tempdir().unwrap();
    let out = owl_on(empty.path(), &bin).arg("doctor").output().unwrap();
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(line(&stdout, "key").starts_with("fail"), "{stdout}");
    assert!(line(&stdout, "config").starts_with("warn"), "{stdout}");
    assert_eq!(
        line(&stdout, "endpoints"),
        "ok   endpoints: none configured (peers reach this daemon over iroh)",
        "{stdout}"
    );
    assert_eq!(
        line(&stdout, "iroh"),
        "warn iroh: unknown (daemon unreachable)"
    );
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with("ok   harness: fake: ")),
        "{stdout}"
    );
    assert!(line(&stdout, "daemon").starts_with("fail"), "{stdout}");
    assert!(line(&stdout, "pull").starts_with("warn"), "{stdout}");

    // Broken config → fail on the config line; an unresolvable endpoint → fail.
    let broken = tempfile::tempdir().unwrap();
    std::fs::write(broken.path().join("config.json"), "{nope").unwrap();
    let out = owl_on(broken.path(), &bin).arg("doctor").output().unwrap();
    let (stdout, _) = text(&out);
    assert!(line(&stdout, "config").starts_with("fail"), "{stdout}");
    assert!(line(&stdout, "config").contains("parsing"), "{stdout}");
    let mut cfg = owlpost::config::Config::default();
    config_for_doctor(&mut cfg);
    cfg.endpoints = vec!["noport".into()];
    cfg.save(broken.path()).unwrap();
    write_contact(broken.path(), &common::Peer::new(&common::id(3), "X", None));
    let out = owl_on(broken.path(), &bin).arg("doctor").output().unwrap();
    let (stdout, _) = text(&out);
    assert!(line(&stdout, "config").starts_with("ok"), "{stdout}");
    assert!(
        line(&stdout, "endpoints").starts_with("fail endpoints: noport: "),
        "{stdout}"
    );
}

/// Through the real binary: a daemon unit under a temp `HOME` naming another
/// program gives `warn binary: daemon unit runs /opt/other/owl` in text and `--json`; a
/// PATH without `owl` gives the `no owl on PATH` warn in both modes; a unit naming this
/// file with this file on PATH gives the single `ok` row.
#[test]
fn doctor_binary_check_reads_the_daemon_unit_in_text_and_json() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("owlpost");
    let user_home = dir.path().join("user");
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let me = std::fs::canonicalize(env!("CARGO_BIN_EXE_owl")).unwrap();
    let write_unit = |program: &str| {
        let (path, text) = if cfg!(target_os = "macos") {
            (
                user_home.join("Library/LaunchAgents/dev.owlpost.owl.plist"),
                format!(
                    "<?xml version=\"1.0\"?><plist><dict><key>ProgramArguments</key><array><string>{program}</string><string>daemon</string></array></dict></plist>"
                ),
            )
        } else {
            (
                user_home.join(".config/systemd/user/owlpost.service"),
                format!("[Service]\nExecStart=\"{program}\" daemon --home \"/h\"\n"),
            )
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let doctor = |bin: &Path, json: bool| -> String {
        let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
        c.env_remove("OWLPOST_HOME")
            .env("HOME", &user_home)
            .env("PATH", bin)
            .arg("--home")
            .arg(&home);
        if json {
            c.arg("--json");
        }
        let out = c.arg("doctor").output().unwrap();
        text(&out).0
    };
    let binary_rows = |stdout: &str| -> Vec<(String, String)> {
        let json: serde_json::Value = serde_json::from_str(stdout).unwrap();
        json.as_array()
            .unwrap()
            .iter()
            .filter(|c| c["check"] == "binary")
            .map(|c| {
                (
                    c["status"].as_str().unwrap().to_string(),
                    c["detail"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };

    // Unit names another program, no owl on PATH: two warn rows, PATH first.
    write_unit("/opt/other/owl");
    let stdout = doctor(&empty, false);
    let rows: Vec<&str> = stdout
        .lines()
        .filter(|l| l.split_whitespace().nth(1) == Some("binary:"))
        .collect();
    assert_eq!(
        rows,
        [
            format!("warn binary: no owl on PATH, this owl is {}", me.display()).as_str(),
            "warn binary: daemon unit runs /opt/other/owl",
        ],
        "{stdout}"
    );
    assert_eq!(
        binary_rows(&doctor(&empty, true)),
        [
            (
                "warn".to_string(),
                format!("no owl on PATH, this owl is {}", me.display())
            ),
            (
                "warn".to_string(),
                "daemon unit runs /opt/other/owl".to_string()
            ),
        ]
    );
    // This file on PATH, unit still wrong: only the unit warn, in both modes.
    let with_owl = dir.path().join("withowl");
    std::fs::create_dir_all(&with_owl).unwrap();
    std::os::unix::fs::symlink(&me, with_owl.join("owl")).unwrap();
    let stdout = doctor(&with_owl, false);
    assert_eq!(
        line(&stdout, "binary"),
        "warn binary: daemon unit runs /opt/other/owl",
        "{stdout}"
    );
    assert_eq!(
        binary_rows(&doctor(&with_owl, true)),
        [(
            "warn".to_string(),
            "daemon unit runs /opt/other/owl".to_string()
        )]
    );
    // Unit names this file: the single ok row.
    write_unit(&me.display().to_string());
    let stdout = doctor(&with_owl, false);
    assert_eq!(
        line(&stdout, "binary"),
        format!("ok   binary: {}", me.display()),
        "{stdout}"
    );
    assert_eq!(
        binary_rows(&doctor(&with_owl, true)),
        [("ok".to_string(), me.display().to_string())]
    );
}
