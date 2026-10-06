//! `owl watch [--id <id>] [--timeout <secs>]` (§9): poll the inbox every 500 ms until an
//! unseen record (matching `--id` when given) exists; print its id and exit 0. Exit 4 with
//! `timeout` on stderr once the deadline passes. Never marks anything seen. Without
//! `--timeout` it waits forever; `--timeout 0` checks exactly once.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::cli::ExitError;
use owlpost::spool::{Dir, Spool};

pub const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Exit code for "nothing arrived in time" (§9: nothing to do).
pub const TIMEOUT_EXIT: u8 = 4;

/// One scan of `inbox/`: the smallest unseen id (or the requested one, if unseen).
pub fn poll_once(spool: &Spool, id: Option<&str>) -> anyhow::Result<Option<String>> {
    let unseen = spool.list(Dir::Inbox, |r| !r.seen)?;
    Ok(unseen
        .into_iter()
        .map(|(i, _)| i)
        .find(|i| id.is_none_or(|want| want == i)))
}

/// Polls until a match or the deadline; `Err(ExitError{code: 4})` on timeout.
pub fn wait(
    spool: &Spool,
    id: Option<&str>,
    timeout: Option<Duration>,
    poll: Duration,
) -> anyhow::Result<String> {
    let deadline = timeout.map(|t| Instant::now() + t);
    loop {
        if let Some(found) = poll_once(spool, id)? {
            return Ok(found);
        }
        match deadline {
            Some(d) if Instant::now() >= d => {
                return Err(ExitError::new(TIMEOUT_EXIT, "timeout").into());
            }
            Some(d) => std::thread::sleep(poll.min(d.saturating_duration_since(Instant::now()))),
            None => std::thread::sleep(poll),
        }
    }
}

/// `owl watch`: prints the matching id on stdout.
pub fn run(home: &Path, id: Option<&str>, timeout_secs: Option<u64>) -> anyhow::Result<()> {
    let spool = Spool::new(home)?;
    let found = wait(
        &spool,
        id,
        timeout_secs.map(Duration::from_secs),
        POLL_INTERVAL,
    )?;
    println!("{found}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use owlpost::spool::Record;

    fn record(seen: bool) -> Record {
        Record {
            raw: "{}".into(),
            sig: String::new(),
            state: "pending".into(),
            seen,
            received_at: "2026-09-01T10:00:00Z".into(),
            draft: None,
            meta: serde_json::json!({}),
        }
    }

    #[test]
    fn poll_once_matches_unseen_and_optional_id() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        assert_eq!(poll_once(&spool, None).unwrap(), None);
        spool.put(Dir::Inbox, "b-seen", &record(true)).unwrap();
        assert_eq!(
            poll_once(&spool, None).unwrap(),
            None,
            "seen records never match"
        );
        spool.put(Dir::Inbox, "c-new", &record(false)).unwrap();
        spool.put(Dir::Inbox, "a-new", &record(false)).unwrap();
        assert_eq!(poll_once(&spool, None).unwrap().as_deref(), Some("a-new"));
        assert_eq!(
            poll_once(&spool, Some("c-new")).unwrap().as_deref(),
            Some("c-new")
        );
        assert_eq!(poll_once(&spool, Some("b-seen")).unwrap(), None);
        assert_eq!(poll_once(&spool, Some("other")).unwrap(), None);
        // Other directories are ignored.
        spool.put(Dir::Outbox, "d-out", &record(false)).unwrap();
        assert_eq!(poll_once(&spool, Some("d-out")).unwrap(), None);
        // Watching never marks seen.
        assert!(!spool.get(Dir::Inbox, "a-new").unwrap().unwrap().seen);
    }

    #[test]
    fn wait_times_out_with_exit_4() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let start = Instant::now();
        let err = wait(
            &spool,
            None,
            Some(Duration::from_millis(120)),
            Duration::from_millis(30),
        )
        .unwrap_err();
        let exit = err.downcast_ref::<ExitError>().expect("ExitError");
        assert_eq!(exit.code, 4);
        assert_eq!(exit.message, "timeout");
        assert!(start.elapsed() >= Duration::from_millis(120));
        assert!(start.elapsed() < Duration::from_secs(2));
        // Zero timeout: exactly one poll, still 4.
        let err = wait(&spool, None, Some(Duration::ZERO), Duration::from_secs(5)).unwrap_err();
        assert_eq!(err.downcast_ref::<ExitError>().unwrap().code, 4);
        // Zero timeout with a record present: found on the single poll.
        spool.put(Dir::Inbox, "x", &record(false)).unwrap();
        assert_eq!(
            wait(&spool, None, Some(Duration::ZERO), Duration::from_secs(5)).unwrap(),
            "x"
        );
    }

    #[test]
    fn wait_returns_when_a_record_arrives() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let writer = Spool::new(home.path()).unwrap();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            writer.put(Dir::Inbox, "late", &record(false)).unwrap();
        });
        let got = wait(
            &spool,
            Some("late"),
            Some(Duration::from_secs(5)),
            Duration::from_millis(20),
        )
        .unwrap();
        assert_eq!(got, "late");
        t.join().unwrap();
        // A corrupt record surfaces as a plain error (exit 1), not a timeout.
        std::fs::write(spool.path(Dir::Inbox, "bad"), "{nope").unwrap();
        let err = wait(&spool, Some("zzz"), Some(Duration::ZERO), Duration::ZERO).unwrap_err();
        assert!(err.downcast_ref::<ExitError>().is_none(), "{err:#}");
    }
}
