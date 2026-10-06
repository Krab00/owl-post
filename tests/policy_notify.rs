//! After an automatic answer the daemon fires one `auto-answered <peer>
//! about <path>` notification (on top of the arrival one), never carrying the question or
//! answer text. `OWLPOST_NOTIFY_CMD` is process-global, so this binary holds exactly one test.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{PATH, PROJECT, Peer, client, id, policy, post_envelope, signed, spawn_daemon_with};
use owlpost::config::Harness;
use owlpost::contacts::Mode;
use owlpost::spool::{Dir, Spool};

const FAKE_HARNESS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/fake-harness.sh"
);

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

fn wait_for_lines(log: &Path, n: usize) -> Vec<String> {
    let start = Instant::now();
    loop {
        let body = std::fs::read_to_string(log).unwrap_or_default();
        let lines: Vec<String> = body.lines().map(str::to_string).collect();
        if lines.len() >= n {
            return lines;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "expected {n} notification lines, got {lines:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_notifies_auto_answered() {
    let scratch = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let (script, log) = logging_script(scratch.path());
    // SAFETY: the only test in this binary; nothing else reads or writes the environment.
    unsafe { std::env::set_var("OWLPOST_NOTIFY_CMD", &script) };

    let ana = id(1);
    let d = spawn_daemon_with(
        2,
        &[Peer::new(&ana, "Ana", Some(policy(Mode::Auto, None)))],
        |cfg| {
            cfg.notify = true;
            cfg.harnesses.insert(
                "fake".into(),
                Harness {
                    cmd: vec![FAKE_HARNESS.into(), "{prompt}".into()],
                    answer_path: "raw".into(),
                    enabled: true,
                    disabled_reason: None,
                    env: Default::default(),
                    model: None,
                },
            );
            cfg.responder.harness = "fake".into();
            cfg.projects.insert(
                PROJECT.into(),
                checkout.path().to_string_lossy().into_owned(),
            );
        },
    )
    .await;
    let question_text = "ZEBRA-SECRET-QUESTION where is the retry policy?";
    let env = signed(&ana, &d.id, question_text);
    let resp = post_envelope(&client(Some(&ana), &d.id), &d, &env).await;
    assert_eq!(resp.status().as_u16(), 202);

    let mut lines = wait_for_lines(&log, 2);
    lines.sort();
    assert_eq!(
        lines,
        [
            format!("owlpost|Ana asks about {PATH}"),
            format!("owlpost|auto-answered Ana about {PATH}"),
        ]
    );
    let body = std::fs::read_to_string(&log).unwrap();
    assert!(!body.contains("ZEBRA"), "question text leaked: {body}");
    assert!(!body.contains("sk-test"), "answer text leaked: {body}");
    // Exactly one auto-answered line even after the answer has settled.
    let home = d.home().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        while Spool::new(&home)
            .unwrap()
            .list(Dir::Outbox, |_| true)
            .unwrap()
            .is_empty()
        {
            assert!(start.elapsed() < Duration::from_secs(5), "no outbox record");
            std::thread::sleep(Duration::from_millis(25));
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let body = std::fs::read_to_string(&log).unwrap();
    assert_eq!(body.matches("auto-answered").count(), 1, "{body}");
    d.running.shutdown();
}
