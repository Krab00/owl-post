//! `owl watch` returns the id of an unseen inbox record as it arrives, exit 4 on timeout.

use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use owlpost::spool::{Dir, Record, Spool};

fn owl(home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
    c.env_remove("OWLPOST_HOME")
        .arg("--home")
        .arg(home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn record(seen: bool) -> Record {
    Record {
        raw: r#"{"body":{"question":"ZEBRA"}}"#.into(),
        sig: String::new(),
        state: "pending".into(),
        seen,
        received_at: "2026-09-01T10:00:00Z".into(),
        draft: None,
        meta: serde_json::json!({}),
    }
}

/// Waits until `start + limit` for the child to exit (elapsed is measured from `start`);
/// kills it and panics otherwise.
fn wait_bounded(mut child: Child, start: Instant, limit: Duration) -> (Output, Duration) {
    loop {
        if child.try_wait().unwrap().is_some() {
            let elapsed = start.elapsed();
            return (child.wait_with_output().unwrap(), elapsed);
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            panic!("owl watch did not exit within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn watch_returns_on_arrival() {
    let home = tempfile::tempdir().unwrap();
    let spool = Spool::new(home.path()).unwrap();
    let id = "0191c7a0-0000-7000-8000-00000000c0de";

    // Arrival after 1 s: exit 0 with the id, well within 3 s.
    let start = Instant::now();
    let child = owl(home.path())
        .args(["watch", "--timeout", "10"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(1));
    spool.put(Dir::Inbox, id, &record(false)).unwrap();
    let (out, elapsed) = wait_bounded(child, start, Duration::from_secs(4));
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(stdout.trim(), id);
    assert!(stderr.is_empty(), "{stderr}");
    assert!(
        elapsed >= Duration::from_secs(1),
        "returned before the record existed"
    );
    assert!(elapsed < Duration::from_secs(3), "took {elapsed:?}");
    // Watching does not mark the record seen.
    assert!(!spool.get(Dir::Inbox, id).unwrap().unwrap().seen);

    // Same spool, --id naming the record: found on the first poll.
    let (out, elapsed) = wait_bounded(
        owl(home.path())
            .args(["watch", "--id", id, "--timeout", "10"])
            .spawn()
            .unwrap(),
        Instant::now(),
        Duration::from_secs(4),
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out).0.trim(), id);
    assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");

    // Same spool, --id other: keeps waiting and exits 4 at the deadline.
    let (out, elapsed) = wait_bounded(
        owl(home.path())
            .args(["watch", "--id", "other", "--timeout", "2"])
            .spawn()
            .unwrap(),
        Instant::now(),
        Duration::from_secs(5),
    );
    let (stdout, stderr) = text(&out);
    assert_eq!(
        out.status.code(),
        Some(4),
        "stdout: {stdout} stderr: {stderr}"
    );
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("timeout"), "{stderr}");
    assert!(
        elapsed >= Duration::from_secs(2),
        "returned early: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(4), "took {elapsed:?}");
}

#[test]
fn watch_ignores_seen_records_and_honours_timeout_zero() {
    let home = tempfile::tempdir().unwrap();
    let spool = Spool::new(home.path()).unwrap();
    spool.put(Dir::Inbox, "seen-one", &record(true)).unwrap();
    let (out, elapsed) = wait_bounded(
        owl(home.path())
            .args(["watch", "--timeout", "1"])
            .spawn()
            .unwrap(),
        Instant::now(),
        Duration::from_secs(4),
    );
    assert_eq!(out.status.code(), Some(4));
    assert!(elapsed >= Duration::from_secs(1));
    // --timeout 0 with an unseen record: one poll, found.
    spool.put(Dir::Inbox, "fresh", &record(false)).unwrap();
    let (out, elapsed) = wait_bounded(
        owl(home.path())
            .args(["watch", "--timeout", "0"])
            .spawn()
            .unwrap(),
        Instant::now(),
        Duration::from_secs(4),
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out).0.trim(), "fresh");
    assert!(elapsed < Duration::from_secs(1));
    // The record's question text never reaches stdout.
    assert!(!text(&out).0.contains("ZEBRA"));
}

#[test]
fn watch_without_timeout_waits_indefinitely() {
    let home = tempfile::tempdir().unwrap();
    let spool = Spool::new(home.path()).unwrap();
    let id = "0191c7a0-0000-7000-8000-00000000f00d";

    // No deadline, record after 1 s: exit 0 with the id.
    let start = Instant::now();
    let child = owl(home.path()).arg("watch").spawn().unwrap();
    std::thread::sleep(Duration::from_secs(1));
    spool.put(Dir::Inbox, id, &record(false)).unwrap();
    let (out, elapsed) = wait_bounded(child, start, Duration::from_secs(4));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    assert_eq!(text(&out).0.trim(), id);
    assert!(elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(3));

    // Negative twin: --id other, no deadline — still running after 2 s, then killed.
    let mut child = owl(home.path())
        .args(["watch", "--id", "other"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        child.try_wait().unwrap().is_none(),
        "watch without --timeout must never exit on its own"
    );
    child.kill().unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert!(text(&out).0.is_empty());
}
