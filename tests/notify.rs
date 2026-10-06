//! The daemon fires one notification per question arrival and per answer ingestion,
//! naming the peer and the path but never the question text.
//!
//! `OWLPOST_NOTIFY_CMD` is process-global, so this binary holds exactly one test.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use owlpost::contacts::Mode;
use owlpost::server::{
    AnswerIngested, DaemonEvent, Declined, Spooled, on_pull_event, on_question_spooled,
};
use owlpost::spool::{Dir, Record};

use common::{Peer, client, id, policy, post_envelope, signed, spawn_daemon_with};

/// A shell script appending `"$1|$2"` to `<dir>/notify.log` per call.
fn logging_script(dir: &Path) -> (PathBuf, PathBuf) {
    let script = dir.join("notify.sh");
    let log = dir.join("notify.log");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s|%s\\n' \"$1\" \"$2\" >> '{}'\n",
            log.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (script, log)
}

/// Bounded poll (≤ 3 s): the notifier is spawned asynchronously.
fn wait_for_lines(log: &Path, n: usize) -> Vec<String> {
    let start = Instant::now();
    loop {
        let body = std::fs::read_to_string(log).unwrap_or_default();
        let lines: Vec<String> = body.lines().map(str::to_string).collect();
        if lines.len() >= n {
            return lines;
        }
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "expected {n} notification lines, got {lines:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_notifies_on_question_and_answer() {
    let scratch = tempfile::tempdir().unwrap();
    let (script, log) = logging_script(scratch.path());
    // SAFETY: the only test in this binary; nothing else reads or writes the environment.
    unsafe { std::env::set_var("OWLPOST_NOTIFY_CMD", &script) };

    let bea = id(1);
    let martin = id(2);
    let nameless = id(4);
    let d = spawn_daemon_with(
        1,
        &[
            Peer::new(&martin, "Martin", Some(policy(Mode::Manual, None))),
            Peer::new(&nameless, "", Some(policy(Mode::Manual, None))),
        ],
        |cfg| cfg.notify = true,
    )
    .await;
    assert!(d.running.state.config.notify, "this test opts in");

    let question_text = "ZEBRA-SECRET-QUESTION why does session expiry drift?";
    let env = signed(&martin, &bea, question_text);
    let resp = post_envelope(&client(Some(&martin), &bea), &d, &env).await;
    assert_eq!(resp.status().as_u16(), 202);

    let lines = wait_for_lines(&log, 1);
    assert_eq!(
        lines.len(),
        1,
        "exactly one line for the question: {lines:?}"
    );
    assert_eq!(
        lines[0],
        format!("owlpost|Martin asks about {}", common::PATH)
    );
    assert!(
        !lines[0].contains("ZEBRA"),
        "question body leaked: {}",
        lines[0]
    );

    // Answer side: the pull loop calls the public hook on the daemon's state.
    on_pull_event(
        &d.running.state,
        DaemonEvent::Answer(AnswerIngested {
            id: "0191c7a0-0000-7000-8000-0000000000aa".into(),
            peer: common::fp(&martin),
            path: "src/other/file.rs".into(),
        }),
    );
    let lines = wait_for_lines(&log, 2);
    assert_eq!(lines.len(), 2, "one line per event: {lines:?}");
    assert_eq!(lines[1], "owlpost|answer from Martin");
    for l in &lines {
        assert!(!l.contains(question_text), "{l}");
        assert!(!l.contains("ZEBRA"), "{l}");
    }

    // An unknown fingerprint falls back to the fingerprint itself as the name.
    on_pull_event(
        &d.running.state,
        DaemonEvent::Answer(AnswerIngested {
            id: "0191c7a0-0000-7000-8000-0000000000bb".into(),
            peer: "owl:unknownpeer0000".into(),
            path: "x".into(),
        }),
    );
    let lines = wait_for_lines(&log, 3);
    assert_eq!(lines[2], "owlpost|answer from owl:unknownpeer0000");

    // A contact whose name is empty is announced by fingerprint, never as "" (§11).
    let env = signed(&nameless, &bea, question_text);
    let resp = post_envelope(&client(Some(&nameless), &bea), &d, &env).await;
    assert_eq!(resp.status().as_u16(), 202);
    let lines = wait_for_lines(&log, 4);
    assert_eq!(
        lines[3],
        format!(
            "owlpost|{} asks about {}",
            common::fp(&nameless),
            common::PATH
        )
    );

    // Question events whose inbox record is missing or unreadable still notify, with `?`
    // standing in for the path (the peer name is unaffected).
    on_question_spooled(
        &d.running.state,
        Spooled {
            id: "0191c7a0-0000-7000-8000-00000000dead".into(),
            peer: common::fp(&martin),
            state: "pending".into(),
            auto: false,
        },
    );
    let lines = wait_for_lines(&log, 5);
    assert_eq!(lines[4], "owlpost|Martin asks about ?");
    d.spool()
        .put(
            Dir::Inbox,
            "0191c7a0-0000-7000-8000-00000000beef",
            &Record {
                raw: "not json".into(),
                sig: String::new(),
                state: "pending".into(),
                seen: false,
                received_at: "2026-09-01T10:00:00Z".into(),
                draft: None,
                meta: serde_json::json!({}),
            },
        )
        .unwrap();
    on_question_spooled(
        &d.running.state,
        Spooled {
            id: "0191c7a0-0000-7000-8000-00000000beef".into(),
            peer: common::fp(&martin),
            state: "pending".into(),
            auto: false,
        },
    );
    let lines = wait_for_lines(&log, 6);
    assert_eq!(lines[5], "owlpost|Martin asks about ?");

    // A repo-level question (no path) is announced with `-` in the path slot.
    let no_path = owlpost::envelope::Payload::question(
        &common::fp(&martin),
        &common::fp(&bea),
        common::PROJECT,
        None,
        question_text,
    );
    let env = owlpost::envelope::Envelope::sign(&no_path, &martin);
    let resp = post_envelope(&client(Some(&martin), &bea), &d, &env).await;
    assert_eq!(resp.status().as_u16(), 202);
    let lines = wait_for_lines(&log, 7);
    assert_eq!(lines[6], "owlpost|Martin asks about -");

    // A declined ask (the pull loop found the peer's Task `REJECTED`) is
    // announced as `declined by <name>`, with no path and no question text.
    on_pull_event(
        &d.running.state,
        DaemonEvent::Declined(Declined {
            id: "0191c7a0-0000-7000-8000-0000000000dd".into(),
            peer: common::fp(&martin),
        }),
    );
    let lines = wait_for_lines(&log, 8);
    assert_eq!(lines[7], "owlpost|declined by Martin");

    // No further lines appear on their own.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(wait_for_lines(&log, 8).len(), 8);
    d.running.shutdown();

    // Same script, same peer, `notify = false`: the daemon spawns nothing for either event.
    let quiet = spawn_daemon_with(
        3,
        &[Peer::new(
            &martin,
            "Martin",
            Some(policy(Mode::Manual, None)),
        )],
        |cfg| cfg.notify = false,
    )
    .await;
    let env = signed(&martin, &id(3), question_text);
    let resp = post_envelope(&client(Some(&martin), &id(3)), &quiet, &env).await;
    assert_eq!(resp.status().as_u16(), 202);
    on_pull_event(
        &quiet.running.state,
        DaemonEvent::Answer(AnswerIngested {
            id: "0191c7a0-0000-7000-8000-0000000000cc".into(),
            peer: common::fp(&martin),
            path: "x".into(),
        }),
    );
    on_pull_event(
        &quiet.running.state,
        DaemonEvent::Declined(Declined {
            id: "0191c7a0-0000-7000-8000-0000000000ee".into(),
            peer: common::fp(&martin),
        }),
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        wait_for_lines(&log, 8).len(),
        8,
        "notify=false must stay silent"
    );
    quiet.running.shutdown();
}
