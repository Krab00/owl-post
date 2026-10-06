//! `owl archive` / `owl unarchive` / `owl delete` / `owl undo` and the `--archived`
//! flag of `owl thread`. Every test drives the `owl` binary against its own temp home; fixture
//! helpers are the `tests/thread.rs` ones, cut to what these tests use.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{PATH, PROJECT, Peer, claude_home, fp, id, prepare_home_with};
use owlpost::envelope::{Envelope, Payload};
use owlpost::events::EVENTS_KEY;
use owlpost::identity::Identity;
use owlpost::route;
use owlpost::spool::{Dir, Record, Spool};
use serde_json::{Value, json};
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");

// ------------------------------------------------------------------ fixture home

struct Home {
    dir: TempDir,
    me: Identity,
    anna: Identity,
    brooke: Identity,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let (me, anna, brooke) = (id(2), id(3), id(1));
        let peers = [
            Peer::new(&anna, "Anna", None),
            Peer::new(&brooke, "Brooke", None),
        ];
        prepare_home_with(dir.path(), &me, &peers, |_| {});
        Home {
            dir,
            me,
            anna,
            brooke,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn spool(&self) -> Spool {
        Spool::new(self.path()).unwrap()
    }

    fn owl(&self) -> Command {
        let mut c = Command::new(OWL);
        c.env_remove("OWLPOST_HOME")
            .env(route::CLAUDE_HOME_ENV, claude_home())
            .arg("--home")
            .arg(self.path());
        c
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.owl().args(args).output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, out, err) = self.run(args);
        assert_eq!(code, 0, "owl {args:?}: {err}");
        out
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        serde_json::from_str(&self.ok(&all)).expect("--json output is JSON")
    }
}

/// A spool record to write by hand: everything a fixture needs to control.
struct Fx<'a> {
    dir: Dir,
    from: &'a Identity,
    to: &'a Identity,
    text: &'a str,
    state: &'a str,
    at: &'a str,
    context_id: Option<&'a str>,
    /// `None` leaves the record legacy (no `meta.events`, `of` derives).
    events: Option<Value>,
}

impl<'a> Fx<'a> {
    fn q(dir: Dir, from: &'a Identity, to: &'a Identity, text: &'a str, at: &'a str) -> Fx<'a> {
        Fx {
            dir,
            from,
            to,
            text,
            state: "pending",
            at,
            context_id: None,
            events: None,
        }
    }

    fn state(mut self, s: &'a str) -> Fx<'a> {
        self.state = s;
        self
    }

    fn context(mut self, c: &'a str) -> Fx<'a> {
        self.context_id = Some(c);
        self
    }

    fn events(mut self, e: Value) -> Fx<'a> {
        self.events = Some(e);
        self
    }
}

/// Writes the fixture into the spool and returns its record id.
fn put(spool: &Spool, f: Fx<'_>) -> String {
    let mut payload = Payload::question(&fp(f.from), &fp(f.to), PROJECT, Some(PATH), f.text);
    payload.context_id = f.context_id.map(str::to_string);
    let env = Envelope::sign(&payload, f.from);
    let mut meta = json!({ "peer": fp(f.from) });
    if let Some(e) = f.events {
        meta[EVENTS_KEY] = e;
    }
    let rec = Record {
        raw: env.raw,
        sig: env.sig,
        state: f.state.into(),
        seen: false,
        received_at: f.at.into(),
        draft: None,
        meta,
    };
    spool.put(f.dir, &payload.id, &rec).unwrap();
    payload.id
}

fn ev(ts: &str, kind: &str) -> Value {
    json!({ "ts": ts, "kind": kind })
}

/// Every file under `spool/`, path → bytes: the delete/undo before/after comparison.
fn spool_bytes(home: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    for dir in Dir::ALL {
        let d = home.join("spool").join(dir.name());
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries {
            let p = e.unwrap().path();
            if p.is_file() {
                out.push((p.clone(), std::fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// The one directory inside `$OWLPOST_HOME/trash`, panicking when there is not exactly one.
fn the_batch(home: &Path) -> PathBuf {
    let batches: Vec<PathBuf> = std::fs::read_dir(home.join("trash"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(batches.len(), 1, "one trash batch: {batches:?}");
    batches.into_iter().next().unwrap()
}

// ------------------------------------------------------------------ archive a chat

/// Archiving a chat hides it from the `owl thread` list (and only `--archived` shows it), is
/// idempotent, never touches the records — a named timeline still shows in full — and
/// unarchiving brings it back, after which `--archived` finds nothing and exits 4.
#[test]
fn archive_and_unarchive_a_chat() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "anna asks",
            "2026-09-14T10:00:00Z",
        ),
    );
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.brooke,
            &h.me,
            "brooke asks",
            "2026-09-14T11:00:00Z",
        ),
    );
    let names = |args: &[&str]| -> Vec<String> {
        h.json(args)
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["from_name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names(&["thread"]), ["Brooke", "Anna"]);

    assert_eq!(h.ok(&["archive", "Anna"]), "archived Anna\n");
    assert_eq!(
        h.ok(&["archive", "Anna"]),
        "archived Anna\n",
        "archiving the archived is fine"
    );
    assert_eq!(names(&["thread"]), ["Brooke"], "the archived chat is out");
    assert_eq!(
        names(&["thread", "--archived"]),
        ["Anna"],
        "--archived lists only archived chats"
    );
    assert_eq!(
        h.json(&["thread", "Anna"]).as_array().unwrap().len(),
        1,
        "the records stay: a named timeline ignores the chat mark"
    );

    assert_eq!(h.ok(&["unarchive", "Anna"]), "unarchived Anna\n");
    assert_eq!(names(&["thread"]), ["Brooke", "Anna"]);
    let (code, out, err) = h.run(&["thread", "--archived"]);
    assert_eq!(code, 4, "{err}");
    assert_eq!(out, "");
    assert!(err.contains("no threads"), "{err}");
}

// ------------------------------------------------------------------ archive a thread

/// Archiving one thread hides its records from the peer's timeline (`--archived` shows only
/// them, an explicit `--context` always shows them) and never hides the chat from the list.
#[test]
fn archive_a_thread_filters_the_timeline() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "threaded one",
            "2026-09-14T10:00:00Z",
        )
        .context("ctx-1")
        .events(json!([ev("2026-09-14T10:00:00Z", "received")])),
    );
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "plain one",
            "2026-09-14T11:00:00Z",
        )
        .events(json!([ev("2026-09-14T11:00:00Z", "received")])),
    );
    let texts = |args: &[&str]| -> Vec<String> {
        h.json(args)
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["text"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(texts(&["thread", "Anna"]), ["threaded one", "plain one"]);

    assert_eq!(
        h.ok(&["archive", "Anna", "--context", "ctx-1"]),
        "archived thread ctx-1 with Anna\n"
    );
    assert_eq!(
        texts(&["thread", "Anna"]),
        ["plain one"],
        "the archived thread's records leave the timeline"
    );
    assert_eq!(
        texts(&["thread", "Anna", "--archived"]),
        ["threaded one"],
        "--archived shows only the archived thread"
    );
    assert_eq!(
        texts(&["thread", "Anna", "--context", "ctx-1"]),
        ["threaded one"],
        "an explicit --context ignores the archive mark"
    );
    assert_eq!(
        h.json(&["thread"]).as_array().unwrap().len(),
        1,
        "the chat itself is not archived: Anna stays on the list"
    );

    assert_eq!(
        h.ok(&["unarchive", "Anna", "--context", "ctx-1"]),
        "unarchived thread ctx-1 with Anna\n"
    );
    assert_eq!(texts(&["thread", "Anna"]), ["threaded one", "plain one"]);
}

// ------------------------------------------------------------------ delete and undo

/// Deleting a chat with records in `inbox`, `asks` and `done` moves every file into one trash
/// batch and out of the spool and `owl thread`; `owl undo` puts every byte back and removes
/// the batch.
#[test]
fn delete_a_chat_and_undo_restores_every_byte() {
    let h = Home::new();
    let s = h.spool();
    let inbox = put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "inbox question",
            "2026-09-14T10:00:00Z",
        ),
    );
    let ask = put(
        &s,
        Fx::q(Dir::Asks, &h.me, &h.anna, "our ask", "2026-09-14T09:00:00Z").state("waiting"),
    );
    let done = put(
        &s,
        Fx::q(
            Dir::Done,
            &h.anna,
            &h.me,
            "old question",
            "2026-09-14T08:00:00Z",
        )
        .state("answered"),
    );
    let brooke = put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.brooke,
            &h.me,
            "brooke stays",
            "2026-09-14T11:00:00Z",
        ),
    );
    let before = spool_bytes(h.path());

    assert_eq!(
        h.ok(&["delete", "Anna"]),
        "deleted 3 records of Anna — owl undo restores them\n"
    );
    for (dir, rid) in [(Dir::Inbox, &inbox), (Dir::Asks, &ask), (Dir::Done, &done)] {
        assert!(!s.path(dir, rid).exists(), "{rid} left {}", dir.name());
    }
    assert!(
        s.path(Dir::Inbox, &brooke).exists(),
        "another peer's record stays"
    );
    let batch = the_batch(h.path());
    assert!(
        batch.join("inbox").join(format!("{inbox}.json")).is_file(),
        "the batch keeps the directory layout"
    );
    let rows = h.json(&["thread"]);
    let names: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["from_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Brooke"], "the deleted chat leaves the list");

    assert_eq!(h.ok(&["undo"]), "restored 3 records of Anna\n");
    assert_eq!(spool_bytes(h.path()), before, "every byte is back");
    assert!(!batch.exists(), "the batch is gone after the undo");
}

/// Deleting one thread moves only its records; undo restores them byte-identical.
#[test]
fn delete_a_thread_and_undo() {
    let h = Home::new();
    let s = h.spool();
    let threaded = put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "threaded one",
            "2026-09-14T10:00:00Z",
        )
        .context("ctx-1")
        .events(json!([ev("2026-09-14T10:00:00Z", "received")])),
    );
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "plain one",
            "2026-09-14T11:00:00Z",
        )
        .events(json!([ev("2026-09-14T11:00:00Z", "received")])),
    );
    let before = spool_bytes(h.path());

    assert_eq!(
        h.ok(&["delete", "Anna", "--context", "ctx-1"]),
        "deleted 1 record of thread ctx-1 with Anna — owl undo restores them\n"
    );
    assert!(!s.path(Dir::Inbox, &threaded).exists());
    let rows = h.json(&["thread", "Anna"]);
    let texts: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["plain one"], "the rest of the chat stays");

    assert_eq!(
        h.ok(&["undo"]),
        "restored 1 record of thread ctx-1 with Anna\n"
    );
    assert_eq!(spool_bytes(h.path()), before, "every byte is back");
}

/// Two deletes make two batches; two undos walk them back newest first, and a third finds
/// nothing (exit 4).
#[test]
fn undos_walk_the_batches_back_newest_first() {
    let h = Home::new();
    let s = h.spool();
    let anna_rec = put(
        &s,
        Fx::q(Dir::Inbox, &h.anna, &h.me, "anna's", "2026-09-14T10:00:00Z"),
    );
    let brooke_rec = put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.brooke,
            &h.me,
            "brooke's",
            "2026-09-14T11:00:00Z",
        ),
    );
    h.ok(&["delete", "Anna"]);
    // The batch name is a UUIDv7 with millisecond resolution: let the clock tick so the two
    // batches sort by time and the undo order is deterministic.
    std::thread::sleep(std::time::Duration::from_millis(20));
    h.ok(&["delete", "Brooke"]);

    assert_eq!(h.ok(&["undo"]), "restored 1 record of Brooke\n");
    assert!(s.path(Dir::Inbox, &brooke_rec).exists());
    assert!(
        !s.path(Dir::Inbox, &anna_rec).exists(),
        "the older batch is still in the trash"
    );
    assert_eq!(h.ok(&["undo"]), "restored 1 record of Anna\n");
    assert!(s.path(Dir::Inbox, &anna_rec).exists());

    let (code, _, err) = h.run(&["undo"]);
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("nothing to undo"), "{err}");
}

// ---------------------------------------------------------- unarchive after delete

/// Unarchive needs no record: a chat archived whole and per-thread, then deleted, still lets
/// both marks come off — a new message of hers must not stay hidden forever.
#[test]
fn unarchive_after_delete_needs_no_records() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "threaded one",
            "2026-09-14T10:00:00Z",
        )
        .context("ctx-1"),
    );
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "plain one",
            "2026-09-14T11:00:00Z",
        ),
    );
    h.ok(&["archive", "Anna"]);
    h.ok(&["archive", "Anna", "--context", "ctx-1"]);
    h.ok(&["delete", "Anna"]);

    assert_eq!(h.ok(&["unarchive", "Anna"]), "unarchived Anna\n");
    assert_eq!(
        h.ok(&["unarchive", "Anna", "--context", "ctx-1"]),
        "unarchived thread ctx-1 with Anna\n"
    );
    let marks: Value =
        serde_json::from_str(&std::fs::read_to_string(h.path().join("archive.json")).unwrap())
            .unwrap();
    assert_eq!(marks, json!({"chats": [], "threads": []}));
}

// ------------------------------------------------------------------ error paths

/// Unknown peer, no conversation, unknown thread and unarchiving the unarchived are user
/// errors (exit 1); an undo with an empty trash is exit 4. No failing command creates
/// `trash/` or `archive.json`.
#[test]
fn error_paths() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(Dir::Inbox, &h.anna, &h.me, "why?", "2026-09-14T10:00:00Z").context("ctx-9"),
    );
    let no_side_effects = || {
        assert!(
            !h.path().join("trash").exists(),
            "a failing command created trash/"
        );
        assert!(
            !h.path().join("archive.json").exists(),
            "a failing command created archive.json"
        );
    };

    for cmd in ["archive", "unarchive", "delete"] {
        let (code, _, err) = h.run(&[cmd, "nobody"]);
        assert_eq!(code, 1, "{cmd}: {err}");
        assert!(err.contains("no contact matches"), "{cmd}: {err}");
        no_side_effects();
    }
    let (code, _, err) = h.run(&["archive", "Brooke"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("no conversation with Brooke"), "{err}");
    no_side_effects();
    let (code, _, err) = h.run(&["delete", "Brooke"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("no conversation with Brooke"), "{err}");
    no_side_effects();
    // Unarchive needs no record: with no mark either, only the mark error applies.
    let (code, _, err) = h.run(&["unarchive", "Brooke"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("Brooke is not archived"), "{err}");
    no_side_effects();
    let (code, _, err) = h.run(&["archive", "Anna", "--context", "nope"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("no thread nope with Anna"), "{err}");
    no_side_effects();
    let (code, _, err) = h.run(&["delete", "Anna", "--context", "nope"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("no thread nope with Anna"), "{err}");
    no_side_effects();

    let (code, _, err) = h.run(&["unarchive", "Anna"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("Anna is not archived"), "{err}");
    no_side_effects();
    let (code, _, err) = h.run(&["unarchive", "Anna", "--context", "ctx-9"]);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("thread ctx-9 with Anna is not archived"),
        "{err}"
    );
    no_side_effects();

    let (code, _, err) = h.run(&["undo"]);
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("nothing to undo"), "{err}");
    no_side_effects();
}

/// When a record exists again at its old place, `owl undo` refuses, moves nothing and leaves
/// the batch in the trash; clearing the squatter lets the undo through.
#[test]
fn undo_refuses_to_overwrite_a_record_that_exists_again() {
    let h = Home::new();
    let s = h.spool();
    let rid = put(
        &s,
        Fx::q(Dir::Inbox, &h.anna, &h.me, "why?", "2026-09-14T10:00:00Z"),
    );
    let bytes = std::fs::read(s.path(Dir::Inbox, &rid)).unwrap();
    h.ok(&["delete", "Anna"]);
    let batch = the_batch(h.path());

    // The id comes back (a re-delivered record); the bytes differ from the trashed ones.
    std::fs::write(s.path(Dir::Inbox, &rid), b"{}").unwrap();
    let (code, _, err) = h.run(&["undo"]);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains(&format!("cannot undo: inbox/{rid}.json exists again")),
        "{err}"
    );
    assert_eq!(
        std::fs::read(batch.join("inbox").join(format!("{rid}.json"))).unwrap(),
        bytes,
        "the trashed file is still there, untouched"
    );
    assert_eq!(
        std::fs::read(s.path(Dir::Inbox, &rid)).unwrap(),
        b"{}",
        "the squatter was not overwritten"
    );

    std::fs::remove_file(s.path(Dir::Inbox, &rid)).unwrap();
    assert_eq!(h.ok(&["undo"]), "restored 1 record of Anna\n");
    assert_eq!(std::fs::read(s.path(Dir::Inbox, &rid)).unwrap(), bytes);
}

/// A trash dir without a readable, parseable `batch.json` (a crash before it was written) is
/// skipped and never touched: undo goes on to the next older batch, and with no valid batch
/// left it is the usual exit 4.
#[test]
fn undo_skips_a_batch_dir_without_batch_json() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(Dir::Inbox, &h.anna, &h.me, "why?", "2026-09-14T10:00:00Z"),
    );
    let before = spool_bytes(h.path());
    h.ok(&["delete", "Anna"]);
    // Both names sort after the UUIDv7 batch: one dir empty, one with a garbage batch.json.
    let empty = h.path().join("trash").join("zz-stale-empty");
    let garbage = h.path().join("trash").join("zz-stale-garbage");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::create_dir_all(&garbage).unwrap();
    std::fs::write(garbage.join("batch.json"), b"not json").unwrap();

    assert_eq!(h.ok(&["undo"]), "restored 1 record of Anna\n");
    assert_eq!(spool_bytes(h.path()), before, "every byte is back");
    assert!(
        empty.is_dir() && std::fs::read_dir(&empty).unwrap().next().is_none(),
        "the empty stale dir is left alone"
    );
    assert_eq!(
        std::fs::read(garbage.join("batch.json")).unwrap(),
        b"not json",
        "the garbage batch.json is left alone"
    );

    let (code, _, err) = h.run(&["undo"]);
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("nothing to undo"), "{err}");
}

/// A stray non-record file in the batch dir does not fail the undo: the records move back
/// byte-identical, the success line prints and the dir stays (stray file kept, batch.json
/// gone), so the next undo skips it and restores the next batch.
#[test]
fn undo_leaves_a_stray_file_in_the_batch_dir() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(Dir::Inbox, &h.anna, &h.me, "why?", "2026-09-14T10:00:00Z"),
    );
    let before = spool_bytes(h.path());
    h.ok(&["delete", "Anna"]);
    let batch = the_batch(h.path());
    std::fs::write(batch.join("note.txt"), b"keep me").unwrap();

    assert_eq!(h.ok(&["undo"]), "restored 1 record of Anna\n");
    assert_eq!(spool_bytes(h.path()), before, "every byte is back");
    assert_eq!(
        std::fs::read(batch.join("note.txt")).unwrap(),
        b"keep me",
        "the stray file is not removed"
    );
    assert!(
        !batch.join("batch.json").exists(),
        "batch.json is gone, so the next undo skips the dir"
    );

    // A second delete+undo works: the leftover dir (its name sorts before the new batch) is
    // skipped, the new batch restores and is removed.
    std::thread::sleep(std::time::Duration::from_millis(20));
    h.ok(&["delete", "Anna"]);
    let entries: Vec<PathBuf> = std::fs::read_dir(h.path().join("trash"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(
        entries.len(),
        2,
        "the stray dir and the new batch: {entries:?}"
    );
    let second = entries
        .iter()
        .find(|p| **p != batch)
        .expect("the second batch dir")
        .clone();
    assert_eq!(h.ok(&["undo"]), "restored 1 record of Anna\n");
    assert_eq!(spool_bytes(h.path()), before, "every byte is back again");
    assert_eq!(std::fs::read(batch.join("note.txt")).unwrap(), b"keep me");
    assert!(
        !second.exists(),
        "the second batch dir is removed after its undo"
    );
    let left: Vec<PathBuf> = std::fs::read_dir(h.path().join("trash"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(
        left,
        std::slice::from_ref(&batch),
        "the trash holds only the stray dir"
    );
}

/// The refusal check runs before any move: a clash on a record that sorts after another one
/// of the batch (inbox before outbox) still moves nothing back.
#[test]
fn undo_with_a_late_clash_moves_nothing() {
    let h = Home::new();
    let s = h.spool();
    let inbox = put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "inbox one",
            "2026-09-14T10:00:00Z",
        ),
    );
    let outbox = put(
        &s,
        Fx::q(
            Dir::Outbox,
            &h.me,
            &h.anna,
            "our answer",
            "2026-09-14T10:01:00Z",
        ),
    );
    let inbox_bytes = std::fs::read(s.path(Dir::Inbox, &inbox)).unwrap();
    let outbox_bytes = std::fs::read(s.path(Dir::Outbox, &outbox)).unwrap();
    h.ok(&["delete", "Anna"]);
    let batch = the_batch(h.path());

    // The outbox id comes back; the inbox record would be moved before it in a merged loop.
    std::fs::write(s.path(Dir::Outbox, &outbox), b"{}").unwrap();
    let (code, _, err) = h.run(&["undo"]);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains(&format!("cannot undo: outbox/{outbox}.json exists again")),
        "{err}"
    );
    assert!(
        !s.path(Dir::Inbox, &inbox).exists(),
        "no partial undo: the earlier record did not move back"
    );
    assert_eq!(
        std::fs::read(batch.join("inbox").join(format!("{inbox}.json"))).unwrap(),
        inbox_bytes,
        "the earlier record is still in the trash"
    );
    assert_eq!(
        std::fs::read(batch.join("outbox").join(format!("{outbox}.json"))).unwrap(),
        outbox_bytes
    );
    assert_eq!(
        std::fs::read(s.path(Dir::Outbox, &outbox)).unwrap(),
        b"{}",
        "the squatter was not overwritten"
    );
}

/// A rename that fails mid-delete stops the run (exit 1): the batch keeps `batch.json` and
/// the records that already moved, the rest stay in the spool, and `owl undo` (permissions
/// restored) brings the moved ones back byte-identical.
#[test]
fn an_interrupted_delete_keeps_a_restorable_batch() {
    use std::os::unix::fs::PermissionsExt;

    let h = Home::new();
    let s = h.spool();
    let inbox = put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "inbox one",
            "2026-09-14T10:00:00Z",
        ),
    );
    let asks = put(
        &s,
        Fx::q(Dir::Asks, &h.me, &h.anna, "our ask", "2026-09-14T09:00:00Z").state("waiting"),
    );
    let before = spool_bytes(h.path());

    // Read-only `spool/asks`: the inbox record moves first, the asks rename then fails.
    struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let asks_dir = h.path().join("spool").join("asks");
    std::fs::set_permissions(&asks_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let guard = Guard(asks_dir);

    let (code, _, err) = h.run(&["delete", "Anna"]);
    assert_eq!(code, 1, "{err}");
    let batch = the_batch(h.path());
    assert!(
        batch.join("batch.json").is_file(),
        "batch.json landed first"
    );
    assert!(
        batch.join("inbox").join(format!("{inbox}.json")).is_file(),
        "what moved is in the batch"
    );
    assert!(!s.path(Dir::Inbox, &inbox).exists());
    assert!(
        s.path(Dir::Asks, &asks).exists(),
        "what failed to move stayed in the spool"
    );
    drop(guard);

    assert_eq!(h.ok(&["undo"]), "restored 1 record of Anna\n");
    assert_eq!(spool_bytes(h.path()), before, "every byte is back");
    assert!(!batch.exists(), "the restored batch dir is removed");
}

/// The thread mark is per (fingerprint, context id): archiving Anna's thread never hides
/// Brooke's records under the same context id.
#[test]
fn archiving_a_thread_is_per_peer() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "anna threaded",
            "2026-09-14T10:00:00Z",
        )
        .context("ctx-1")
        .events(json!([ev("2026-09-14T10:00:00Z", "received")])),
    );
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.brooke,
            &h.me,
            "brooke threaded",
            "2026-09-14T10:05:00Z",
        )
        .context("ctx-1")
        .events(json!([ev("2026-09-14T10:05:00Z", "received")])),
    );
    let texts = |args: &[&str]| -> Vec<String> {
        h.json(args)
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["text"].as_str().unwrap().to_string())
            .collect()
    };

    assert_eq!(
        h.ok(&["archive", "Anna", "--context", "ctx-1"]),
        "archived thread ctx-1 with Anna\n"
    );
    assert_eq!(texts(&["thread", "Anna"]), Vec::<String>::new());
    assert_eq!(
        texts(&["thread", "Brooke"]),
        ["brooke threaded"],
        "the same context id of another peer is not archived"
    );
    let (code, out, err) = h.run(&["--json", "thread", "Brooke", "--archived"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap(),
        json!([]),
        "Brooke has no archived thread"
    );
}

// ------------------------------------------------------------------ --json shapes

/// The exact `--json` object of each command: `archived` for archive/unarchive, `records` for
/// delete/undo, `context_id` null on a chat and the id on a thread.
#[test]
fn json_shapes() {
    let h = Home::new();
    let s = h.spool();
    let anna = fp(&h.anna);
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "threaded one",
            "2026-09-14T10:00:00Z",
        )
        .context("ctx-1"),
    );
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "plain one",
            "2026-09-14T11:00:00Z",
        ),
    );

    assert_eq!(
        h.json(&["archive", "Anna", "--context", "ctx-1"]),
        json!({"peer": anna, "context_id": "ctx-1", "archived": true})
    );
    assert_eq!(
        h.json(&["unarchive", "Anna", "--context", "ctx-1"]),
        json!({"peer": anna, "context_id": "ctx-1", "archived": false})
    );
    assert_eq!(
        h.json(&["archive", "Anna"]),
        json!({"peer": anna, "context_id": null, "archived": true})
    );
    assert_eq!(
        h.json(&["unarchive", "Anna"]),
        json!({"peer": anna, "context_id": null, "archived": false})
    );
    assert_eq!(
        h.json(&["delete", "Anna", "--context", "ctx-1"]),
        json!({"peer": anna, "context_id": "ctx-1", "records": 1})
    );
    assert_eq!(
        h.json(&["undo"]),
        json!({"peer": anna, "context_id": "ctx-1", "records": 1})
    );
    assert_eq!(
        h.json(&["delete", "Anna"]),
        json!({"peer": anna, "context_id": null, "records": 2})
    );
    assert_eq!(
        h.json(&["undo"]),
        json!({"peer": anna, "context_id": null, "records": 2})
    );
}

// ------------------------------------------------------------------ archive.json.bak

/// The chat names on the `owl thread` list, in list order.
fn thread_names(h: &Home) -> Vec<String> {
    h.json(&["thread"])
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["from_name"].as_str().unwrap().to_string())
        .collect()
}

/// The fingerprints in the `chats` marks of `archive.json`.
fn archived_chats(h: &Home) -> Vec<String> {
    let marks: Value =
        serde_json::from_slice(&std::fs::read(h.path().join("archive.json")).unwrap()).unwrap();
    marks["chats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// No atomic-write leftovers in the home.
fn assert_no_tmp(h: &Home) {
    for name in ["archive.json.tmp", "archive.json.bak.tmp"] {
        assert!(!h.path().join(name).exists(), "{name} left behind");
    }
}

/// The first save has nothing to rotate (no `.bak`); every later save leaves
/// `archive.json.bak` byte-identical to the `archive.json` from before the call, while
/// `archive.json` holds the new marks.
#[test]
fn save_rotates_a_backup_of_the_previous_archive_json() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "anna asks",
            "2026-09-14T10:00:00Z",
        ),
    );
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.brooke,
            &h.me,
            "brooke asks",
            "2026-09-14T11:00:00Z",
        ),
    );
    let file = h.path().join("archive.json");
    let bak = h.path().join("archive.json.bak");

    assert_eq!(h.ok(&["archive", "Anna"]), "archived Anna\n");
    assert!(file.is_file(), "archive.json was written");
    assert!(!bak.exists(), "no previous file: no backup to rotate");
    assert_no_tmp(&h);
    let before = std::fs::read(&file).unwrap();

    assert_eq!(h.ok(&["archive", "Brooke"]), "archived Brooke\n");
    assert_eq!(
        std::fs::read(&bak).unwrap(),
        before,
        "the backup is the previous archive.json, byte-identical"
    );
    let mut chats = archived_chats(&h);
    chats.sort();
    let mut want = [fp(&h.anna), fp(&h.brooke)];
    want.sort();
    assert_eq!(chats, want, "archive.json holds the new marks");
    assert_no_tmp(&h);
}

/// A corrupt `archive.json` falls back to a good backup: `owl thread` exits 0, the backup's
/// marks apply, one warning names the backup, and reading writes nothing.
#[test]
fn a_corrupt_archive_json_falls_back_to_the_backup() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "anna asks",
            "2026-09-14T10:00:00Z",
        ),
    );
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.brooke,
            &h.me,
            "brooke asks",
            "2026-09-14T11:00:00Z",
        ),
    );
    let before = spool_bytes(h.path());
    h.ok(&["archive", "Anna"]);
    h.ok(&["archive", "Anna"]); // the idempotent save rotates a good backup
    let file = h.path().join("archive.json");
    let bak = h.path().join("archive.json.bak");
    let bak_bytes = std::fs::read(&bak).unwrap();
    std::fs::write(&file, b"{not json").unwrap();

    let (code, _, err) = h.run(&["thread"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        thread_names(&h),
        ["Brooke"],
        "the backup's marks hide Anna's chat"
    );
    assert_eq!(
        err.matches("warning:").count(),
        1,
        "one warning line: {err}"
    );
    assert!(
        err.contains("is corrupt") && err.contains("archive.json.bak"),
        "{err}"
    );
    assert_eq!(spool_bytes(h.path()), before, "the spool is untouched");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"{not json",
        "reading never writes"
    );
    assert_eq!(std::fs::read(&bak).unwrap(), bak_bytes);
}

/// A corrupt `archive.json` without a usable backup (none, or a corrupt one) means an empty
/// archive with a warning: `owl thread` exits 0 and shows every chat.
#[test]
fn a_corrupt_archive_json_without_a_usable_backup_means_empty() {
    for backup in [None, Some(&b"{also bad"[..])] {
        let h = Home::new();
        let s = h.spool();
        put(
            &s,
            Fx::q(
                Dir::Inbox,
                &h.anna,
                &h.me,
                "anna asks",
                "2026-09-14T10:00:00Z",
            ),
        );
        put(
            &s,
            Fx::q(
                Dir::Inbox,
                &h.brooke,
                &h.me,
                "brooke asks",
                "2026-09-14T11:00:00Z",
            ),
        );
        let file = h.path().join("archive.json");
        let bak = h.path().join("archive.json.bak");
        std::fs::write(&file, b"{not json").unwrap();
        match backup {
            Some(bytes) => std::fs::write(&bak, bytes).unwrap(),
            None => assert!(!bak.exists(), "precondition: no backup"),
        }
        let before = spool_bytes(h.path());
        let bak_bytes = backup.map(|_| std::fs::read(&bak).unwrap());

        let (code, out, err) = h.run(&["thread"]);
        assert_eq!(code, 0, "{err}");
        for name in ["Brooke", "Anna"] {
            assert!(
                out.lines().any(|l| l.starts_with(name)),
                "stdout lists {name}: {out}"
            );
        }
        assert_eq!(
            thread_names(&h),
            ["Brooke", "Anna"],
            "nothing is archived: {backup:?}"
        );
        assert_eq!(
            err,
            format!(
                "warning: {} is corrupt (key must be a string at line 1 column 2) and there is no usable backup; nothing is archived\n",
                file.display()
            ),
            "the warning text: {backup:?}"
        );
        assert_eq!(spool_bytes(h.path()), before, "the spool is untouched");
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"{not json",
            "reading never writes"
        );
        match bak_bytes {
            Some(bytes) => assert_eq!(
                std::fs::read(&bak).unwrap(),
                bytes,
                "the corrupt backup is left byte-identical"
            ),
            None => assert!(!bak.exists(), "a read never creates the backup"),
        }
        assert_no_tmp(&h);
    }
}

/// Saving with a corrupt `archive.json` does not rotate the corrupt bytes over a good backup:
/// the backup survives byte-identical and the new file builds on the backup's marks.
#[test]
fn a_save_never_replaces_a_good_backup_with_corrupt_bytes() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "anna asks",
            "2026-09-14T10:00:00Z",
        ),
    );
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.brooke,
            &h.me,
            "brooke asks",
            "2026-09-14T11:00:00Z",
        ),
    );
    h.ok(&["archive", "Anna"]);
    h.ok(&["archive", "Anna"]); // the idempotent save rotates a good backup
    let file = h.path().join("archive.json");
    let bak = h.path().join("archive.json.bak");
    let bak_bytes = std::fs::read(&bak).unwrap();
    std::fs::write(&file, b"{not json").unwrap();

    let (code, out, err) = h.run(&["archive", "Brooke"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "archived Brooke\n");
    assert!(err.contains("archive.json.bak"), "the load warning: {err}");
    assert_eq!(
        std::fs::read(&bak).unwrap(),
        bak_bytes,
        "the good backup is not replaced by the corrupt bytes"
    );
    let mut chats = archived_chats(&h);
    chats.sort();
    let mut want = [fp(&h.anna), fp(&h.brooke)];
    want.sort();
    assert_eq!(
        chats, want,
        "the new file is the backup's marks plus Brooke"
    );
    assert_no_tmp(&h);
}

/// A missing `archive.json` means nothing is archived — the backup is only for a corrupt
/// file: with no `archive.json` but an `archive.json.bak` archiving Anna, `owl thread` exits
/// 0, shows both chats, warns about nothing, and writes nothing.
#[test]
fn a_missing_archive_json_ignores_the_backup() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "anna asks",
            "2026-09-14T10:00:00Z",
        ),
    );
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.brooke,
            &h.me,
            "brooke asks",
            "2026-09-14T11:00:00Z",
        ),
    );
    h.ok(&["archive", "Anna"]);
    h.ok(&["archive", "Anna"]); // the idempotent save rotates a good backup
    let file = h.path().join("archive.json");
    let bak = h.path().join("archive.json.bak");
    let bak_bytes = std::fs::read(&bak).unwrap();
    std::fs::remove_file(&file).unwrap();

    let (code, _, err) = h.run(&["thread"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        thread_names(&h),
        ["Brooke", "Anna"],
        "a missing archive.json means nothing is archived"
    );
    assert!(err.is_empty(), "no warning for a missing file: {err}");
    assert!(!file.exists(), "reading never writes");
    assert_eq!(std::fs::read(&bak).unwrap(), bak_bytes);
}
