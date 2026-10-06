//! `owl inbox --count --follow --session <id>` (the poll-loop
//! fallback: marker with its pid, counter only on change, nothing at zero, marker removed on
//! a clean end) and the hook path `owl inbox --count --format claude`: `SessionStart` carries
//! this session's `watchPaths` and sweeps dead markers, `FileChanged` wakes (exit 2, the wake
//! file on stderr) on `add` of a file in this session's wake dir only, and no event
//! asks the model to arm anything any more.
//! Every test drives the built binary against its own temp home with a fast poll interval
//! (`OWLPOST_FOLLOW_SECS`).

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Output, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use common::{Peer, id, policy, prepare_home, signed};
use owlpost::contacts::Mode;
use owlpost::envelope::{self, Body, Payload};
use owlpost::identity::Identity;
use owlpost::spool::{Dir, Record, Spool};
use serde_json::Value;
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");
/// The three events that inject context; `FileChanged` is the wake and prints no context.
const EVENTS: [&str; 3] = ["SessionStart", "UserPromptSubmit", "PostToolUse"];
/// The retired arm sentence prefix: must never appear in any output again.
const ARM_PREFIX: &str = "owlpost: before handling this prompt";
const STDIN_S: &str =
    r#"{"session_id":"S","transcript_path":"/t","cwd":"/c","hook_event_name":"UserPromptSubmit"}"#;
const COUNTER_ONE: &str =
    "🦉 owlpost: 1 new question (Martin 1). Say \"show owlpost inbox\" or run `owl inbox`.";

struct Home {
    dir: TempDir,
    me: Identity,
    martin: Identity,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let (me, martin) = (id(2), id(1));
        prepare_home(
            dir.path(),
            &me,
            false,
            &[Peer::new(
                &martin,
                "Martin",
                Some(policy(Mode::Manual, None)),
            )],
        );
        Home { dir, me, martin }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// One unseen pending question from Martin.
    fn put(&self, text: &str) {
        let env = signed(&self.martin, &self.me, text);
        let p: Payload = serde_json::from_str(&env.raw).unwrap();
        let Body::Question {
            project,
            path,
            question,
            ..
        } = &p.body
        else {
            unreachable!()
        };
        let rec = Record {
            raw: env.raw.clone(),
            sig: env.sig.clone(),
            state: "pending".into(),
            seen: false,
            received_at: envelope::rfc3339_now(),
            draft: None,
            meta: serde_json::json!({ "peer": p.from, "hash": envelope::question_hash(project, path.as_deref(), question) }),
        };
        Spool::new(self.path())
            .unwrap()
            .put(Dir::Inbox, &p.id, &rec)
            .unwrap();
    }

    fn mark_all_seen(&self) {
        let spool = Spool::new(self.path()).unwrap();
        for (id, _) in spool.list(Dir::Inbox, |r| !r.seen).unwrap() {
            spool.mark_seen(Dir::Inbox, &id).unwrap();
        }
    }

    fn unseen(&self) -> usize {
        Spool::new(self.path())
            .unwrap()
            .list(Dir::Inbox, |r| !r.seen)
            .unwrap()
            .len()
    }

    fn marker(&self, id: &str) -> PathBuf {
        self.path().join("watch").join(id)
    }

    /// The `watchPaths` entry `owl` must emit: the canonical
    /// `<home>/sessions/<sid>/wake`, which exists once `SessionStart` ran for `sid`.
    fn wake_path(&self, sid: &str) -> String {
        self.path()
            .join("sessions")
            .join(sid)
            .join("wake")
            .canonicalize()
            .unwrap_or_else(|e| panic!("sessions/{sid}/wake must exist: {e}"))
            .to_string_lossy()
            .into_owned()
    }

    fn owl(&self) -> Command {
        let mut c = Command::new(OWL);
        c.env("OWLPOST_HOME", self.path())
            .env("OWLPOST_FOLLOW_SECS", "0.05");
        c
    }

    /// `owl inbox --count --format claude --hook-event <event>` with `stdin` piped in
    /// (`None`: stdin closed, the way a harness without hook input runs it).
    fn hook(&self, event: &str, stdin: Option<&str>) -> Output {
        let mut cmd = self.owl();
        cmd.args([
            "inbox",
            "--count",
            "--format",
            "claude",
            "--hook-event",
            event,
        ])
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        child.wait_with_output().unwrap()
    }
}

/// Every wait on a `--follow` child is bounded by this: a broken exit path must fail the
/// test, never hang it.
const BOUND: Duration = Duration::from_secs(10);

/// Polls until `cond` holds; after [`BOUND`] the child is killed and the test fails.
fn wait_for(child: &mut Child, what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        if start.elapsed() > BOUND {
            let _ = child.kill();
            let _ = child.wait();
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The child's stdout as a channel of lines (a reader thread), so reads can be bounded.
fn lines_of(stdout: ChildStdout) -> Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// The next line within [`BOUND`]; otherwise the child is killed and the test fails.
fn next_line(rx: &Receiver<String>, child: &mut Child, what: &str) -> String {
    match rx.recv_timeout(BOUND) {
        Ok(line) => line,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("no line within {BOUND:?} waiting for {what}: {e:?}");
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
        || Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .is_ok_and(|o| o.status.success())
}

/// A follow child for `session`, with the marker already written.
fn spawn_follow(h: &Home, session: Option<&str>) -> Child {
    let mut cmd = h.owl();
    cmd.args(["inbox", "--count", "--follow"]);
    if let Some(id) = session {
        cmd.args(["--session", id]);
    }
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(id) = session {
        let marker = h.marker(id);
        wait_for(&mut child, "the marker", || marker.exists());
        let pid: u32 = std::fs::read_to_string(&marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(pid, child.id(), "marker holds the follow's own pid");
        assert!(pid_alive(pid));
    }
    child
}

/// Waits for the child to end on its own within [`BOUND`]; a child still running then is
/// killed and the test fails (a follow whose exit path is broken must not hang the suite).
fn wait_exit(mut child: Child, what: &str) -> Output {
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > BOUND {
            let _ = child.kill();
            let _ = child.wait();
            panic!("follow still running after {BOUND:?}: {what}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

/// The marker holds the pid; the counter goes out once per change, never repeated while the
/// count stands still, never at zero; nothing is marked seen; removing the marker ends the
/// loop cleanly.
#[test]
fn follow_prints_on_change_only_and_ends_when_its_marker_is_removed() {
    let h = Home::new();
    let mut child = spawn_follow(&h, Some("S1"));
    let rx = lines_of(child.stdout.take().unwrap());
    // 0 → 2: one counter line (the first thing ever printed: zero was silent).
    h.put("why does session 0 retry?");
    h.put("why does session 1 retry?");
    assert_eq!(
        next_line(&rx, &mut child, "the 2-count"),
        "🦉 owlpost: 2 new questions (Martin 2). Say \"show owlpost inbox\" or run `owl inbox`."
    );
    // Many polls at the same count: nothing; 2 → 3: exactly the next line.
    std::thread::sleep(Duration::from_millis(400));
    h.put("and a third?");
    assert_eq!(
        next_line(&rx, &mut child, "the 3-count"),
        "🦉 owlpost: 3 new questions (Martin 3). Say \"show owlpost inbox\" or run `owl inbox`."
    );
    // 3 → 0 is silent; 0 → 1 prints again, so the line after the 3-count is the 1-count.
    h.mark_all_seen();
    std::thread::sleep(Duration::from_millis(400));
    h.put("one more?");
    assert_eq!(
        next_line(&rx, &mut child, "the 1-count"),
        "🦉 owlpost: 1 new question (Martin 1). Say \"show owlpost inbox\" or run `owl inbox`."
    );
    assert_eq!(h.unseen(), 1, "the follow never marks anything seen");
    // Removing the marker ends the loop (bounded wait): exit 0, nothing more on stdout,
    // marker still absent.
    std::fs::remove_file(h.marker("S1")).unwrap();
    let out = wait_exit(child, "after the marker was removed");
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)),
        Err(RecvTimeoutError::Disconnected),
        "no more output after the marker is gone"
    );
    assert_eq!(out.status.code(), Some(0), "{:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    assert!(!h.marker("S1").exists());
}

/// A closed stdout (the Monitor's reader went away) ends the loop and removes the marker.
#[test]
fn follow_removes_its_marker_when_stdout_closes() {
    let h = Home::new();
    let mut child = spawn_follow(&h, Some("S2"));
    drop(child.stdout.take());
    h.put("anyone there?");
    let out = wait_exit(child, "follow to end on EPIPE");
    assert_eq!(out.status.code(), Some(0), "{:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    assert!(!h.marker("S2").exists(), "marker removed on the clean end");
    assert_eq!(h.unseen(), 1);
    // `--hook-event SessionEnd` needs `--count --format claude` the same way.
    for args in [
        vec![
            "inbox",
            "--count",
            "--format",
            "plain",
            "--hook-event",
            "SessionEnd",
        ],
        vec!["inbox", "--count", "--hook-event", "SessionEnd"],
        vec!["inbox", "--format", "claude", "--hook-event", "SessionEnd"],
        vec!["inbox", "--hook-event", "SessionEnd"],
    ] {
        let out = output_within(h.owl().args(&args).stdin(Stdio::null()));
        assert_eq!(out.status.code(), Some(2), "{args:?}: {:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("--hook-event SessionEnd requires --count --format claude"),
            "{args:?}"
        );
    }
    assert_eq!(h.unseen(), 1);
}

/// Without `--session` the same loop runs with no marker at all.
#[test]
fn follow_without_session_writes_no_marker() {
    let h = Home::new();
    let mut child = spawn_follow(&h, None);
    h.put("hello?");
    let rx = lines_of(child.stdout.take().unwrap());
    assert!(next_line(&rx, &mut child, "the 1-count").starts_with("🦉 owlpost: 1 new question"));
    assert!(!h.path().join("watch").exists(), "no watch dir, no marker");
    child.kill().unwrap();
    child.wait().unwrap();
}

/// A killed follow leaves its marker behind (stale, pid dead); the `SessionStart` sweep
/// removes it — `UserPromptSubmit` does not look at markers at all.
#[test]
fn a_killed_follow_leaves_a_stale_marker_the_session_start_sweep_removes() {
    let h = Home::new();
    let mut child = spawn_follow(&h, Some("S3"));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(h.marker("S3").exists(), "kill leaves the marker");
    let out = h.hook(
        "UserPromptSubmit",
        Some(r#"{"session_id":"S3","hook_event_name":"UserPromptSubmit"}"#),
    );
    assert_eq!(hook_output(&out), None, "nothing to inject at 0 unseen");
    assert!(
        h.marker("S3").exists(),
        "UserPromptSubmit never touches markers"
    );
    // Another session's start sweeps it: the sweep is by pid, not by session id.
    let out = h.hook(
        "SessionStart",
        Some(r#"{"session_id":"other","hook_event_name":"SessionStart"}"#),
    );
    let v = hook_output(&out).expect("the SessionStart line");
    assert_eq!(watch_paths(&v), Some(vec![h.wake_path("other")]));
    assert!(
        !h.marker("S3").exists(),
        "stale marker removed by the SessionStart sweep"
    );
}

/// The follow interval comes from `OWLPOST_FOLLOW_SECS`: with the default 5 s no line
/// appears within a second of a change, with 0.05 it does.
#[test]
fn follow_interval_is_read_from_the_env() {
    let h = Home::new();
    let mut child = Command::new(OWL)
        .env("OWLPOST_HOME", h.path())
        .env_remove("OWLPOST_FOLLOW_SECS")
        .args(["inbox", "--count", "--follow", "--session", "slow"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let marker = h.marker("slow");
    wait_for(&mut child, "the marker", || marker.exists());
    // The first poll ran at start (count 0, silent); the next one is 5 s away.
    std::thread::sleep(Duration::from_millis(100));
    h.put("slow?");
    std::thread::sleep(Duration::from_millis(900));
    child.kill().unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "",
        "5 s interval: no poll within a second"
    );
}

/// `cmd`'s output, with the run bounded by [`BOUND`]: a guard that has gone missing would
/// start a real follow loop, so the child is killed and the test fails instead of hanging.
fn output_within(cmd: &mut Command) -> Output {
    let child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_exit(child, "a rejected invocation")
}

/// Ids outside `[A-Za-z0-9._-]{1,128}` are a clap usage error (exit 2) before anything is
/// written; so are `--session` without `--follow`, `--follow` without `--count`, and
/// `--follow` with `--format`.
#[test]
fn follow_rejects_bad_session_ids_and_flag_combinations_with_exit_2() {
    let h = Home::new();
    let long = "x".repeat(129);
    for bad in ["", "a b", "../x", "a/b", "ü", "$(x)", long.as_str()] {
        let out = output_within(
            h.owl()
                .args(["inbox", "--count", "--follow", "--session", bad]),
        );
        assert_eq!(out.status.code(), Some(2), "{bad:?}: {:?}", out.status);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("session id must match [A-Za-z0-9._-]{1,128}"),
            "{bad:?}: {err}"
        );
        assert!(err.contains("for '--session <ID>'"), "{bad:?}: {err}");
        assert!(!h.path().join("watch").exists(), "{bad:?}: nothing written");
    }
    for args in [
        vec!["inbox", "--count", "--session", "S"],
        vec!["inbox", "--follow", "--session", "S"],
        vec![
            "inbox",
            "--count",
            "--follow",
            "--format",
            "plain",
            "--session",
            "S",
        ],
    ] {
        let out = output_within(h.owl().args(&args));
        assert_eq!(out.status.code(), Some(2), "{args:?}: {:?}", out.status);
        assert!(!h.path().join("watch").exists(), "{args:?}");
    }
    // The 128-char boundary is accepted (the follow starts and is killed at once).
    let ok = "y".repeat(128);
    let mut child = spawn_follow(&h, Some(&ok));
    child.kill().unwrap();
    child.wait().unwrap();
}

// ---------- The hook path ----------

/// `hookSpecificOutput` of a one-line `--format claude` output, `None` for empty output; the
/// line must be valid JSON, exit 0, nothing on stderr.
fn hook_output(out: &Output) -> Option<Value> {
    assert_eq!(out.status.code(), Some(0), "{:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "", "stderr");
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();
    if stdout.is_empty() {
        return None;
    }
    assert_eq!(stdout.lines().count(), 1, "one line: {stdout:?}");
    assert!(!stdout.contains(ARM_PREFIX), "{stdout}");
    let v: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    let inner = v["hookSpecificOutput"].clone();
    assert!(inner.is_object(), "{stdout}");
    Some(inner)
}

/// `additionalContext` of the line, `None` when the line or the key is absent.
fn context(out: &Output) -> Option<String> {
    hook_output(out)?
        .get("additionalContext")?
        .as_str()
        .map(str::to_string)
}

/// `watchPaths` of a `hookSpecificOutput`, `None` when the key is absent.
fn watch_paths(v: &Value) -> Option<Vec<String>> {
    Some(
        v.get("watchPaths")?
            .as_array()
            .expect("watchPaths is an array")
            .iter()
            .map(|p| p.as_str().unwrap().to_string())
            .collect(),
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Marker {
    None,
    Live,
    Dead,
}

/// The full grid at 0 unseen: event × marker state × `plugin.json` × stdin. The only output
/// is the `SessionStart` line with `watchPaths` (the canonical `<home>/sessions/S/wake`)
/// and no `additionalContext`, with a usable `session_id` on stdin and the watch not
/// off; `UserPromptSubmit` and `PostToolUse` print nothing. A dead marker is removed exactly
/// on `SessionStart` (any stdin, watch on or off), a live one always stays; no cell ever
/// carries the retired arm sentence.
#[test]
fn hook_emits_watch_paths_and_sweeps_markers_per_event_marker_plugin_json_and_stdin() {
    let h = Home::new();
    let mut sleeper = Command::new("sleep")
        .arg("60")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut reaped = Command::new("true").spawn().unwrap();
    reaped.wait().unwrap();
    std::fs::create_dir_all(h.path().join("watch")).unwrap();
    let plugin_json = h.path().join("plugin.json");
    let mut cells = 0;
    for event in EVENTS {
        for marker in [Marker::None, Marker::Live, Marker::Dead] {
            for watch in [None, Some(true), Some(false)] {
                for stdin in [Some(STDIN_S), Some("not json"), None] {
                    match marker {
                        Marker::None => {
                            let _ = std::fs::remove_file(h.marker("S"));
                        }
                        Marker::Live => {
                            std::fs::write(h.marker("S"), sleeper.id().to_string()).unwrap()
                        }
                        Marker::Dead => {
                            std::fs::write(h.marker("S"), reaped.id().to_string()).unwrap()
                        }
                    }
                    match watch {
                        None => {
                            let _ = std::fs::remove_file(&plugin_json);
                        }
                        Some(b) => {
                            std::fs::write(&plugin_json, format!(r#"{{"watch": {b}}}"#)).unwrap()
                        }
                    }
                    let cell = format!("{event} marker={marker:?} watch={watch:?} stdin={stdin:?}");
                    let out = h.hook(event, stdin);
                    let v = hook_output(&out);
                    let want =
                        event == "SessionStart" && watch != Some(false) && stdin == Some(STDIN_S);
                    match (&v, want) {
                        (Some(v), true) => {
                            assert_eq!(v["hookEventName"], "SessionStart", "{cell}");
                            assert_eq!(watch_paths(v), Some(vec![h.wake_path("S")]), "{cell}");
                            assert!(v.get("additionalContext").is_none(), "{cell}: {v}");
                            assert_eq!(v.as_object().unwrap().len(), 2, "{cell}: {v}");
                        }
                        (None, false) => {}
                        other => panic!("{cell}: got {other:?}"),
                    }
                    // Marker bookkeeping: a live marker always stays; a dead one goes exactly
                    // on SessionStart, whatever stdin or plugin.json say.
                    let swept = event == "SessionStart";
                    match marker {
                        Marker::Live => assert!(h.marker("S").exists(), "{cell}: live marker kept"),
                        Marker::Dead => {
                            assert_eq!(!h.marker("S").exists(), swept, "{cell}: stale marker")
                        }
                        Marker::None => assert!(!h.marker("S").exists(), "{cell}"),
                    }
                    cells += 1;
                }
            }
        }
    }
    assert_eq!(cells, 3 * 3 * 3 * 3);
    sleeper.kill().unwrap();
    sleeper.wait().unwrap();
}

/// The sweep covers every marker in `watch/` — dead ones of any session id go,
/// a live one (a spawned `sleep`) stays — and an absent or unreadable `watch/` does not fail
/// the hook.
#[test]
fn session_start_sweeps_every_dead_marker_and_keeps_live_ones() {
    let h = Home::new();
    let mut sleeper = Command::new("sleep").arg("60").spawn().unwrap();
    let mut reaped = Command::new("true").spawn().unwrap();
    reaped.wait().unwrap();
    // Absent watch dir: the line still goes out.
    assert!(!h.path().join("watch").exists());
    let v = hook_output(&h.hook("SessionStart", Some(STDIN_S))).unwrap();
    assert_eq!(watch_paths(&v), Some(vec![h.wake_path("S")]));
    assert!(
        !h.path().join("watch").exists(),
        "the sweep creates nothing"
    );
    // Three dead markers with unrelated ids, one live, one garbage.
    std::fs::create_dir_all(h.path().join("watch")).unwrap();
    for dead in ["S", "abc-123", "other.session"] {
        std::fs::write(h.marker(dead), format!("{}\n", reaped.id())).unwrap();
    }
    std::fs::write(h.marker("garbage"), "not a pid").unwrap();
    std::fs::write(h.marker("live"), sleeper.id().to_string()).unwrap();
    // UserPromptSubmit and PostToolUse leave everything alone.
    for event in ["UserPromptSubmit", "PostToolUse"] {
        assert_eq!(hook_output(&h.hook(event, Some(STDIN_S))), None);
        for m in ["S", "abc-123", "other.session", "garbage", "live"] {
            assert!(h.marker(m).exists(), "{event} must not touch {m}");
        }
    }
    hook_output(&h.hook("SessionStart", Some(STDIN_S))).unwrap();
    for gone in ["S", "abc-123", "other.session", "garbage"] {
        assert!(!h.marker(gone).exists(), "{gone} swept");
    }
    assert!(h.marker("live").exists(), "live marker kept");
    assert!(pid_alive(sleeper.id()));
    // Watch off: the sweep still runs (no line at 0 unseen).
    std::fs::write(h.marker("S"), reaped.id().to_string()).unwrap();
    std::fs::write(h.path().join("plugin.json"), r#"{"watch": false}"#).unwrap();
    assert_eq!(hook_output(&h.hook("SessionStart", Some(STDIN_S))), None);
    assert!(!h.marker("S").exists());
    assert!(h.marker("live").exists());
    std::fs::remove_file(h.path().join("plugin.json")).unwrap();
    // `watch` is a file, not a directory: unreadable as a dir, the hook still succeeds.
    sleeper.kill().unwrap();
    sleeper.wait().unwrap();
    std::fs::remove_dir_all(h.path().join("watch")).unwrap();
    std::fs::write(h.path().join("watch"), "x").unwrap();
    let v = hook_output(&h.hook("SessionStart", Some(STDIN_S))).unwrap();
    assert_eq!(watch_paths(&v), Some(vec![h.wake_path("S")]));
    assert!(h.path().join("watch").is_file());
}

/// With unseen records the counter is the whole context (no arm sentence, no space); on
/// `SessionStart` the previews and the open sentence close it and `watchPaths` rides along.
/// Nothing is marked seen; the event name is echoed on every line.
#[test]
fn hook_keeps_the_counter_and_previews_and_never_asks_to_arm() {
    let h = Home::new();
    h.put("why does session 0 retry?");
    h.put("why does session 1 retry?");
    let counter =
        "🦉 owlpost: 2 new questions (Martin 2). Say \"show owlpost inbox\" or run `owl inbox`.";
    let previews = common::previews(h.path(), "Martin");
    let ss = hook_output(&h.hook("SessionStart", Some(STDIN_S))).unwrap();
    assert_eq!(ss["additionalContext"], format!("{counter}\n{previews}"));
    assert_eq!(watch_paths(&ss), Some(vec![h.wake_path("S")]));
    let ups = hook_output(&h.hook("UserPromptSubmit", Some(STDIN_S))).unwrap();
    assert_eq!(ups["additionalContext"], counter);
    assert_eq!(watch_paths(&ups), None, "{ups}");
    let ptu = hook_output(&h.hook("PostToolUse", Some(STDIN_S))).unwrap();
    assert_eq!(ptu["additionalContext"], counter);
    assert_eq!(watch_paths(&ptu), None, "{ptu}");
    // Watch off: same context, no watchPaths.
    std::fs::write(h.path().join("plugin.json"), r#"{"watch": false}"#).unwrap();
    let off = hook_output(&h.hook("SessionStart", Some(STDIN_S))).unwrap();
    assert_eq!(off["additionalContext"], format!("{counter}\n{previews}"));
    assert_eq!(watch_paths(&off), None, "{off}");
    std::fs::remove_file(h.path().join("plugin.json")).unwrap();
    // No stdin: no session, so the same context without `watchPaths`.
    let mut no_session = ss.clone();
    no_session.as_object_mut().unwrap().remove("watchPaths");
    assert_eq!(hook_output(&h.hook("SessionStart", None)), Some(no_session));
    assert_eq!(
        context(&h.hook("UserPromptSubmit", None)).as_deref(),
        Some(counter)
    );
    assert_eq!(h.unseen(), 2);
    for event in EVENTS {
        let v = hook_output(&h.hook(event, Some(STDIN_S))).unwrap();
        assert_eq!(v["hookEventName"], event);
    }
}

/// Stdin that carries nothing usable — empty, not JSON, no `session_id`, a bad id — names
/// no session: `SessionStart` writes no marker and carries no `watchPaths` (so it
/// prints nothing at 0 unseen), the others print nothing, no `sessions/` or marker dir is
/// created. `--session-start` (an older hooks.json) is accepted and ignored.
#[test]
fn hook_context_events_ignore_unusable_stdin_and_session_start_flag() {
    let h = Home::new();
    for stdin in [
        "",
        "not json",
        "{}",
        r#"{"session_id": 5}"#,
        r#"{"session_id": ""}"#,
        r#"{"session_id": "a/b"}"#,
        r#"{"session_id": "../x"}"#,
        r#"{"sessionId": "S"}"#,
    ] {
        for event in EVENTS {
            assert_eq!(
                hook_output(&h.hook(event, Some(stdin))),
                None,
                "{event} {stdin:?}"
            );
        }
        assert!(
            !h.path().join("sessions").exists(),
            "{stdin:?}: no session directory"
        );
    }
    assert!(!h.path().join("watch").exists());
    let out = h
        .owl()
        .args([
            "inbox",
            "--count",
            "--format",
            "claude",
            "--hook-event",
            "SessionStart",
            "--session-start",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(hook_output(&out), None);
    let out = h
        .owl()
        .args(["inbox", "--count", "--format", "claude", "--session-start"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(hook_output(&out), None);
    // A usable session id (`S`, `event: add` along with it is irrelevant here): the line.
    let v =
        hook_output(&h.hook("SessionStart", Some(r#"{"session_id":"S","event":"add"}"#))).unwrap();
    assert_eq!(watch_paths(&v), Some(vec![h.wake_path("S")]));
    assert!(h.path().join("sessions/S/marker.json").is_file());
}

/// Other formats never read stdin, never carry `watchPaths` and never carry the sentence.
#[test]
fn other_formats_ignore_stdin_and_never_carry_watch_paths() {
    let h = Home::new();
    h.put("one?");
    for f in ["plain", "codex", "kimi"] {
        for event in EVENTS {
            let mut child = h
                .owl()
                .args(["inbox", "--count", "--format", f, "--hook-event", event])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(STDIN_S.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            let s = String::from_utf8_lossy(&out.stdout);
            assert!(s.contains("1 new question"), "{f} {event}: {s}");
            assert!(!s.contains(ARM_PREFIX), "{f} {event}: {s}");
            assert!(!s.contains("--follow"), "{f} {event}: {s}");
            assert!(!s.contains("watchPaths"), "{f} {event}: {s}");
        }
        // Codex at SessionStart, 0 unseen: nothing (no watch path for a harness without the hook).
        h.mark_all_seen();
        let out = h
            .owl()
            .args([
                "inbox",
                "--count",
                "--format",
                f,
                "--hook-event",
                "SessionStart",
            ])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{f}");
        h.put("one?");
    }
    assert!(!h.path().join("watch").exists());
}

// ---------- The wake ----------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Input {
    Add,
    Change,
    Unlink,
    NoEvent,
    Empty,
    Garbage,
    Closed,
}

impl Input {
    fn stdin(self, path: &str) -> Option<String> {
        match self {
            Input::Add | Input::Change | Input::Unlink => {
                let event = format!("{self:?}").to_lowercase();
                Some(format!(
                    r#"{{"session_id":"S","transcript_path":"/t","cwd":"/c","hook_event_name":"FileChanged","file_path":"{path}","event":"{event}"}}"#
                ))
            }
            Input::NoEvent => Some(format!(
                r#"{{"session_id":"S","hook_event_name":"FileChanged","file_path":"{path}"}}"#
            )),
            Input::Empty => Some(String::new()),
            Input::Garbage => Some("not json".to_string()),
            Input::Closed => None,
        }
    }
}

/// Where the changed file lives, relative to the session `S` the hook runs for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Where {
    /// `sessions/S/wake/r.md`: this session's wake file.
    Own,
    /// `sessions/S10/wake/r.md`: another session's (a prefix look-alike of `S`).
    Other,
    /// `sessions/S/wake-evil/r.md`: a directory whose name starts with `wake`.
    Lookalike,
    /// `spool/inbox/r.json`: the old shared watch path.
    Inbox,
    /// `sessions/S/wake/missing.md`: a file that does not exist.
    Missing,
}

/// The full grid: input × location × `plugin.json` × unseen count. Exit 2 with the wake
/// file's content byte for byte on stderr and nothing on stdout exactly for `add` of this
/// session's own wake file with the watch not off, whatever the count; every other cell is
/// exit 0 and silent. The count never changes and the file stays.
#[test]
fn file_changed_prints_the_sessions_wake_file_on_add_only() {
    let h = Home::new();
    let plugin_json = h.path().join("plugin.json");
    let body =
        "Show the table below\r\n\n| 🦉 **Martin** · 10:00 · p · src/a.rs |\n|---|\n| why? |\n\n";
    hook_output(&h.hook("SessionStart", Some(STDIN_S))).unwrap();
    hook_output(&h.hook(
        "SessionStart",
        Some(r#"{"session_id":"S10","hook_event_name":"SessionStart"}"#),
    ))
    .unwrap();
    let own = h.path().join("sessions/S/wake/r.md");
    let other = h.path().join("sessions/S10/wake/r.md");
    let lookalike = h.path().join("sessions/S/wake-evil/r.md");
    std::fs::create_dir_all(lookalike.parent().unwrap()).unwrap();
    std::fs::write(&own, body).unwrap();
    std::fs::write(&other, "other").unwrap();
    std::fs::write(&lookalike, "evil").unwrap();
    let mut cells = 0;
    for count in [0usize, 1] {
        h.mark_all_seen();
        for i in 0..count {
            h.put(&format!("why does session {i} retry?"));
        }
        for input in [
            Input::Add,
            Input::Change,
            Input::Unlink,
            Input::NoEvent,
            Input::Empty,
            Input::Garbage,
            Input::Closed,
        ] {
            for at in [
                Where::Own,
                Where::Other,
                Where::Lookalike,
                Where::Inbox,
                Where::Missing,
            ] {
                let path = match at {
                    Where::Own => own.clone(),
                    Where::Other => other.clone(),
                    Where::Lookalike => lookalike.clone(),
                    Where::Inbox => h.path().join("spool/inbox/r.json"),
                    Where::Missing => h.path().join("sessions/S/wake/missing.md"),
                };
                for watch in [None, Some(true), Some(false)] {
                    match watch {
                        None => {
                            let _ = std::fs::remove_file(&plugin_json);
                        }
                        Some(b) => {
                            std::fs::write(&plugin_json, format!(r#"{{"watch": {b}}}"#)).unwrap()
                        }
                    }
                    let cell = format!("count={count} input={input:?} at={at:?} watch={watch:?}");
                    let stdin = input.stdin(&path.to_string_lossy());
                    let out = h.hook("FileChanged", stdin.as_deref());
                    let wake = input == Input::Add && at == Where::Own && watch != Some(false);
                    assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{cell}: stdout");
                    if wake {
                        assert_eq!(out.status.code(), Some(2), "{cell}: {:?}", out.status);
                        assert_eq!(out.stderr, body.as_bytes(), "{cell}: stderr is the file");
                        assert!(!String::from_utf8_lossy(&out.stderr).contains("--hook-event"));
                    } else {
                        assert_eq!(out.status.code(), Some(0), "{cell}: {:?}", out.status);
                        assert_eq!(String::from_utf8_lossy(&out.stderr), "", "{cell}: stderr");
                    }
                    assert_eq!(h.unseen(), count, "{cell}: nothing marked seen");
                    assert_eq!(
                        std::fs::read(&own).unwrap(),
                        body.as_bytes(),
                        "{cell}: file stays"
                    );
                    cells += 1;
                }
            }
        }
    }
    assert_eq!(cells, 2 * 7 * 5 * 3);
    let _ = std::fs::remove_file(&plugin_json);
    assert!(
        !h.path().join("watch").exists(),
        "the wake writes no marker"
    );
    // The hook never composes text: the old counter sentence appears nowhere.
    h.put("one?");
    let out = h.hook(
        "FileChanged",
        Input::Add.stdin(&own.to_string_lossy()).as_deref(),
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&out.stderr).contains(COUNTER_ONE));
    assert_eq!(
        COUNTER_ONE,
        "🦉 owlpost: 1 new question (Martin 1). Say \"show owlpost inbox\" or run `owl inbox`."
    );
}

/// `--hook-event FileChanged` with any format but `claude` is a clap usage error: exit 2,
/// nothing on stdout, the usage text (not a counter) on stderr, nothing marked seen.
#[test]
fn file_changed_requires_the_claude_format() {
    let h = Home::new();
    h.put("one?");
    for f in ["plain", "codex", "kimi"] {
        let out = output_within(
            h.owl()
                .args([
                    "inbox",
                    "--count",
                    "--format",
                    f,
                    "--hook-event",
                    "FileChanged",
                ])
                .stdin(Stdio::null()),
        );
        assert_eq!(out.status.code(), Some(2), "{f}: {:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{f}: stdout");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("--hook-event FileChanged requires --format claude"),
            "{f}: {err}"
        );
        assert!(err.contains("Usage:"), "{f}: {err}");
        assert!(!err.contains("🦉"), "{f}: {err}");
    }
    // Without `--format` at all (a plain count, a listing) it is the same usage error: the
    // event has no meaning outside the Claude hook.
    for args in [
        vec!["inbox", "--count", "--hook-event", "FileChanged"],
        vec!["inbox", "--hook-event", "FileChanged"],
    ] {
        let out = output_within(h.owl().args(&args).stdin(Stdio::null()));
        assert_eq!(out.status.code(), Some(2), "{args:?}: {:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout), "", "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("--hook-event FileChanged requires --format claude"),
            "{args:?}"
        );
    }
    assert_eq!(h.unseen(), 1);
}
