//! The daemon's trash GC. A running daemon removes trash batches older than 30 days
//! — valid or junk — and leaves the young batches, the spool and everything else in the home
//! byte-identical, so `owl undo` still restores what is left.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{Peer, claude_home, id, prepare_home_with, respawn, spawn_daemon_with};
use owlpost::envelope::Envelope;
use owlpost::pull::read_status;
use owlpost::route;
use owlpost::spool::{Dir, Spool};

const OWL: &str = env!("CARGO_BIN_EXE_owl");

/// A UUIDv7 name whose embedded timestamp is `secs` (the batch-name shape `owl delete`
/// writes).
fn v7_name(secs: u64) -> String {
    uuid::Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, secs, 0)).to_string()
}

/// Every file under `root`, relative path → bytes.
fn tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push((
                    p.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(&p).unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
}

/// Every file under `home` except `trash/` and the daemon's own `daemon.status` (rewritten
/// after every pull loop): the GC must leave all of this byte-identical.
fn home_bytes(home: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![home.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                if p != home.join("trash") {
                    stack.push(p);
                }
            } else if p != home.join("daemon.status") {
                out.push((p.clone(), std::fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// With a real `owl delete` batch (young, real UUIDv7) and two hand-crafted 31-day-old
/// batches (one valid, one junk) in the trash, a running daemon removes exactly the old two;
/// the young batch and the whole spool are byte-identical afterwards, and `owl undo` still
/// restores the young batch's record byte for byte.
#[tokio::test]
async fn daemon_gcs_old_trash_and_undo_restores_the_young_batch() {
    let dir = tempfile::tempdir().unwrap();
    let (me, anna) = (id(2), id(3));
    prepare_home_with(dir.path(), &me, &[Peer::new(&anna, "Anna", None)], |_| {});
    let home = dir.path();

    // A record of Anna's, deleted through the real CLI: the young batch with a real UUIDv7.
    let spool = Spool::new(home).unwrap();
    let payload = common::question(&anna, &me, "why?");
    let env = Envelope::sign(&payload, &anna);
    spool
        .put(Dir::Inbox, &payload.id, &common::record(&env, "pending"))
        .unwrap();
    let record_bytes = std::fs::read(spool.path(Dir::Inbox, &payload.id)).unwrap();
    let out = Command::new(OWL)
        .env_remove("OWLPOST_HOME")
        .env(route::CLAUDE_HOME_ENV, claude_home())
        .arg("--home")
        .arg(home)
        .args(["delete", "Anna"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "owl delete: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let young = {
        let entries: Vec<PathBuf> = std::fs::read_dir(home.join("trash"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(entries.len(), 1, "one batch after the delete: {entries:?}");
        entries.into_iter().next().unwrap()
    };
    let young_bytes = tree(&young);

    // Two hand-crafted 31-day-old batches: one valid (batch.json + record), one junk.
    let secs_31d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 31 * 86_400;
    let old_valid = home.join("trash").join(v7_name(secs_31d));
    std::fs::create_dir_all(old_valid.join("inbox")).unwrap();
    std::fs::write(
        old_valid.join("batch.json"),
        b"{\"peer\":\"x\",\"context_id\":null}",
    )
    .unwrap();
    std::fs::write(old_valid.join("inbox").join("old.json"), b"old record").unwrap();
    let old_junk = home.join("trash").join(v7_name(secs_31d));
    std::fs::create_dir_all(&old_junk).unwrap();
    std::fs::write(old_junk.join("note.txt"), b"stale").unwrap();
    assert!(old_valid.is_dir() && old_junk.is_dir(), "precondition");
    let before = home_bytes(home);

    let d = respawn(dir, me).await;
    let mut tries = 0;
    while old_valid.exists() || old_junk.exists() {
        assert!(tries < 100, "the old batches were never collected");
        tokio::time::sleep(Duration::from_millis(100)).await;
        tries += 1;
    }
    assert!(young.is_dir(), "the young batch survived the GC");
    assert_eq!(
        tree(&young),
        young_bytes,
        "the young batch is byte-identical"
    );
    assert_eq!(
        home_bytes(d.home()),
        before,
        "everything outside the trash is byte-identical"
    );

    let out = Command::new(OWL)
        .env_remove("OWLPOST_HOME")
        .env(route::CLAUDE_HOME_ENV, claude_home())
        .arg("--home")
        .arg(d.home())
        .arg("undo")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "owl undo: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "restored 1 record of Anna\n"
    );
    assert_eq!(
        std::fs::read(
            d.home()
                .join("spool/inbox")
                .join(format!("{}.json", payload.id))
        )
        .unwrap(),
        record_bytes,
        "the restored record is byte for byte the deleted one"
    );
    assert!(!young.exists(), "the batch is gone after the undo");
    d.running.shutdown();
}

/// The GC runs at daemon start and then at most once a day, NOT on every loop: on a daemon
/// with a 1 s pull interval, a 31-day-old batch that lands AFTER the start GC survives every
/// later loop, byte-identical.
#[tokio::test]
async fn gc_does_not_run_on_every_loop() {
    let d = spawn_daemon_with(2, &[], |cfg| cfg.pull_interval_secs = 1).await;
    let home = d.home();
    // daemon.status is written at the end of every loop; the first one means the start GC
    // (it runs before `tick` in the same closure) already ran on the empty trash.
    let mut first = None;
    for _ in 0..100 {
        if let Ok(Some(status)) = read_status(home) {
            first = Some(status.last_pull_at);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut last = first.expect("the first loop never wrote daemon.status");
    assert!(
        !home.join("trash").exists(),
        "precondition: the start GC left no trash behind"
    );

    let secs_31d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 31 * 86_400;
    let old = home.join("trash").join(v7_name(secs_31d));
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(
        old.join("batch.json"),
        b"{\"peer\":\"x\",\"context_id\":null}",
    )
    .unwrap();
    std::fs::write(old.join("note.txt"), b"old").unwrap();
    let bytes = tree(&old);

    // Three more loops (three status rewrites with a moving last_pull_at): the daily GC has
    // not run again, so the old batch is still there, byte-identical.
    let (mut loops, mut tries) = (0, 0);
    while loops < 3 {
        assert!(tries < 200, "three more loops never happened");
        tokio::time::sleep(Duration::from_millis(100)).await;
        tries += 1;
        if let Ok(Some(status)) = read_status(home)
            && status.last_pull_at != last
        {
            last = status.last_pull_at;
            loops += 1;
        }
    }
    assert!(old.is_dir(), "the old batch survived: no GC since start");
    assert_eq!(tree(&old), bytes, "byte-identical");
    d.running.shutdown();
}
