//! The per-record event log (`meta.events`) and `owl thread` — the peer list and one
//! person's whole conversation. Every test drives the `owl` binary against its own temp home
//! and pins `OWLPOST_CLAUDE_HOME` through `common::claude_home()`; the only harness that ever
//! runs is `tests/fixtures/fake-harness.sh`.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{PATH, PROJECT, Peer, claude_home, fp, id, prepare_home_with};
use owlpost::config::Harness;
use owlpost::envelope::{self, Envelope, Payload};
use owlpost::events::EVENTS_KEY;
use owlpost::identity::Identity;
use owlpost::route;
use owlpost::spool::{Dir, Record, Spool};
use serde_json::{Value, json};
use tempfile::TempDir;

const OWL: &str = env!("CARGO_BIN_EXE_owl");
const FAKE_HARNESS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/fake-harness.sh"
);
const FAKE_EDITOR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake-editor.sh");

// ------------------------------------------------------------------ fixture home

struct Home {
    dir: TempDir,
    _checkout: TempDir,
    me: Identity,
    anna: Identity,
    brooke: Identity,
    cezary: Identity,
}

impl Home {
    fn new() -> Home {
        Self::with(|_| {})
    }

    fn with(tweak: impl FnOnce(&mut owlpost::config::Config)) -> Home {
        let dir = tempfile::tempdir().unwrap();
        let checkout = tempfile::tempdir().unwrap();
        // Anna's fingerprint sorts AFTER Brooke's while her name sorts before his: the
        // `last_ts` tie in the peer list can then only be broken by `from_name`.
        let (me, anna, brooke, cezary) = (id(2), id(3), id(1), id(5));
        let peers = [
            Peer::new(&anna, "Anna", None),
            Peer::new(&brooke, "Brooke", None),
            Peer::new(&cezary, "Cezary", None),
        ];
        let checkout_path = checkout.path().to_string_lossy().into_owned();
        prepare_home_with(dir.path(), &me, &peers, |cfg| {
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
            cfg.projects.insert(PROJECT.into(), checkout_path);
            tweak(cfg);
        });
        Home {
            dir,
            _checkout: checkout,
            me,
            anna,
            brooke,
            cezary,
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
            .env_remove("EDITOR")
            .env_remove("FAKE_HARNESS_LOG")
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
    seen: bool,
    context_id: Option<&'a str>,
    /// `None` leaves the record legacy (no `meta.events`, `of` derives).
    events: Option<Value>,
    done_at: Option<&'a str>,
    draft: Option<Value>,
    answer: bool,
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
            seen: false,
            context_id: None,
            events: None,
            done_at: None,
            draft: None,
            answer: false,
        }
    }

    fn state(mut self, s: &'a str) -> Fx<'a> {
        self.state = s;
        self
    }

    fn seen(mut self) -> Fx<'a> {
        self.seen = true;
        self
    }

    fn events(mut self, e: Value) -> Fx<'a> {
        self.events = Some(e);
        self
    }

    fn context(mut self, c: &'a str) -> Fx<'a> {
        self.context_id = Some(c);
        self
    }

    fn done_at(mut self, t: &'a str) -> Fx<'a> {
        self.done_at = Some(t);
        self
    }

    fn draft(mut self, text: &str) -> Fx<'a> {
        self.draft = Some(json!({
            "text": text, "harness": "fake", "redactions": 0, "status": "ok",
            "drafted_at": "2026-09-14T10:04:40Z",
        }));
        self
    }

    fn answer(mut self) -> Fx<'a> {
        self.answer = true;
        self
    }
}

/// Writes the fixture into the spool and returns its record id.
fn put(spool: &Spool, f: Fx<'_>) -> String {
    let mut payload = if f.answer {
        let q = Payload::question(&fp(f.to), &fp(f.from), PROJECT, Some(PATH), "q");
        Payload::answer(&q, f.text, "fake", 0, false)
    } else {
        Payload::question(&fp(f.from), &fp(f.to), PROJECT, Some(PATH), f.text)
    };
    payload.from = fp(f.from);
    payload.to = fp(f.to);
    payload.context_id = f.context_id.map(str::to_string);
    let env = Envelope::sign(&payload, f.from);
    let mut meta = json!({ "peer": fp(f.from) });
    if let Some(e) = f.events {
        meta[EVENTS_KEY] = e;
    }
    if let Some(t) = f.done_at {
        meta["done_at"] = json!(t);
    }
    let rec = Record {
        raw: env.raw,
        sig: env.sig,
        state: f.state.into(),
        seen: f.seen,
        received_at: f.at.into(),
        draft: f.draft,
        meta,
    };
    spool.put(f.dir, &payload.id, &rec).unwrap();
    payload.id
}

fn ev(ts: &str, kind: &str) -> Value {
    json!({ "ts": ts, "kind": kind })
}

/// Every file under `spool/`, path → bytes: the read-only before/after comparison.
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

// ------------------------------------------------------------------ the peer list

/// One row per peer, sorted by `last_ts` descending with `from_name` ascending as the
/// tiebreak. The fixture is inserted out of order and orders differently under the two keys:
/// by name it would be Anna, Brooke, Cezary; by time Cezary is newest and Anna/Brooke share a
/// timestamp, so only the name tiebreak can separate them.
#[test]
fn thread_list_rows_and_sort_order() {
    let h = Home::new();
    let s = h.spool();
    // Out of order on purpose: Brooke first, the newest peer in the middle.
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.brooke,
            &h.me,
            "brooke asks",
            "2026-09-14T10:00:00Z",
        )
        .seen(),
    );
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.cezary,
            &h.me,
            "cezary asks",
            "2026-09-14T11:00:00Z",
        )
        .state("consent"),
    );
    // Anna: two unseen questions (one open `pending`, one already `drafted`, both open), an
    // answer of hers that is unseen but never "open", and the newest record shares Brooke's
    // timestamp so the name tiebreak decides.
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "older question",
            "2026-09-14T09:00:00Z",
        )
        .state("drafted"),
    );
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "why is the token rotated?",
            "2026-09-14T10:00:00Z",
        ),
    );
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "an answer of hers",
            "2026-09-14T09:30:00Z",
        )
        .answer(),
    );

    let rows = h.json(&["thread"]);
    let names: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["from_name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["Cezary", "Anna", "Brooke"],
        "newest first, name breaks the tie: {rows:#}"
    );
    let anna = &rows[1];
    assert_eq!(anna["from"], fp(&h.anna));
    assert_eq!(anna["last_ts"], "2026-09-14T10:00:00Z");
    assert_eq!(anna["unseen"], 3, "two questions and her answer");
    assert_eq!(anna["open"], 2, "the answer is unseen but never open");
    assert_eq!(anna["last_summary"], "why is the token rotated?");
    assert_eq!(rows[2]["unseen"], 0, "Brooke's one record is seen");
    assert_eq!(rows[0]["open"], 1, "a consent record is open");

    // Human format: the §9 columns, same order, and every cell of every row in place.
    // `print_table` pads each cell and joins with two spaces, so a run of two or more
    // spaces separates cells while the single spaces inside a summary are kept.
    let table = h.ok(&["thread"]);
    let cells = |line: &str| -> Vec<String> {
        line.split("  ")
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .collect()
    };
    let mut lines = table.lines();
    assert_eq!(
        cells(lines.next().unwrap()),
        ["PEER", "UNSEEN", "OPEN", "LAST", "SUMMARY"]
    );
    let rows: Vec<Vec<String>> = lines.map(cells).collect();
    let row = |cs: [&str; 5]| -> Vec<String> { cs.iter().map(|c| c.to_string()).collect() };
    // Anna is 3 unseen / 2 open and Brooke 0 / 1, so swapping the two columns is visible.
    assert_eq!(
        rows,
        vec![
            row(["Cezary", "1", "1", "2026-09-14T11:00:00Z", "cezary asks"]),
            row([
                "Anna",
                "3",
                "2",
                "2026-09-14T10:00:00Z",
                "why is the token rotated?"
            ]),
            row(["Brooke", "0", "1", "2026-09-14T10:00:00Z", "brooke asks"]),
        ],
        "table:\n{table}"
    );
}

/// Negative twin: nothing in any of the four directories is exit 4 `no threads`.
#[test]
fn thread_list_without_records_exits_4() {
    let h = Home::new();
    for args in [vec!["thread"], vec!["--json", "thread"]] {
        let (code, out, err) = h.run(&args);
        assert_eq!(code, 4, "{args:?}: {err}");
        assert_eq!(out, "", "{args:?} prints nothing on stdout");
        assert!(err.contains("no threads"), "{args:?}: {err}");
    }
}

// ------------------------------------------------------------------ the timeline

/// Every event of every record of that peer in `inbox/`, `outbox/`, `asks/` and `done/`,
/// oldest first. The fixture is inserted out of order, two events share a second across two
/// records (the `record_id` tiebreak) and two share a second inside one record (the event
/// index tiebreak). A second peer's records never appear, the text comes back uncut, and the
/// optional keys are present exactly where the payload or the event has them.
#[test]
fn thread_timeline_merges_four_directories_in_order() {
    let h = Home::new();
    let s = h.spool();
    let long = "ł".repeat(90);
    // Another peer's record: must never show up.
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.brooke,
            &h.me,
            "brooke's question",
            "2026-09-14T10:00:00Z",
        )
        .events(json!([ev("2026-09-14T10:00:00Z", "received")])),
    );
    // done/: the oldest exchange, two events in the same second inside one record.
    let done = put(
        &s,
        Fx::q(
            Dir::Done,
            &h.anna,
            &h.me,
            "first question",
            "2026-09-14T09:00:00Z",
        )
        .state("answered")
        .done_at("2026-09-14T09:00:30Z")
        .events(json!([
            ev("2026-09-14T09:00:00Z", "received"),
            ev("2026-09-14T09:00:00Z", "held"),
            { "ts": "2026-09-14T09:00:30Z", "kind": "sent", "by": "human",
              "detail": { "answer_id": "0191" } },
        ])),
    );
    // asks/: our own question to her, same second as the outbox event below.
    let ask = put(
        &s,
        Fx::q(
            Dir::Asks,
            &h.me,
            &h.anna,
            "what we asked her",
            "2026-09-14T09:30:00Z",
        )
        .state("waiting")
        .events(json!([
            { "ts": "2026-09-14T09:30:00Z", "kind": "asked", "by": "human" },
        ])),
    );
    // outbox/: our answer to her, sharing a second with the asks/ record above — and
    // written after it, so its id sorts after the ask's while `records()` walks it first.
    let outbox = put(
        &s,
        Fx::q(
            Dir::Outbox,
            &h.me,
            &h.anna,
            "our answer text",
            "2026-09-14T09:30:00Z",
        )
        .state("unacked")
        .answer()
        .events(json!([ev("2026-09-14T09:30:00Z", "received")])),
    );
    // inbox/: the newest, threaded, with a context snippet and a draft.
    let inbox = put(
        &s,
        Fx::q(Dir::Inbox, &h.anna, &h.me, &long, "2026-09-14T10:00:00Z")
            .state("drafted")
            .context("ctx-1")
            .draft("our draft answer")
            .events(json!([
                ev("2026-09-14T10:00:00Z", "received"),
                { "ts": "2026-09-14T10:04:40Z", "kind": "drafted", "by": "fake",
                  "detail": { "redactions": 1, "status": "ok" } },
            ])),
    );

    let rows = h.json(&["thread", "Anna"]);
    let rows = rows.as_array().unwrap();
    let seq: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|r| {
            (
                r["ts"].as_str().unwrap(),
                r["kind"].as_str().unwrap(),
                r["dir"].as_str().unwrap(),
            )
        })
        .collect();
    // Two records share 09:30:00; the id tiebreak is deterministic, so assert the set per
    // timestamp rather than a guessed record order, and the overall order by timestamp.
    let stamps: Vec<&str> = seq.iter().map(|(t, _, _)| *t).collect();
    let mut sorted = stamps.clone();
    sorted.sort();
    assert_eq!(stamps, sorted, "ascending by ts: {seq:?}");
    assert_eq!(seq.len(), 7, "every event of all four directories: {seq:?}");
    assert_eq!(
        (seq[0].1, seq[1].1),
        ("received", "held"),
        "same-second events keep their order inside a record"
    );
    // The two records stamped in the same second order by `record_id`, not by insertion.
    let same: Vec<&str> = rows
        .iter()
        .filter(|r| r["ts"] == "2026-09-14T09:30:00Z")
        .map(|r| r["record_id"].as_str().unwrap())
        .collect();
    let mut want = vec![outbox.as_str(), ask.as_str()];
    want.sort();
    assert_eq!(same, want, "same-second events order by record_id");
    assert!(
        !rows
            .iter()
            .any(|r| r["text"].as_str().unwrap().contains("brooke")),
        "another peer never appears: {rows:?}"
    );

    let by_record =
        |id: &str| -> Vec<&Value> { rows.iter().filter(|r| r["record_id"] == id).collect() };
    assert_eq!(by_record(&done).len(), 3);
    assert_eq!(by_record(&outbox).len(), 1);
    assert_eq!(by_record(&ask).len(), 1);
    let inbox_rows = by_record(&inbox);
    assert_eq!(inbox_rows.len(), 2);

    // Directions: hers in, ours out.
    assert_eq!(by_record(&done)[0]["dir"], "in");
    assert_eq!(by_record(&outbox)[0]["dir"], "out");
    assert_eq!(by_record(&ask)[0]["dir"], "out");
    assert_eq!(inbox_rows[0]["dir"], "in");

    // The newest record's row: every documented key, the full text and the optionals.
    let drafted = inbox_rows[1];
    assert_eq!(drafted["kind"], "drafted");
    assert_eq!(drafted["type"], "question");
    assert_eq!(drafted["state"], "drafted");
    assert_eq!(drafted["project"], PROJECT);
    assert_eq!(drafted["path"], PATH);
    assert_eq!(drafted["context_id"], "ctx-1");
    assert_eq!(drafted["by"], "fake");
    assert_eq!(drafted["detail"]["redactions"], 1);
    assert_eq!(drafted["harness"], "fake");
    assert_eq!(
        drafted["text"].as_str().unwrap().chars().count(),
        90,
        "the full text, never cut to SUMMARY_CHARS"
    );

    // The absent optionals really are absent, not null.
    let asked = by_record(&ask)[0].as_object().unwrap();
    assert!(!asked.contains_key("detail"), "{asked:?}");
    assert!(!asked.contains_key("context"), "{asked:?}");
    assert_eq!(asked["by"], "human");
    assert_eq!(
        asked["context_id"],
        json!(null),
        "unthreaded: null, not absent"
    );
    let sent = by_record(&done)[2].as_object().unwrap();
    assert!(
        !sent.contains_key("harness"),
        "no draft, no harness: {sent:?}"
    );
    // The peer's own message never carries our harness, even on a record that has a draft.
    let received = inbox_rows[0].as_object().unwrap();
    assert!(
        !received.contains_key("harness"),
        "a `received` row is the peer's message, not our reply: {received:?}"
    );
}

/// `body.context` reaches the row when the payload carries one.
#[test]
fn thread_timeline_carries_the_asker_snippet() {
    let h = Home::new();
    let s = h.spool();
    let mut payload = Payload::question(&fp(&h.anna), &fp(&h.me), PROJECT, Some(PATH), "why?");
    if let owlpost::envelope::Body::Question { context, .. } = &mut payload.body {
        *context = Some("fn main() {}".into());
    }
    let env = Envelope::sign(&payload, &h.anna);
    s.put(
        Dir::Inbox,
        &payload.id,
        &Record {
            raw: env.raw,
            sig: env.sig,
            state: "pending".into(),
            seen: false,
            received_at: "2026-09-14T10:00:00Z".into(),
            draft: None,
            meta: json!({ "peer": fp(&h.anna), "events": [ev("2026-09-14T10:00:00Z", "received")] }),
        },
    )
    .unwrap();
    let rows = h.json(&["thread", "Anna"]);
    assert_eq!(rows[0]["context"], "fn main() {}");
}

// ------------------------------------------------------------------ unknown kinds

/// A kind this version does not know passes through `--json` untouched, renders as its
/// own name in the human format, and the events around it survive.
#[test]
fn unknown_event_kind_passes_through_and_renders() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(Dir::Inbox, &h.anna, &h.me, "why?", "2026-09-14T10:00:00Z")
            .state("pending")
            .events(json!([
                ev("2026-09-14T10:00:00Z", "received"),
                // `content-requested` has a meaning now; this fixture needs a kind no
                // version of the renderer knows.
                { "ts": "2026-09-14T10:01:00Z", "kind": "parcel-requested", "by": "peer",
                  "detail": { "files": 2 } },
                ev("2026-09-14T10:02:00Z", "held"),
            ])),
    );
    let rows = h.json(&["thread", "Anna"]);
    let kinds: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["received", "parcel-requested", "held"]);
    assert_eq!(rows[1]["detail"]["files"], 2);
    assert_eq!(rows[1]["by"], "peer");

    let text = h.ok(&["thread", "Anna"]);
    assert!(
        text.lines()
            .any(|l| l.contains("parcel-requested") && l.contains("by peer")),
        "the unknown kind prints by name: {text}"
    );
    assert!(
        text.lines().any(|l| l.contains("files=2")),
        "its detail prints as k=v: {text}"
    );
    assert!(
        text.contains("held"),
        "the events around it survive: {text}"
    );
}

// ------------------------------------------------------------------ human format

/// The header line, the peer's question as the `--format claude` table byte-for-byte,
/// and our own draft in a ```text block and never in a table.
#[test]
fn thread_human_format_tables_peer_messages_and_fences_ours() {
    let h = Home::new();
    let s = h.spool();
    // A draftless record: `owl show --format claude` renders the bare table, so the thread's
    // block for it must match byte-for-byte.
    let qid = put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "why is the token rotated?",
            "2026-09-14T10:00:00Z",
        )
        .state("pending")
        .events(json!([ev("2026-09-14T10:00:00Z", "received")])),
    );
    // A second record of hers that we have drafted an answer to: our words, never a table.
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "and what about the refresh window?",
            "2026-09-14T10:02:00Z",
        )
        .state("drafted")
        .draft("our draft answer")
        .events(json!([
            ev("2026-09-14T10:02:00Z", "received"),
            { "ts": "2026-09-14T10:04:40Z", "kind": "drafted", "by": "fake" },
        ])),
    );
    let thread = h.ok(&["thread", "Anna"]);
    let show = h.ok(&["show", &qid, "--format", "claude"]);

    let header = thread.lines().next().unwrap();
    assert!(header.starts_with(&fp(&h.anna)), "{header}");
    assert!(header.contains("— Anna · 3 events · last "), "{header}");
    assert!(
        thread.contains(show.trim_end()),
        "the peer's message table must be byte-identical to `owl show --format claude`\n\
         thread:\n{thread}\nshow:\n{show}"
    );
    assert!(
        thread.lines().any(|l| l.starts_with("| 🦉 ")),
        "a table always means from a peer: {thread}"
    );
    // Our own side: one line, and the draft fenced, never a table row.
    let ours = thread
        .lines()
        .find(|l| l.contains("drafted"))
        .expect("a drafted line");
    assert!(ours.contains(" · by fake"), "{ours}");
    assert!(
        !ours.starts_with('|'),
        "our own event is never a table row: {ours}"
    );
    assert!(
        thread.contains("```text\nour draft answer\n```"),
        "our own draft comes fenced: {thread}"
    );
    assert!(
        !thread.contains("| our draft answer |"),
        "our draft is never a table row: {thread}"
    );
    assert!(
        !thread.contains("```text\nand what about the refresh window?"),
        "the fence holds our draft, not the peer's question: {thread}"
    );
}

/// `--since` and `--context` each drop events, with the negative twin proving the
/// excluded event is there in the unfiltered call.
#[test]
fn thread_since_and_context_filter_with_negative_twins() {
    let h = Home::new();
    let s = h.spool();
    // `--since 1d` is measured against the wall clock, so both fixtures are stamped relative
    // to now: a fixed date would drop out of the window on the day after it was written.
    let ago = |secs: u64| envelope::unix_to_rfc3339(envelope::now_unix() - secs);
    let (old, recent) = (ago(3 * 86_400), ago(7_200));
    put(
        &s,
        Fx::q(Dir::Done, &h.anna, &h.me, "old question", &old)
            .state("answered")
            .events(json!([ev(&old, "received")])),
    );
    put(
        &s,
        Fx::q(Dir::Inbox, &h.anna, &h.me, "threaded question", &recent)
            .context("ctx-keep")
            .events(json!([ev(&recent, "received")])),
    );
    let texts = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|r| r["text"].as_str().unwrap().to_string())
            .collect()
    };
    let all = h.json(&["thread", "Anna"]);
    assert_eq!(texts(&all).len(), 2, "the twin: both are there unfiltered");
    assert!(texts(&all).contains(&"old question".to_string()));

    let since = h.json(&["thread", "Anna", "--since", "1d"]);
    assert_eq!(
        texts(&since),
        ["threaded question"],
        "--since dropped the old one"
    );
    let ctx = h.json(&["thread", "Anna", "--context", "ctx-keep"]);
    assert_eq!(
        texts(&ctx),
        ["threaded question"],
        "--context kept one thread"
    );
    // Both filters together, and an empty result is exit 0 with `[]`.
    let both = h.json(&["thread", "Anna", "--since", "1d", "--context", "ctx-keep"]);
    assert_eq!(texts(&both), ["threaded question"]);
    let none = h.json(&["thread", "Anna", "--context", "nothing-matches"]);
    assert_eq!(none, json!([]), "an empty result is exit 0 and []");
    let (code, out, _) = h.run(&["thread", "Anna", "--context", "nothing-matches"]);
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 1, "the header line alone: {out:?}");
    assert!(out.contains("· 0 events ·"), "{out}");
}

// ------------------------------------------------------------------ legacy records

/// A record with no `meta.events` gets its log derived on read — and reading it leaves
/// the file byte-identical, so a read-only command never migrates anything.
#[test]
fn legacy_records_derive_their_log_and_are_never_rewritten() {
    let h = Home::new();
    let s = h.spool();
    let open = put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "legacy question",
            "2026-09-14T10:00:00Z",
        ),
    );
    let closed = put(
        &s,
        Fx::q(
            Dir::Done,
            &h.anna,
            &h.me,
            "legacy answered",
            "2026-09-14T09:00:00Z",
        )
        .state("answered")
        .done_at("2026-09-14T09:30:00Z"),
    );
    let declined = put(
        &s,
        Fx::q(
            Dir::Done,
            &h.me,
            &h.anna,
            "our declined ask",
            "2026-09-14T08:00:00Z",
        )
        .state("declined")
        .done_at("2026-09-14T08:30:00Z"),
    );
    let before = spool_bytes(h.path());

    let rows = h.json(&["thread", "Anna"]);
    let of = |id: &str| -> Vec<(String, String)> {
        rows.as_array()
            .unwrap()
            .iter()
            .filter(|r| r["record_id"] == id)
            .map(|r| {
                (
                    r["kind"].as_str().unwrap().to_string(),
                    r["ts"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    assert_eq!(
        of(&open),
        [("received".to_string(), "2026-09-14T10:00:00Z".to_string())],
        "an open legacy record derives its `received` and nothing else"
    );
    assert_eq!(
        of(&closed),
        [
            ("received".to_string(), "2026-09-14T09:00:00Z".to_string()),
            ("sent".to_string(), "2026-09-14T09:30:00Z".to_string()),
        ]
    );
    assert_eq!(
        of(&declined).last().unwrap().0,
        "denied",
        "declined → denied"
    );
    let declined_row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["record_id"] == declined && r["kind"] == "denied")
        .unwrap();
    assert_eq!(declined_row["detail"]["by"], "peer");

    // The human format lists them too, and nothing on disk moved.
    let text = h.ok(&["thread", "Anna"]);
    assert!(text.contains("legacy question"), "{text}");
    assert_eq!(
        spool_bytes(h.path()),
        before,
        "`owl thread` never migrates a legacy log"
    );
}

// ------------------------------------------------------------------ read-only

/// Both forms of `owl thread` open no socket, run no harness and write nothing — against
/// a contact whose endpoint is a listener counting connections, with the fake harness armed
/// through `$FAKE_HARNESS_LOG`, comparing every spool file's bytes before and after.
#[test]
fn thread_is_read_only() {
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    let accepting = std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stream.is_err() {
                break;
            }
            seen.fetch_add(1, Ordering::SeqCst);
        }
    });

    let h = Home::new();
    // Point Anna's contact at the counting listener.
    common::write_contact_full(
        h.path(),
        &Peer::new(&h.anna, "Anna", None),
        &[&format!("https://{addr}")],
        &["anna@example.org"],
    );
    let s = h.spool();
    let id = put(
        &s,
        Fx::q(Dir::Inbox, &h.anna, &h.me, "why?", "2026-09-14T10:00:00Z").state("consent"),
    );
    // A routing file and a wake file that must survive.
    let routing = route::routing_path(h.path(), &id);
    std::fs::create_dir_all(routing.parent().unwrap()).unwrap();
    std::fs::write(&routing, br#"{"current":"sid-1"}"#).unwrap();
    let wake = route::wake_file(h.path(), "sid-1", &id);
    std::fs::create_dir_all(wake.parent().unwrap()).unwrap();
    std::fs::write(&wake, b"wake").unwrap();

    let harness_log = h.path().join("fake-harness.log");
    let before = spool_bytes(h.path());
    for args in [
        vec!["thread"],
        vec!["thread", "Anna"],
        vec!["--json", "thread"],
        vec!["--json", "thread", "Anna"],
    ] {
        let out = h
            .owl()
            .args(&args)
            .env("FAKE_HARNESS_LOG", &harness_log)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "owl {args:?}");
    }
    assert_eq!(spool_bytes(h.path()), before, "no spool file changed");
    assert!(
        !harness_log.exists(),
        "no harness ran: {} was created",
        harness_log.display()
    );
    assert_eq!(count.load(Ordering::SeqCst), 0, "no socket was opened");
    assert!(
        !h.path().join(owlpost::render::MARKERS_FILE).exists(),
        "no per-peer marker was assigned: `owl thread` stays out of markers.json"
    );
    // Positive control: a command that does dial the contact increments the same counter.
    let _ = h.run(&["card", "Anna"]);
    assert!(
        count.load(Ordering::SeqCst) > 0,
        "the connection counter works — the zero above means nothing was dialled"
    );
    assert!(
        !s.get(Dir::Inbox, &id).unwrap().unwrap().seen,
        "seen stays false"
    );
    assert!(routing.is_file(), "the routing file survives");
    assert!(wake.is_file(), "the wake file survives");
    drop(accepting);
}

// ------------------------------------------------------------------ context_id

/// `owl inbox --json` and `owl history --json` gain `context_id` — the payload's value
/// for a threaded record, `null` for one without — every pre-existing key is unchanged and
/// the plain tables keep their columns.
#[test]
fn inbox_and_history_json_carry_context_id() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(
            Dir::Inbox,
            &h.anna,
            &h.me,
            "threaded",
            "2026-09-14T10:00:00Z",
        )
        .context("ctx-9"),
    );
    put(
        &s,
        Fx::q(Dir::Inbox, &h.anna, &h.me, "plain", "2026-09-14T09:00:00Z"),
    );
    put(
        &s,
        Fx::q(
            Dir::Done,
            &h.anna,
            &h.me,
            "finished threaded",
            "2026-09-14T08:00:00Z",
        )
        .state("answered")
        .done_at("2026-09-14T08:30:00Z")
        .context("ctx-9"),
    );
    put(
        &s,
        Fx::q(
            Dir::Done,
            &h.anna,
            &h.me,
            "finished plain",
            "2026-09-14T07:00:00Z",
        )
        .state("answered")
        .done_at("2026-09-14T07:30:00Z"),
    );

    let inbox = h.json(&["inbox"]);
    // Row by row: the threaded record carries its own thread id, the plain one carries null.
    let row_for = |v: &Value, text: &str| -> Value {
        v.as_array()
            .unwrap()
            .iter()
            .find(|r| r["summary"] == text || r["text"] == text)
            .unwrap_or_else(|| panic!("no row for {text:?}: {v:#}"))
            .clone()
    };
    assert_eq!(row_for(&inbox, "threaded")["context_id"], "ctx-9");
    assert_eq!(row_for(&inbox, "plain")["context_id"], json!(null));
    // Every pre-existing key still there, on every row.
    for row in inbox.as_array().unwrap() {
        for key in [
            "id",
            "from",
            "from_name",
            "to",
            "type",
            "state",
            "seen",
            "path",
            "project",
            "received_at",
            "age_secs",
            "age",
            "has_draft",
            "summary",
        ] {
            assert!(row.get(key).is_some(), "inbox row lost {key}: {row:#}");
        }
    }
    let history = h.json(&["history"]);
    for row in history.as_array().unwrap() {
        for key in [
            "id",
            "peer",
            "peer_name",
            "from",
            "to",
            "type",
            "state",
            "project",
            "path",
            "text",
            "received_at",
            "age",
            "done_at",
            "context_id",
        ] {
            assert!(row.get(key).is_some(), "history row lost {key}: {row:#}");
        }
    }
    assert_eq!(
        row_for(&history, "finished threaded")["context_id"],
        "ctx-9"
    );
    assert_eq!(
        row_for(&history, "finished plain")["context_id"],
        json!(null)
    );

    // The human tables are the §9 ones, unchanged by the additive key.
    let inbox_table = h.ok(&["inbox"]);
    assert_eq!(
        inbox_table
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["#", "ID", "FROM", "TYPE", "STATE", "PATH", "AGE", "SUMMARY"]
    );
    assert!(
        !inbox_table.to_lowercase().contains("context"),
        "the inbox table gained no column: {inbox_table}"
    );
    let history_table = h.ok(&["history"]);
    assert_eq!(
        history_table
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["ID", "PEER", "TYPE", "STATE", "PATH", "RECEIVED"]
    );
    assert!(
        !history_table.to_lowercase().contains("context"),
        "the history table gained no column: {history_table}"
    );
}

/// The peer argument takes the §5 forms, and an unknown one is a user error (exit 1).
#[test]
fn thread_resolves_the_peer_and_refuses_an_unknown_one() {
    let h = Home::new();
    let s = h.spool();
    put(
        &s,
        Fx::q(Dir::Inbox, &h.anna, &h.me, "why?", "2026-09-14T10:00:00Z"),
    );
    for form in [fp(&h.anna), "anna@example.org".into(), "ann".into()] {
        let rows = h.json(&["thread", &form]);
        assert_eq!(rows.as_array().unwrap().len(), 1, "form {form:?}");
    }
    let (code, _, err) = h.run(&["thread", "nobody"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("no contact matches"), "{err}");
}

// ------------------------------------------------------------------ the real flow

/// A running daemon whose home answers with the fake harness, plus the checkout its one
/// project points at. `Anna` has no policy, so her questions arrive at `consent`.
async fn daemon_home(anna: &Identity) -> (common::TestDaemon, TempDir) {
    let checkout = tempfile::tempdir().unwrap();
    let path = checkout.path().to_string_lossy().into_owned();
    let d = common::spawn_daemon_with(2, &[Peer::new(anna, "Anna", None)], |cfg| {
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
        cfg.projects.insert(PROJECT.into(), path);
    })
    .await;
    (d, checkout)
}

/// Posts one signed question from `anna` and returns its record id, held at `consent`.
async fn post_question(d: &common::TestDaemon, anna: &Identity, text: &str) -> String {
    let payload = common::question(anna, &d.id, text);
    let env = Envelope::sign(&payload, anna);
    let cl = common::client(Some(anna), &d.id);
    let resp = common::post_envelope(&cl, d, &env).await;
    assert_eq!(resp.status().as_u16(), 202, "the question was accepted");
    let rec = d.spool().get(Dir::Inbox, &payload.id).unwrap().unwrap();
    assert_eq!(rec.state, "consent", "no policy means held for consent");
    payload.id
}

fn owl_in(home: &Path) -> Command {
    let mut c = Command::new(OWL);
    c.env_remove("OWLPOST_HOME")
        .env_remove("EDITOR")
        .env(route::CLAUDE_HOME_ENV, claude_home())
        .arg("--home")
        .arg(home);
    c
}

fn run_ok(home: &Path, args: &[&str]) {
    let out = owl_in(home).args(args).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "owl {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The `(kind, by, detail)` triples of a record's stored log.
fn log_of(spool: &Spool, dir: Dir, id: &str) -> Vec<(String, Option<String>, Value)> {
    let rec = spool
        .get(dir, id)
        .unwrap()
        .unwrap_or_else(|| panic!("{id} in {}", dir.name()));
    rec.meta[EVENTS_KEY]
        .as_array()
        .unwrap_or_else(|| panic!("no stored log on {id}: {}", rec.meta))
        .iter()
        .map(|e| {
            assert!(e["ts"].as_str().is_some_and(|t| t.ends_with('Z')), "{e}");
            (
                e["kind"].as_str().unwrap().to_string(),
                e["by"].as_str().map(str::to_string),
                e.get("detail").cloned().unwrap_or(Value::Null),
            )
        })
        .collect()
}

fn kinds_of(log: &[(String, Option<String>, Value)]) -> Vec<&str> {
    log.iter().map(|(k, _, _)| k.as_str()).collect()
}

/// A question held for consent, allowed `--once`, drafted with the fake harness and sent
/// carries exactly `received, held, allowed, drafted, sent` in `done/`, with the documented
/// `by` and `detail.scope`.
#[tokio::test]
async fn allowed_drafted_and_sent_writes_the_documented_log() {
    let anna = id(1);
    let (d, _checkout) = daemon_home(&anna).await;
    let qid = post_question(&d, &anna, "why is the token rotated?").await;
    let home = d.home().to_path_buf();
    assert_eq!(
        kinds_of(&log_of(&d.spool(), Dir::Inbox, &qid)),
        ["received", "held"],
        "record birth"
    );
    run_ok(&home, &["allow", "Anna", "--once"]);
    run_ok(&home, &["draft", &qid]);
    run_ok(&home, &["send", &qid]);

    let log = log_of(&d.spool(), Dir::Done, &qid);
    assert_eq!(
        kinds_of(&log),
        ["received", "held", "allowed", "drafted", "sent"],
        "{log:?}"
    );
    assert_eq!(log[2].1.as_deref(), Some("human"), "allowed by the human");
    assert_eq!(log[2].2["scope"], "once", "--once is recorded as the scope");
    assert_eq!(log[3].1.as_deref(), Some("fake"), "drafted by the harness");
    assert_eq!(log[3].2["status"], "ok");
    assert_eq!(log[4].1.as_deref(), Some("human"), "sent by the human");
    assert!(log[4].2["answer_id"].is_string(), "{log:?}");
    // The timestamps never go backwards.
    let ts: Vec<String> = d.spool().get(Dir::Done, &qid).unwrap().unwrap().meta[EVENTS_KEY]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["ts"].as_str().unwrap().to_string())
        .collect();
    let mut sorted = ts.clone();
    sorted.sort();
    assert_eq!(ts, sorted, "the log is chronological");
    d.running.shutdown();
}

/// Twin: `owl deny` writes `denied` and `owl reject` writes `rejected`.
#[tokio::test]
async fn denied_and_rejected_twins() {
    let anna = id(1);
    let (d, _checkout) = daemon_home(&anna).await;
    let home = d.home().to_path_buf();
    // Both arrive while Anna still has no policy, so both are held at `consent`.
    let denied = post_question(&d, &anna, "the denied one").await;
    let rejected = post_question(&d, &anna, "the rejected one").await;
    // Reject first: `owl deny` would otherwise finish both held records at once.
    run_ok(&home, &["reject", &rejected]);
    run_ok(&home, &["deny", "Anna"]);
    let log = log_of(&d.spool(), Dir::Done, &denied);
    assert_eq!(kinds_of(&log), ["received", "held", "denied"], "{log:?}");
    assert_eq!(log[2].1.as_deref(), Some("human"));
    let log = log_of(&d.spool(), Dir::Done, &rejected);
    assert_eq!(*kinds_of(&log).last().unwrap(), "rejected", "{log:?}");
    assert_eq!(log.last().unwrap().1.as_deref(), Some("human"));
    d.running.shutdown();
}

/// AC2b: `owl edit` with `$EDITOR` pointed at the fake editor pushes one `edited` event
/// between `drafted` and `sent`; the editor's own log proves it ran.
#[tokio::test]
async fn edit_pushes_one_edited_event_between_drafted_and_sent() {
    let anna = id(1);
    let (d, _checkout) = daemon_home(&anna).await;
    let home = d.home().to_path_buf();
    let qid = post_question(&d, &anna, "why is the token rotated?").await;
    run_ok(&home, &["allow", "Anna", "--once"]);
    run_ok(&home, &["draft", &qid]);

    let editor_log = home.join("fake-editor.log");
    let out = owl_in(&home)
        .args(["edit", &qid])
        .env("EDITOR", FAKE_EDITOR)
        .env("FAKE_EDITOR_LOG", &editor_log)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "owl edit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        std::fs::read_to_string(&editor_log)
            .expect("the fake editor wrote its log")
            .contains("argv:"),
        "the editor really ran"
    );
    let edited = log_of(&d.spool(), Dir::Inbox, &qid);
    assert_eq!(
        kinds_of(&edited),
        ["received", "held", "allowed", "drafted", "edited"],
        "{edited:?}"
    );
    assert_eq!(edited[4].1.as_deref(), Some("human"));
    assert!(
        d.spool()
            .get(Dir::Inbox, &qid)
            .unwrap()
            .unwrap()
            .draft
            .unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("edited by the fake editor"),
        "the edit reached the draft"
    );

    run_ok(&home, &["send", &qid]);
    let sent = log_of(&d.spool(), Dir::Done, &qid);
    assert_eq!(
        kinds_of(&sent),
        ["received", "held", "allowed", "drafted", "edited", "sent"],
        "`edited` sits between `drafted` and `sent`: {sent:?}"
    );
    d.running.shutdown();
}
