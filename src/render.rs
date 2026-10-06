//! Markdown rendering of peer messages for `--format claude|codex|kimi` (§9):
//! the one-column message table `owl show <id> --format claude` prints, the answers
//! table `owl inbox --format claude` prints, and the per-peer colour markers persisted in
//! `$OWLPOST_HOME/markers.json`. A table is the only Markdown Claude Code highlights as a
//! box, so every message from a peer is one — our own drafts stay a plain code block. The
//! CLI renders, the model pastes — no template lives in prose. Free of CLI-only types so the
//! daemon can write the same table into a wake file.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;

use crate::answer::StoredDraft;
use crate::contacts::{ContactBook, key_standing};
use crate::envelope::{self, Body, Payload};
use crate::spool::{Dir, Record, Spool};

/// Per-peer markers, assigned in order of first appearance, then repeating.
pub const MARKERS: [&str; 6] = ["🟦", "🟩", "🟨", "🟪", "🟧", "🟥"];

/// `$OWLPOST_HOME/markers.json`: fingerprint → marker index, append-only.
pub const MARKERS_FILE: &str = "markers.json";

/// Seconds an answer stays in the table.
pub const TABLE_WINDOW_SECS: u64 = 86_400;
/// Rows the table shows at most (the newest).
pub const TABLE_ROWS: usize = 10;

/// The line under the table when rows were cut.
pub fn older_line(n: usize) -> String {
    format!("{n} older answers not shown — owl history")
}

/// The note under a draft whose language differs from the question's.
pub const LANGUAGE_NOTE: &str =
    "note: the draft is in a different language than the question — pick Edit";

/// The separator under the header of the one-column message table.
pub const TABLE_RULE: &str = "|---|";

/// The row that introduces the asker's snippet inside the message table.
pub const CONTEXT_ROW_TEXT: &str = "**context:**";

/// One line of a message as one table row: `| <line> |` with `|` escaped as `\|` and a
/// trailing `\r` (a CRLF text) dropped; an empty line becomes `|  |`. Leading and inner
/// whitespace is kept, and a ```` ``` ```` line is just a row — the table never fences.
pub fn table_line(line: &str) -> String {
    format!("| {} |", line.trim_end_matches('\r').replace('|', "\\|"))
}

/// `text` as body rows, one per `\n`-separated line; trailing line breaks are dropped, so a
/// text ending in a newline gets no extra empty row. Empty text is the single row `|  |`.
pub fn text_rows(text: &str) -> Vec<String> {
    text.trim_end_matches(['\r', '\n'])
        .split('\n')
        .map(table_line)
        .collect()
}

/// The line under the header of a question that continues a thread.
pub fn follow_up_line(context_id: &str) -> String {
    format!("↩ follow-up in thread {}", short_id(context_id))
}

/// The one-column message table: the header row, [`TABLE_RULE`], the `🔑 <standing>` row when
/// `standing` is given (A `consent` record says how its signing key got into the
/// contact book, before anything the peer wrote), the [`follow_up_line`] when `thread` is
/// given, one row per line of `text`, and — when `context` is given — [`CONTEXT_ROW_TEXT`]
/// plus one row per snippet line after the body.
pub fn message_table(
    header: &str,
    text: &str,
    standing: Option<&str>,
    thread: Option<&str>,
    context: Option<&str>,
) -> String {
    let mut rows = vec![table_line(header), TABLE_RULE.to_string()];
    if let Some(standing) = standing {
        rows.push(table_line(&format!("🔑 {standing}")));
    }
    if let Some(cid) = thread {
        rows.push(table_line(&follow_up_line(cid)));
    }
    rows.extend(text_rows(text));
    if let Some(ctx) = context {
        rows.push(table_line(CONTEXT_ROW_TEXT));
        rows.extend(text_rows(ctx));
    }
    rows.join("\n")
}

/// `🦉 #N **<name>** · HH:MM · <project or "-"> · <path or "whole repository">`; `#N` is the
/// record's `owl inbox` row number (absent for a record no longer in the inbox); with a
/// fingerprint (a `consent` record) ` (<fingerprint>)` follows the bold name.
pub fn header(
    n: Option<usize>,
    peer_name: &str,
    fingerprint: Option<&str>,
    hh_mm: &str,
    project: &str,
    path: Option<&str>,
) -> String {
    let n = n.map_or(String::new(), |n| format!("#{n} "));
    let fp = fingerprint.map_or(String::new(), |f| format!(" ({f})"));
    let project = if project.is_empty() { "-" } else { project };
    format!(
        "🦉 {n}**{peer_name}**{fp} · {hh_mm} · {project} · {}",
        path.unwrap_or("whole repository")
    )
}

/// The `#N` of an inbox record: its 1-based position in the id-sorted inbox, the same number
/// `owl inbox` prints; `None` when the record is not in the inbox.
pub fn inbox_n(spool: &Spool, id: &str) -> Option<usize> {
    spool
        .list(Dir::Inbox, |_| true)
        .ok()?
        .iter()
        .position(|(x, _)| x == id)
        .map(|p| p + 1)
}

/// Local wall-clock `HH:MM` of an RFC 3339 timestamp; `--:--` when it does not parse.
pub fn local_hh_mm(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339).map_or_else(
        |_| "--:--".to_string(),
        |t| t.with_timezone(&chrono::Local).format("%H:%M").to_string(),
    )
}

/// `draft:` and the draft in a plain ```` ```text ```` block — a draft is our own text, so
/// it never becomes a table; a table always means "from a peer". The fence is one backtick
/// longer than the longest backtick run inside the text (three at least), so a draft
/// containing three backticks is fenced with four; trailing line breaks are dropped.
pub fn draft_block(text: &str) -> String {
    format!("draft:\n{}", fenced_text(text))
}

/// `text` in a plain ```` ```text ```` block — our own words, so never a table. The fence is
/// one backtick longer than the longest backtick run inside the text (three at least), so a
/// text containing three backticks is fenced with four; trailing line breaks are dropped.
pub fn fenced_text(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    let body = text.trim_end_matches(['\r', '\n']);
    format!("{fence}text\n{body}\n{fence}")
}

// ponytail: Polish/English stopwords only — swap for a real detector when a third language
// shows up.
const POLISH: &[&str] = &[
    "i", "w", "nie", "na", "jest", "się", "to", "że", "jak", "czy", "z", "do", "ale", "tak", "co",
    "o", "od", "po", "dla", "przez", "tego", "jaki", "masz", "u", "siebie",
];
const ENGLISH: &[&str] = &[
    "the", "a", "an", "and", "is", "are", "of", "to", "in", "it", "that", "this", "you", "we",
    "not", "for", "on", "with", "as", "be", "have", "do", "does", "what", "which", "your", "my",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    Polish,
    English,
}

/// The stopword list with more hits; ties and zero hits are unknown.
fn language(text: &str) -> Option<Language> {
    let lower = text.to_lowercase();
    let words = lower.split(|c: char| !c.is_alphanumeric());
    let (mut pl, mut en) = (0usize, 0usize);
    for w in words.filter(|w| !w.is_empty()) {
        if POLISH.contains(&w) {
            pl += 1;
        }
        if ENGLISH.contains(&w) {
            en += 1;
        }
    }
    match pl.cmp(&en) {
        std::cmp::Ordering::Greater => Some(Language::Polish),
        std::cmp::Ordering::Less => Some(Language::English),
        std::cmp::Ordering::Equal => None,
    }
}

/// [`LANGUAGE_NOTE`] when the question and the draft are detected as different languages;
/// nothing when either is unknown or both agree.
pub fn language_note(question: &str, draft: &str) -> Option<&'static str> {
    match (language(question), language(draft)) {
        (Some(q), Some(d)) if q != d => Some(LANGUAGE_NOTE),
        _ => None,
    }
}

/// A table cell: `|` escaped as `\|`, line breaks (`\r\n` or `\n`) as `<br>`; trailing
/// line breaks dropped.
pub fn cell(text: &str) -> String {
    text.trim_end_matches(['\r', '\n'])
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace('\n', "<br>")
}

/// `| <marker> HH:MM · <name> | <text> |`.
pub fn table_row(marker: &str, hh_mm: &str, peer_name: &str, text: &str) -> String {
    format!("| {marker} {hh_mm} · {peer_name} | {} |", cell(text))
}

/// Fingerprint → marker index, loaded from and appended to `$OWLPOST_HOME/markers.json`, so
/// the same peer keeps the same marker across calls and sessions.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Markers {
    map: BTreeMap<String, usize>,
    /// Appearance order, so the index a peer gets is the number of peers seen before it.
    order: Vec<String>,
    dirty: bool,
}

impl Markers {
    /// A missing or unparseable file is an empty map.
    pub fn load(home: &Path) -> Markers {
        let map: BTreeMap<String, usize> = std::fs::read(home.join(MARKERS_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut order: Vec<(usize, String)> = map.iter().map(|(k, v)| (*v, k.clone())).collect();
        order.sort();
        Markers {
            order: order.into_iter().map(|(_, k)| k).collect(),
            map,
            dirty: false,
        }
    }

    /// The marker of `fingerprint`; a first-time peer gets the next one (`count % 6`).
    pub fn marker_for(&mut self, fingerprint: &str) -> &'static str {
        let idx = match self.map.get(fingerprint) {
            Some(i) => *i,
            None => {
                let i = self.order.len();
                self.map.insert(fingerprint.to_string(), i);
                self.order.push(fingerprint.to_string());
                self.dirty = true;
                i
            }
        };
        MARKERS[idx % MARKERS.len()]
    }

    /// Writes the map atomically (`markers.json.tmp` + rename) when a marker was assigned.
    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let path = home.join(MARKERS_FILE);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.map)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("renaming to {}", path.display()));
        }
        Ok(())
    }
}

/// One received answer, as the table needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnswerRow {
    pub received_at: String,
    pub fingerprint: String,
    pub peer_name: String,
    pub answer: String,
    /// The question's id (`in_reply_to`), when known.
    pub question_id: Option<String>,
    /// The first line of that question, or empty when the question is not in the spool.
    pub question_first_line: String,
}

/// The last 8 characters of an id (the random tail of a UUIDv7; its head is the timestamp).
pub fn short_id(id: &str) -> String {
    let n = id.chars().count();
    id.chars().skip(n.saturating_sub(8)).collect()
}

/// The answers table: rows received within the last [`TABLE_WINDOW_SECS`], newest last, the
/// [`TABLE_ROWS`] newest of them; above it one `<marker> ↳ <question id short> "<first
/// line, ≤60 chars>"` line per shown row; under it [`older_line`] when any row was cut
/// (older than the window or beyond the row cap). Markers are assigned in order of first
/// appearance in the shown rows. Empty string for no rows.
pub fn answers_table(rows: &[AnswerRow], now_unix: u64, markers: &mut Markers) -> String {
    let mut in_window: Vec<(u64, &AnswerRow)> = rows
        .iter()
        .filter_map(|r| {
            let t = envelope::parse_rfc3339_to_unix(&r.received_at)?;
            (now_unix.saturating_sub(t) <= TABLE_WINDOW_SECS).then_some((t, r))
        })
        .collect();
    in_window.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.received_at.cmp(&b.1.received_at))
    });
    let shown: Vec<&AnswerRow> = in_window
        .iter()
        .skip(in_window.len().saturating_sub(TABLE_ROWS))
        .map(|(_, r)| *r)
        .collect();
    if shown.is_empty() {
        return String::new();
    }
    let cut = rows.len() - shown.len();
    let mut refs = Vec::new();
    let mut lines = Vec::new();
    for r in &shown {
        let marker = markers.marker_for(&r.fingerprint);
        let first: String = r
            .question_first_line
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        refs.push(format!(
            "{marker} ↳ {} \"{first}\"",
            r.question_id.as_deref().map_or("-".to_string(), short_id)
        ));
        lines.push(table_row(
            marker,
            &local_hh_mm(&r.received_at),
            &r.peer_name,
            &r.answer,
        ));
    }
    let mut out = refs;
    out.push(lines[0].clone());
    out.push("|---|---|".to_string());
    out.extend(lines.into_iter().skip(1));
    if cut > 0 {
        out.push(older_line(cut));
    }
    out.join("\n")
}

/// Contact name for a fingerprint, or the fingerprint itself for unknown peers.
pub fn peer_name(book: &ContactBook, fp: &str) -> String {
    book.contacts
        .iter()
        .find(|c| c.fingerprint == fp)
        .map_or_else(|| fp.to_string(), |c| c.name.clone())
}

/// [`peer_name`] cut to one line of at most 64 characters (control characters dropped): the
/// form a model-facing notice (the wake, the session-start preview) may carry.
pub fn peer_name_line(book: &ContactBook, fp: &str) -> String {
    peer_name(book, fp)
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect()
}

/// The `--format claude` block for one record — what `owl show <id> --format claude` prints
/// (never the wake file, which carries metadata only — `route::wake_content`): a question as its
/// [`message_table`] (on a `consent` record the fingerprint in the header and the `🔑`
/// key-standing row under the rule), a drafted one followed by [`draft_block`],
/// `harness: <name>` and the [`language_note`]; an answer as the same table, project and
/// path taken from the question it replies to.
pub fn record_block(
    spool: &Spool,
    book: &ContactBook,
    rec: &Record,
    payload: &Payload,
    draft: Option<&StoredDraft>,
) -> String {
    let name = peer_name(book, &payload.from);
    let hh_mm = local_hh_mm(&rec.received_at);
    let fingerprint = (rec.state == "consent").then_some(payload.from.as_str());
    // A consent record says how we know the signing key, in the first body row.
    let standing = fingerprint.map(|fp| key_standing(book, fp).wording(fp));
    let n = inbox_n(spool, &payload.id);
    match &payload.body {
        Body::Question {
            project,
            path,
            question,
            context,
        } => {
            let head = header(n, &name, fingerprint, &hh_mm, project, path.as_deref());
            // The follow-up line only when this thread has an earlier exchange of ours in
            // `done/` (never trusting the payload's word for it).
            let thread = payload
                .context_id
                .as_deref()
                .filter(|cid| crate::answer::thread_has_earlier(spool, cid, &payload.id));
            let mut out = message_table(
                &head,
                question,
                standing.as_deref(),
                thread,
                context.as_deref(),
            );
            if let Some(d) = draft {
                out.push('\n');
                out.push_str(&draft_block(&d.text));
                out.push_str(&format!("\nharness: {}", d.harness));
                if let Some(note) = language_note(question, &d.text) {
                    out.push('\n');
                    out.push_str(note);
                }
            }
            out
        }
        // A tool-call request shows what would run (the tool and its input) and,
        // once drafted, the run's own line over the output in a plain text block; the reply
        // shows the same line over what came back. Never squeezed into the message table.
        Body::ToolCall { .. } | Body::ToolReply { .. } => {
            let text = match &payload.body {
                Body::ToolCall { .. } => {
                    format!("asks to run {}", crate::tools::body_summary(&payload.body))
                }
                Body::ToolReply {
                    output,
                    exit_code,
                    duration_ms,
                    ..
                } => format!(
                    "{}\n{}",
                    crate::tools::Run::show_line(*exit_code, *duration_ms, output.len()),
                    crate::tools::display(output)
                ),
                _ => unreachable!("outer match selected a tool body"),
            };
            let project = match &payload.body {
                Body::ToolCall { project, .. } => project.as_deref().unwrap_or("-"),
                _ => "-",
            };
            let mut out = message_table(
                &header(n, &name, fingerprint, &hh_mm, project, None),
                &text,
                standing.as_deref(),
                None,
                None,
            );
            if let Some(r) = crate::tools::stored(rec) {
                out.push('\n');
                out.push_str(&r.draft_line());
                out.push('\n');
                out.push_str(&fenced_text(&crate::tools::display(&r.output)));
            }
            out
        }
        // A content request and its reply are rendered by `owl show` itself (the
        // request head, the content in a plain text block), never squeezed into the message
        // table, so the table here shows the request's own summary line.
        Body::Content { .. } | Body::ContentReply { .. } => {
            let text = match &payload.body {
                Body::Content {
                    path,
                    memory,
                    git_ref,
                    ..
                } => match (path, memory) {
                    (Some(p), _) => {
                        format!("asks for {p}@{}", git_ref.as_deref().unwrap_or("HEAD"))
                    }
                    (_, Some(k)) => format!("asks for memory:{k}"),
                    _ => "asks for content".to_string(),
                },
                // The asker's side of the same cap (§4): `--format claude` is a mode of
                // `owl show`, so it cuts at `CONTENT_SHOW_LINES` like the plain form.
                Body::ContentReply {
                    content, sha256, ..
                } => crate::content::display(content, sha256),
                _ => unreachable!("outer match selected a content body"),
            };
            let (project, path) = match &payload.body {
                Body::Content { project, path, .. } => {
                    (project.as_deref().unwrap_or("-"), path.as_deref())
                }
                _ => ("-", None),
            };
            let mut out = message_table(
                &header(n, &name, fingerprint, &hh_mm, project, path),
                &text,
                standing.as_deref(),
                None,
                None,
            );
            // Our own content, once drafted: the exact bytes (to the display cap) in a plain
            // text block, never squeezed into the table.
            if let Some(c) = crate::content::stored(rec) {
                out.push('\n');
                out.push_str("content:\n");
                out.push_str(&fenced_text(&crate::content::display(&c.text, &c.sha256)));
                out.push_str(&format!("\nsha256: {}", c.sha256));
            }
            out
        }
        Body::Answer { answer, .. } => {
            let question = payload
                .in_reply_to
                .as_deref()
                .and_then(|q| find_question(spool, q));
            let (project, path) = match question.as_ref().map(|q| &q.body) {
                Some(Body::Question { project, path, .. }) => (project.as_str(), path.as_deref()),
                _ => ("-", None),
            };
            message_table(
                &header(n, &name, fingerprint, &hh_mm, project, path),
                answer,
                None,
                None,
                None,
            )
        }
    }
}

/// The question an answer replies to: `done/` (an answered ask), `asks/` (still open) or
/// `inbox/` (a question received here), whichever holds `id`.
pub fn find_question(spool: &Spool, id: &str) -> Option<Payload> {
    [Dir::Done, Dir::Asks, Dir::Inbox]
        .into_iter()
        .find_map(|d| spool.get(d, id).ok().flatten())
        .and_then(|rec| serde_json::from_str(&rec.raw).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(received_at: &str, fp: &str, name: &str, answer: &str, q: &str) -> AnswerRow {
        AnswerRow {
            received_at: received_at.into(),
            fingerprint: fp.into(),
            peer_name: name.into(),
            answer: answer.into(),
            question_id: Some(format!("0192aaaa-bbbb-7ccc-8ddd-{q:0>12}")),
            question_first_line: format!("question {q}\nsecond line"),
        }
    }

    /// The header row, the rule and one row per message line — no frame and no
    /// fence anywhere.
    #[test]
    fn message_table_is_header_rule_and_one_row_per_line() {
        let h = header(Some(3), "Ana", None, "09:08", "github.com/x/y", None);
        assert_eq!(
            h,
            "🦉 #3 **Ana** · 09:08 · github.com/x/y · whole repository"
        );
        assert_eq!(
            message_table(&h, "Jaki masz ostatni commit?", None, None, None),
            format!("| {h} |\n|---|\n| Jaki masz ostatni commit? |")
        );
        assert_eq!(TABLE_RULE, "|---|");
        assert_eq!(
            header(None, "Ana", Some("owl:abc"), "09:08", "p", Some("src/a.rs")),
            "🦉 **Ana** (owl:abc) · 09:08 · p · src/a.rs"
        );
        assert_eq!(
            header(None, "Ana", None, "09:08", "", None),
            "🦉 **Ana** · 09:08 · - · whole repository"
        );
        let t = message_table(&h, "a\n\nb", None, None, None);
        assert_eq!(t, format!("| {h} |\n|---|\n| a |\n|  |\n| b |"));
        assert!(!t.contains('🟧') && !t.contains("```"));
    }

    /// Escaping and line splitting — a pipe, a ``` line, a whitespace-only
    /// line, a trailing newline, CRLF and multi-byte text.
    #[test]
    fn table_rows_escape_pipes_and_keep_every_line() {
        assert_eq!(table_line("a|b"), "| a\\|b |");
        assert_eq!(table_line("a|b|c"), "| a\\|b\\|c |");
        assert_eq!(table_line(""), "|  |");
        assert_eq!(table_line("   "), "|     |", "whitespace is kept");
        assert_eq!(table_line("```"), "| ``` |", "a fence line is just a row");
        assert_eq!(table_line("x\r"), "| x |", "a CRLF line drops the \\r");
        assert_eq!(text_rows("one\ntwo"), ["| one |", "| two |"]);
        assert_eq!(text_rows("one\r\ntwo"), ["| one |", "| two |"]);
        assert_eq!(text_rows("one\n"), ["| one |"], "no extra empty row");
        assert_eq!(text_rows("one\n\n\n"), ["| one |"]);
        assert_eq!(text_rows(""), ["|  |"]);
        assert_eq!(text_rows("\n\nmid\n"), ["|  |", "|  |", "| mid |"]);
        assert_eq!(text_rows("Zażółć\ngęślą"), ["| Zażółć |", "| gęślą |"]);
    }

    /// The follow-up line is the first body row, the context rows come after
    /// the body; neither appears when not given.
    #[test]
    fn follow_up_row_is_first_and_context_rows_are_last() {
        let h = "🦉 **Ana** · 09:08 · p · src/a.rs";
        let cid = "0192aaaa-bbbb-7ccc-8ddd-000000000042";
        assert_eq!(follow_up_line(cid), "↩ follow-up in thread 00000042");
        assert_eq!(CONTEXT_ROW_TEXT, "**context:**");
        assert_eq!(
            message_table(h, "q1\nq2", None, Some(cid), Some("ctx|1\nctx2")),
            format!(
                "| {h} |\n|---|\n| ↩ follow-up in thread 00000042 |\n| q1 |\n| q2 |\n\
                 | **context:** |\n| ctx\\|1 |\n| ctx2 |"
            )
        );
        let bare = message_table(h, "q1", None, None, None);
        assert!(
            !bare.contains("follow-up") && !bare.contains("context"),
            "{bare}"
        );
        assert_eq!(bare.lines().count(), 3);
    }

    /// The `🔑` standing row sits directly after the rule, before every other
    /// body row (the follow-up line included) — and only when a standing is given.
    #[test]
    fn the_standing_row_is_the_first_body_row() {
        let h = "🦉 **Ana** (owl:abc) · 09:08 · p · src/a.rs";
        let cid = "0192aaaa-bbbb-7ccc-8ddd-000000000042";
        let standing =
            "known key: contact \"Ana\" — repo peer file (.agents/peers/, reviewed in a PR)";
        assert_eq!(
            message_table(h, "q1", Some(standing), Some(cid), Some("ctx")),
            format!(
                "| {h} |\n|---|\n| 🔑 {standing} |\n| ↩ follow-up in thread 00000042 |\n\
                 | q1 |\n| **context:** |\n| ctx |"
            )
        );
        let t = message_table(h, "q1", Some(standing), None, None);
        assert_eq!(t.lines().nth(2).unwrap(), format!("| 🔑 {standing} |"));
        assert_eq!(t.matches('🔑').count(), 1, "{t}");
        assert!(
            !message_table(h, "q1", None, None, None).contains('🔑'),
            "no standing, no row"
        );
    }

    /// A draft stays a plain code block whose fence grows past backticks in it.
    #[test]
    fn draft_block_stays_a_fenced_code_block() {
        assert_eq!(draft_block("plain"), "draft:\n```text\nplain\n```");
        assert_eq!(
            draft_block("has ``` inside"),
            "draft:\n````text\nhas ``` inside\n````"
        );
        assert_eq!(
            draft_block("has ```` inside"),
            "draft:\n`````text\nhas ```` inside\n`````"
        );
        assert_eq!(
            draft_block("one ` tick"),
            "draft:\n```text\none ` tick\n```"
        );
        assert_eq!(
            draft_block("trailing\n\n"),
            "draft:\n```text\ntrailing\n```"
        );
        assert!(!draft_block("d").contains('|'), "a draft is never a table");
    }

    #[test]
    fn hh_mm_handles_garbage() {
        assert_eq!(local_hh_mm("garbage"), "--:--");
        let t = local_hh_mm("2026-09-06T23:08:11Z");
        assert_eq!(t.len(), 5, "{t}");
        assert_eq!(&t[2..3], ":");
    }

    #[test]
    fn language_note_only_when_both_known_and_different() {
        let pl = "Jaki masz ostatni commit u siebie i czy to jest ok?";
        let en = "The last commit is on main and it is fine, what do you need?";
        let pl2 = "Nie, to nie jest tak jak myślisz, ale się zgadzam.";
        let en2 = "This is the file you are looking for.";
        assert_eq!(language_note(pl, en), Some(LANGUAGE_NOTE));
        assert_eq!(language_note(en, pl), Some(LANGUAGE_NOTE));
        assert_eq!(language_note(pl, pl2), None);
        assert_eq!(language_note(en, en2), None);
        assert_eq!(language_note("12345 ???", en), None, "unknown question");
        assert_eq!(language_note(pl, "src/main.rs:42"), None, "unknown draft");
        assert_eq!(language_note("", ""), None);
        // `to`, `do`, `o` are in both lists: a tie is unknown.
        assert_eq!(language("to do"), None);
    }

    #[test]
    fn cell_escapes_pipes_and_breaks() {
        assert_eq!(cell("a|b"), "a\\|b");
        assert_eq!(cell("l1\nl2\r\nl3\n"), "l1<br>l2<br>l3");
        assert_eq!(
            table_row("🟦", "00:16", "Ana", "x|y\nz"),
            "| 🟦 00:16 · Ana | x\\|y<br>z |"
        );
    }

    #[test]
    fn markers_follow_first_appearance_and_persist() {
        let home = tempfile::tempdir().unwrap();
        let mut m = Markers::load(home.path());
        assert_eq!(m.marker_for("owl:b"), "🟦");
        assert_eq!(m.marker_for("owl:a"), "🟩");
        assert_eq!(m.marker_for("owl:b"), "🟦");
        for (i, fp) in ["c", "d", "e", "f", "g"].iter().enumerate() {
            assert_eq!(m.marker_for(fp), MARKERS[(i + 2) % 6]);
        }
        assert_eq!(m.marker_for("g"), "🟦", "wraps after six");
        m.save(home.path()).unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.path().join(MARKERS_FILE)).unwrap())
                .unwrap();
        assert_eq!(v["owl:b"], 0);
        assert_eq!(v["owl:a"], 1);
        assert_eq!(v["g"], 6);
        let mut again = Markers::load(home.path());
        assert_eq!(again.marker_for("owl:a"), "🟩");
        assert_eq!(again.marker_for("owl:b"), "🟦");
        assert_eq!(again.marker_for("new"), MARKERS[7 % 6]);
        // A missing or broken file is an empty map; nothing is written until a marker is new.
        let empty = tempfile::tempdir().unwrap();
        std::fs::write(empty.path().join(MARKERS_FILE), "{").unwrap();
        let m = Markers::load(empty.path());
        assert_eq!(m, Markers::default());
        m.save(empty.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(empty.path().join(MARKERS_FILE)).unwrap(),
            "{"
        );
    }

    #[test]
    fn table_keeps_24h_window_and_10_newest_rows_newest_last() {
        let now = envelope::parse_rfc3339_to_unix("2026-09-07T12:00:00Z").unwrap();
        // 12 answers over 26 h, one per 2 h, alternating peers: 11 within 24 h, 1 older.
        let rows: Vec<AnswerRow> = (0..12)
            .map(|i| {
                let t = envelope::unix_to_rfc3339(now - 26 * 3600 + i * 2 * 3600 + 1);
                let (fp, name) = if i % 2 == 0 {
                    ("owl:one", "One")
                } else {
                    ("owl:two", "Two")
                };
                row(&t, fp, name, &format!("answer {i}"), &i.to_string())
            })
            .collect();
        // Handed over out of order: the table sorts by `received_at`, not by input order.
        let mut shuffled = rows.clone();
        shuffled.rotate_left(5);
        shuffled.swap(0, 3);
        assert_ne!(shuffled, rows);
        let mut m = Markers::default();
        let out = answers_table(&shuffled, now, &mut m);
        let lines: Vec<&str> = out.lines().collect();
        let refs: Vec<&&str> = lines.iter().filter(|l| l.contains(" ↳ ")).collect();
        let table: Vec<&&str> = lines.iter().filter(|l| l.starts_with("| ")).collect();
        assert_eq!(refs.len(), 10);
        assert_eq!(table.len(), 10);
        assert_eq!(lines[10 + 1], "|---|---|");
        assert_eq!(lines.last().unwrap(), &older_line(2));
        // Rows 0 (older than 24 h) and 1 (oldest in window) are cut; 2..12 shown, ascending.
        assert!(table[0].ends_with("| answer 2 |"), "{}", table[0]);
        assert!(table[9].ends_with("| answer 11 |"), "{}", table[9]);
        assert!(!out.contains("answer 0 ") && !out.contains("answer 1 "));
        // Row 2 is from peer one: the first-appearing peer gets 🟦, the other 🟩.
        assert!(table[0].starts_with("| 🟦 "), "{}", table[0]);
        assert!(table[1].starts_with("| 🟩 "), "{}", table[1]);
        assert!(
            refs[0].starts_with("🟦 ↳ 00000002 \"question 2\""),
            "{}",
            refs[0]
        );
        // Exactly at the window edge stays; one second past it goes.
        let edge = row(
            &envelope::unix_to_rfc3339(now - 86_400),
            "x",
            "X",
            "edge",
            "e",
        );
        let past = row(
            &envelope::unix_to_rfc3339(now - 86_401),
            "x",
            "X",
            "past",
            "p",
        );
        let out = answers_table(&[past, edge], now, &mut Markers::default());
        assert!(
            out.contains("| edge |") && !out.contains("| past |"),
            "{out}"
        );
        assert!(out.ends_with(&older_line(1)), "{out}");
        assert_eq!(answers_table(&[], now, &mut Markers::default()), "");
    }

    #[test]
    fn table_names_missing_questions_with_an_empty_string() {
        let now = envelope::parse_rfc3339_to_unix("2026-09-07T12:00:00Z").unwrap();
        let mut r = row(&envelope::unix_to_rfc3339(now - 5), "a", "A", "x", "1");
        r.question_first_line = String::new();
        let out = answers_table(&[r.clone()], now, &mut Markers::default());
        assert!(out.starts_with("🟦 ↳ 00000001 \"\"\n| 🟦 "), "{out}");
        r.question_id = None;
        let out = answers_table(&[r], now, &mut Markers::default());
        assert!(out.starts_with("🟦 ↳ - \"\"\n"), "{out}");
        assert_eq!(short_id("0192aaaa-bbbb-7ccc-8ddd-000000000001"), "00000001");
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn reference_line_keeps_the_first_60_chars_of_the_first_line() {
        let now = envelope::parse_rfc3339_to_unix("2026-09-07T12:00:00Z").unwrap();
        let seventy = "x".repeat(70);
        let sixty = "y".repeat(60);
        let mut long = row(&envelope::unix_to_rfc3339(now - 5), "a", "A", "x", "1");
        long.question_first_line = format!("{seventy}\nsecond line");
        let mut exact = row(&envelope::unix_to_rfc3339(now - 4), "a", "A", "y", "2");
        exact.question_first_line = sixty.clone();
        // 60 chars, not bytes: a 70 × `ł` line (140 bytes) keeps 60 chars (120 bytes), a
        // 60 × `ł` line stays whole.
        let mut polish = row(&envelope::unix_to_rfc3339(now - 3), "a", "A", "z", "3");
        polish.question_first_line = format!("{}\nsecond line", "ł".repeat(70));
        let mut polish_exact = row(&envelope::unix_to_rfc3339(now - 2), "a", "A", "w", "4");
        polish_exact.question_first_line = "ł".repeat(60);
        let out = answers_table(
            &[long, exact, polish, polish_exact],
            now,
            &mut Markers::default(),
        );
        let refs: Vec<&str> = out.lines().filter(|l| l.contains(" ↳ ")).collect();
        assert_eq!(refs[0], format!("🟦 ↳ 00000001 \"{}\"", "x".repeat(60)));
        assert_eq!(refs[1], format!("🟦 ↳ 00000002 \"{sixty}\""));
        assert_eq!(refs[2], format!("🟦 ↳ 00000003 \"{}\"", "ł".repeat(60)));
        assert_eq!(refs[2].len(), "🟦 ↳ 00000003 \"\"".len() + 120);
        assert_eq!(refs[3], format!("🟦 ↳ 00000004 \"{}\"", "ł".repeat(60)));
        assert!(!out.contains(&"x".repeat(61)) && !out.contains(&"ł".repeat(61)));
    }
}
