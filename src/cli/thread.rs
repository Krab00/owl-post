//! `owl thread` (§9): the conversation with one person, in both directions and in
//! one chronological order.
//!
//! * `owl thread [--json] [--archived]` — one row per peer ever seen in `inbox/`, `outbox/`,
//!   `asks/` or `done/`, newest conversation first. Chats hidden by `owl archive`
//!   are left out; `--archived` lists only them.
//! * `owl thread <peer> [--json] [--since <age|date>] [--context <context_id>] [--archived]` —
//!   every event of every record of that peer, oldest first, with the **full** text of each
//!   message. Records of an archived thread are left out unless `--archived` (then only they
//!   show); an explicit `--context` always shows its thread, and a named peer's timeline shows
//!   in full even when the chat itself is archived.
//!
//! Read-only by construction: no socket, no harness, no `mark_seen`, no routing release and
//! no migration of a legacy event log. The only thing it may create is the spool's own
//! directory tree (`Spool::new`), exactly as `owl history` does; `archive.json` is only read.

use std::collections::BTreeMap;
use std::path::Path;

use owlpost::contacts::ContactBook;
use owlpost::envelope::{self, Body, Payload};
use owlpost::events::{self, Event};
use owlpost::identity::{self, Identity};
use owlpost::render;
use owlpost::spool::{Dir, Record, Spool};
use serde_json::{Value, json};

use super::{
    ExitError, SUMMARY_CHARS, body_path, first_line, kind_str, payload_of, peer_name, print_json,
    print_table, user_error,
};

/// The four directories a conversation can live in; `cache/` holds copies, never a thread.
pub(super) const DIRS: [Dir; 4] = [Dir::Inbox, Dir::Outbox, Dir::Asks, Dir::Done];

/// Inbox states that still want a decision from the owner — what the `OPEN` column counts.
const OPEN_STATES: [&str; 3] = ["consent", "pending", "drafted"];

pub struct Filters {
    pub since: Option<String>,
    pub context: Option<String>,
    /// Only archived chats (list) or archived threads (timeline), instead of only
    /// unarchived ones.
    pub archived: bool,
}

pub fn run(home: &Path, peer: Option<&str>, filters: Filters, json: bool) -> anyhow::Result<()> {
    match peer {
        None => list(home, filters.archived, json),
        Some(p) => timeline(home, p, filters, json),
    }
}

/// Every parseable record of the spool with the directory it sits in and the fingerprint of
/// the other party. Corrupt files are skipped, never fatal.
pub(super) fn records(
    spool: &Spool,
    own: Option<&str>,
) -> Vec<(Dir, String, Record, Payload, String)> {
    let mut out = Vec::new();
    for dir in DIRS {
        for (id, rec) in spool.list_lenient(dir).unwrap_or_default() {
            let Ok(payload) = payload_of(&id, &rec) else {
                continue;
            };
            let peer = if own == Some(payload.from.as_str()) {
                payload.to.clone()
            } else {
                payload.from.clone()
            };
            out.push((dir, id, rec, payload, peer));
        }
    }
    out
}

pub(super) fn own_fingerprint(home: &Path) -> Option<String> {
    Identity::load(home)
        .ok()
        .map(|id| identity::fingerprint(&id.verifying_key()))
}

// ------------------------------------------------------------------ the peer list

struct Row {
    from: String,
    from_name: String,
    last_ts: String,
    /// Id of the record `last_ts` came from: the tiebreak that picks `last_summary`.
    last_id: String,
    last_summary: String,
    unseen: u64,
    open: u64,
}

fn list(home: &Path, archived: bool, json: bool) -> anyhow::Result<()> {
    let spool = Spool::new(home)?;
    let book = super::contact_book(home)?;
    let marks = super::archive::load(home)?;
    let own = own_fingerprint(home);
    let mut by_peer: BTreeMap<String, Row> = BTreeMap::new();
    for (dir, id, rec, payload, peer) in records(&spool, own.as_deref()) {
        let text = message_text(&payload);
        let row = by_peer.entry(peer.clone()).or_insert_with(|| Row {
            from_name: peer_name(&book, &peer),
            from: peer,
            last_ts: String::new(),
            last_id: String::new(),
            last_summary: String::new(),
            unseen: 0,
            open: 0,
        });
        if dir == Dir::Inbox {
            if !rec.seen {
                row.unseen += 1;
            }
            // A held content request is open work too.
            if OPEN_STATES.contains(&rec.state.as_str()) && payload.kind.is_request() {
                row.open += 1;
            }
        }
        // Newest record of this peer: `received_at`, the (time-ordered UUIDv7) id as tiebreak.
        if (rec.received_at.as_str(), id.as_str()) > (row.last_ts.as_str(), row.last_id.as_str()) {
            row.last_ts = rec.received_at.clone();
            row.last_id = id;
            row.last_summary = first_line(&text, SUMMARY_CHARS);
        }
    }
    let mut rows: Vec<Row> = by_peer.into_values().collect();
    // Newest conversation first; the contact name breaks a tie.
    rows.sort_by(|a, b| {
        b.last_ts
            .cmp(&a.last_ts)
            .then_with(|| a.from_name.cmp(&b.from_name))
    });
    // Archived chats are left out by default; `--archived` lists only them.
    rows.retain(|r| marks.chats.contains(&r.from) == archived);
    if rows.is_empty() {
        return Err(ExitError::error(4, "no threads"));
    }
    if json {
        print_json(&Value::Array(
            rows.iter()
                .map(|r| {
                    json!({
                        "from": r.from,
                        "from_name": r.from_name,
                        "last_ts": r.last_ts,
                        "unseen": r.unseen,
                        "open": r.open,
                        "last_summary": r.last_summary,
                    })
                })
                .collect(),
        ))?;
    } else {
        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|r| {
                vec![
                    r.from_name.clone(),
                    r.unseen.to_string(),
                    r.open.to_string(),
                    r.last_ts.clone(),
                    r.last_summary.clone(),
                ]
            })
            .collect();
        print_table(&["PEER", "UNSEEN", "OPEN", "LAST", "SUMMARY"], &cells);
    }
    Ok(())
}

// ------------------------------------------------------------------ one conversation

/// The question or answer text of a payload (A content request's summary line, a
/// content reply's content).
fn message_text(payload: &Payload) -> std::borrow::Cow<'_, str> {
    super::body_text(&payload.body)
}

/// One timeline row before rendering: the event plus everything the row needs from its record.
struct Entry {
    ts: String,
    record_id: String,
    idx: usize,
    event: Event,
    dir: &'static str,
    rec: Record,
    payload: Payload,
}

fn timeline(home: &Path, peer: &str, filters: Filters, json: bool) -> anyhow::Result<()> {
    let spool = Spool::new(home)?;
    let book = super::contact_book(home)?;
    let marks = super::archive::load(home)?;
    let contact = book.resolve(peer).map_err(|e| user_error(e.to_string()))?;
    let (fingerprint, name) = (contact.fingerprint.clone(), contact.name.clone());
    let own = own_fingerprint(home);
    let now = envelope::now_unix();
    let since = filters
        .since
        .as_deref()
        .map(|s| super::history::parse_since(s, now))
        .transpose()?;

    let mut entries: Vec<Entry> = Vec::new();
    for (dir, id, rec, payload, peer_fp) in records(&spool, own.as_deref()) {
        if peer_fp != fingerprint {
            continue;
        }
        if let Some(want) = &filters.context
            && payload.context_id.as_deref() != Some(want.as_str())
        {
            continue;
        }
        // An explicitly named thread is always shown; otherwise the archive mark
        // decides, and `--archived` flips the filter to only-archived.
        if filters.context.is_none() {
            let is_archived = payload
                .context_id
                .as_ref()
                .is_some_and(|c| marks.threads.contains(&(fingerprint.clone(), c.clone())));
            if is_archived != filters.archived {
                continue;
            }
        }
        // `in` when the peer sent this record, `out` when we produced it.
        let direction = if payload.from == fingerprint {
            "in"
        } else {
            "out"
        };
        for (idx, event) in events::of(dir, &rec).into_iter().enumerate() {
            if let Some(s) = since
                && !envelope::parse_rfc3339_to_unix(&event.ts).is_some_and(|t| t >= s)
            {
                continue;
            }
            entries.push(Entry {
                ts: event.ts.clone(),
                record_id: id.clone(),
                idx,
                event,
                dir: direction,
                rec: rec.clone(),
                payload: payload.clone(),
            });
        }
    }
    // Oldest first; two events stamped in the same second keep their real order through the
    // record id and then the event's index inside its record.
    entries.sort_by(|a, b| (&a.ts, &a.record_id, a.idx).cmp(&(&b.ts, &b.record_id, b.idx)));

    if json {
        print_json(&Value::Array(entries.iter().map(row).collect()))?;
        return Ok(());
    }
    let last = entries
        .last()
        .map_or_else(|| "--:--".to_string(), |e| render::local_hh_mm(&e.ts));
    println!(
        "{fingerprint} — {name} · {} event{} · last {last}",
        entries.len(),
        if entries.len() == 1 { "" } else { "s" }
    );
    for e in &entries {
        println!();
        println!("{}", block(&spool, &book, e));
    }
    Ok(())
}

/// One `--json` row. `context`, `harness`, `by` and `detail` are omitted when absent; `kind`
/// is passed through as the record stored it, whatever this version makes of it.
fn row(e: &Entry) -> Value {
    let mut v = json!({
        "ts": e.ts,
        "kind": e.event.kind,
        "dir": e.dir,
        "record_id": e.record_id,
        "context_id": e.payload.context_id,
        "type": kind_str(e.payload.kind),
        "state": e.rec.state,
        "project": super::body_project(&e.payload.body),
        "path": body_path(&e.payload.body),
        // The full text, never cut: the reader (or the mod) decides what to truncate.
        "text": message_text(&e.payload),
    });
    let m = v.as_object_mut().expect("row is an object");
    if let Body::Question {
        context: Some(ctx), ..
    } = &e.payload.body
    {
        m.insert("context".into(), json!(ctx));
    }
    if let Some(h) = harness_of(e) {
        m.insert("harness".into(), json!(h));
    }
    if let Some(by) = &e.event.by {
        m.insert("by".into(), json!(by));
    }
    if let Some(d) = &e.event.detail {
        m.insert("detail".into(), d.clone());
    }
    v
}

/// The harness behind a row: an answer payload names its own, and on a question record only
/// the events that are about our reply (`drafted`, `sent`) carry the stored draft's — a
/// `received` row is the peer's message and has no harness.
fn harness_of(e: &Entry) -> Option<String> {
    if let Body::Answer { harness, .. }
    | Body::ContentReply { harness, .. }
    | Body::ToolReply { harness, .. } = &e.payload.body
    {
        return Some(harness.clone());
    }
    if !matches!(e.event.kind.as_str(), "drafted" | "sent") {
        return None;
    }
    owlpost::answer::StoredDraft::from_record(&e.record_id, &e.rec)
        .ok()
        .flatten()
        .map(|d| d.harness)
}

/// Kinds whose block carries our own words: the answer we drafted or sent, the question we
/// asked, and the content we served and the output the tool printed.
/// Each of these rows is the only place the human format shows what left this machine.
const OUR_TEXT: [&str; 7] = [
    "drafted",
    "sent",
    "asked",
    "content-drafted",
    "content-sent",
    "tool-run",
    "tool-sent",
];

/// One human-format block: a peer message as the `--format claude` table of its record,
/// everything else as one line plus, when the event carries our own text, that text in a
/// plain ```` ```text ```` block.
fn block(spool: &Spool, book: &ContactBook, e: &Entry) -> String {
    // A content request and a content reply are peer messages too.
    if e.dir == "in"
        && matches!(
            e.event.kind.as_str(),
            "received"
                | "answer-received"
                | "content-requested"
                | "content-received"
                // A tool-call request and its reply are peer messages too.
                | "tool-requested"
                | "tool-received"
        )
    {
        // Byte-for-byte what `owl show <id> --format claude` prints for this record.
        return render::record_block(spool, book, &e.rec, &e.payload, None);
    }
    let mut line = format!("{} {}", render::local_hh_mm(&e.ts), e.event.kind);
    if let Some(by) = &e.event.by {
        line.push_str(&format!(" · by {by}"));
    }
    if let Some(d) = detail_line(e.event.detail.as_ref()) {
        line.push_str(&format!(" · {d}"));
    }
    if OUR_TEXT.contains(&e.event.kind.as_str())
        && let Some(text) = our_text(e)
    {
        line.push('\n');
        line.push_str(&render::fenced_text(&text));
    }
    line
}

/// `k=v, k=v` for an object detail; the compact JSON for anything else.
fn detail_line(detail: Option<&Value>) -> Option<String> {
    match detail? {
        Value::Object(m) if m.is_empty() => None,
        Value::Object(m) => Some(
            m.iter()
                .map(|(k, v)| match v {
                    Value::String(s) => format!("{k}={s}"),
                    other => format!("{k}={other}"),
                })
                .collect::<Vec<_>>()
                .join(", "),
        ),
        other => Some(other.to_string()),
    }
}

/// Our own words behind a `drafted`/`sent`/`asked` event: the stored draft when there is one
/// (what we wrote in reply), else the record's own message (our ask).
fn our_text(e: &Entry) -> Option<String> {
    // A content draft and a tool draft can each be 256 KiB; the timeline shows them under
    // the same 200-line cap `owl show` uses, never the whole buffer.
    if let Some(c) = owlpost::content::stored(&e.rec) {
        return Some(owlpost::content::display(&c.text, &c.sha256));
    }
    if let Some(r) = owlpost::tools::stored(&e.rec) {
        return Some(owlpost::tools::display(&r.output));
    }
    if let Ok(Some(d)) = owlpost::answer::StoredDraft::from_record(&e.record_id, &e.rec) {
        return Some(d.text);
    }
    Some(message_text(&e.payload).to_string())
}
