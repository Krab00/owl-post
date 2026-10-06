//! `owl inbox [--count] [--new] [--all] [--format plain|claude|codex|kimi]` (§8, §9, §3.3).
//!
//! Listing: every inbox record by default (§3.3 "lists everything and marks it seen"), only
//! unseen ones with `--new`; `--all` is accepted for symmetry and lists everything too. Every
//! listed record is marked `seen`. Counting: `--count` prints the unseen count (`--all`: the
//! total) and never marks anything; with `--format` it prints the harness injection line
//! instead, or nothing at all when the count is 0.
//!
//! `--count --format claude` (the plugin hook) is event-driven and per session
//! (`owlpost::route`). Every Claude event reads the hook input JSON on stdin once;
//! its `session_id` (`[A-Za-z0-9._-]{1,128}`) names the session. On `--hook-event
//! SessionStart` the hook creates `sessions/<sid>/{wake,tmp}`, writes `marker.json` (the
//! canonicalised `cwd`, `source`, both timestamps), sweeps dead session directories, assigns
//! the unseen backlog to this session (routing only, no wake file) and prints the line — also
//! at count 0 — whose `hookSpecificOutput` carries `watchPaths:
//! ["<home>/sessions/<sid>/wake"]`, unless `$OWLPOST_HOME/plugin.json` says `{"watch":
//! false}` (then the key is absent and count 0 prints nothing; the marker is still written).
//! Without a usable `session_id` no marker is written and no `watchPaths` goes out.
//! `additionalContext` is present only when there is text: the counter sentence, on
//! `SessionStart` one preview line per unseen record (`- <name> <kind> [<state>] · <id>`,
//! metadata only) and the `OPEN_SENTENCE`, which offers the inbox instead of opening it
//! reporting a counter; the preview never marks anything seen. `SessionStart` also sweeps
//! `$OWLPOST_HOME/watch/` (the `--follow` markers below); no sweep ever fails the hook.
//! `UserPromptSubmit` and `PostToolUse` print what they always did and touch the session's
//! heartbeat. `SessionEnd` removes `sessions/<sid>/`, clears `current` in every routing
//! naming the session, prints nothing and exits 0.
//!
//! `--count --format claude --hook-event FileChanged` is the wake: when `event` is `add`,
//! `file_path` is a file directly under `<home>/sessions/<sid>/wake/` and the watch is
//! enabled, it prints that file's content byte for byte on stderr and exits 2 — the one exit
//! code Claude Code's `asyncRewake` hook turns into a new model turn; every other case
//! (`change`, `unlink`, another path, a missing file, watch off, a terminal, empty or
//! unparseable stdin) exits 0 silently. The hook never composes text: the daemon (or `owl
//! route`) wrote the file for exactly this session. Nothing is ever marked seen.
//!
//! `--count --follow [--session <id>]` is a poll loop any harness can run (plain format
//! only): with `--session` it writes its pid to the marker, then polls the count every 5 s
//! (`OWLPOST_FOLLOW_SECS`), prints the counter sentence only when it changed, nothing at zero,
//! never marks anything seen, and ends (removing the marker) when the marker is removed from
//! outside or its stdout is closed.
//!
//! Listing with `--format claude|codex|kimi` prints Markdown for the model to paste
//! verbatim: every question as its message table (`consent` ones first, then list order),
//! then the answers table (`owlpost::render::answers_table`: last 24 h, the 10 newest,
//! newest last, per-peer markers persisted in `$OWLPOST_HOME/markers.json`), one blank line
//! between sections and nothing else; the consent prompts and `auto_error` notes of the plain
//! listing are not printed. The listed records are marked seen the same way, and every
//! listed record is released from the session wake routing.
//!
//! Consent records get a prompt line under the table (`<name> wants to ask your agent about
//! <project> — owl allow <fp> [--once|--always] / owl deny <fp>`); records the auto-accept
//! scheduler failed on show their `auto_error` (§3.4) with the command that retries them:
//! `owl send <id>` when the signed answer already sits in `outbox/` (`owl draft` refuses
//! then), `owl draft <id>` otherwise.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use owlpost::answer::auto_error;
use owlpost::contacts::{ContactBook, key_standing};
use owlpost::envelope::{self, Body, Payload};
use owlpost::render::{self, AnswerRow, Markers};
use owlpost::route;
use owlpost::spool::{Dir, Record, Spool};
use serde::Serialize;
use serde_json::{Value, json};

use super::{StoredDraft, existing_answer, payload_of, peer_name, print_json, summary};

pub struct Opts {
    pub count: bool,
    pub new: bool,
    pub all: bool,
    pub format: Option<String>,
    pub json: bool,
    pub follow: bool,
    pub session: Option<String>,
    pub hook_event: String,
}

/// Session ids: `[A-Za-z0-9._-]{1,128}`, so `watch/<id>` can never leave `watch/`.
pub use owlpost::route::{SESSION_ID_RULE, pid_alive, valid_session_id};
/// Poll interval of `--follow` in seconds (fractions allowed); default 5.
pub const FOLLOW_SECS_ENV: &str = "OWLPOST_FOLLOW_SECS";
/// Where a running follow leaves its pid: `$OWLPOST_HOME/watch/<session id>`.
pub const MARKER_DIR: &str = "watch";
/// The hook event that wakes the session (Claude Code's `FileChanged`).
pub const FILE_CHANGED: &str = "FileChanged";
/// The hook event that ends a session's wake dir (Claude Code's `SessionEnd`).
pub const SESSION_END: &str = "SessionEnd";
/// The exit code Claude Code's `asyncRewake` hook turns into a new model turn.
pub const WAKE_EXIT: i32 = 2;

/// Prefix of every counter sentence: incoming messages wear the owl.
pub const ICON: &str = "🦉 ";

/// clap value parser for `--session`: a usage error (exit 2) for anything outside the rule.
pub fn parse_session_id(s: &str) -> Result<String, String> {
    if valid_session_id(s) {
        Ok(s.to_string())
    } else {
        Err(format!("session id must match {SESSION_ID_RULE}"))
    }
}

/// `$OWLPOST_HOME/watch/<id>`.
pub fn marker_path(home: &Path, id: &str) -> PathBuf {
    home.join(MARKER_DIR).join(id)
}

/// A marker is live when it holds the pid of a running process.
// ponytail: a recycled pid keeps a stale marker "live" until that process exits — store the
// process start time next to the pid when that shows up.
pub fn marker_is_live(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .is_some_and(|pid| pid > 0 && pid_alive(pid))
}

/// Removes every marker under `$OWLPOST_HOME/watch/` whose pid is dead, whatever its session
/// id; live markers stay. Never fails: an absent or unreadable directory is nothing to sweep.
pub fn sweep_markers(home: &Path) {
    let Ok(entries) = std::fs::read_dir(home.join(MARKER_DIR)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && !marker_is_live(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The hook input JSON Claude Code writes to the hook's stdin; `None` on a terminal, on
/// empty or unparseable input.
pub fn hook_input_from_stdin() -> Option<Value> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut raw = String::new();
    stdin.read_to_string(&mut raw).ok()?;
    serde_json::from_str(&raw).ok()
}

/// The `event` of a `FileChanged` hook input (`add` | `change` | `unlink`); `None` when absent
/// or not a string.
pub fn file_changed_event(input: &Value) -> Option<&str> {
    input.get("event")?.as_str()
}

/// The `session_id` of a hook input when it follows the rule; `None` otherwise.
pub fn session_id_of(input: Option<&Value>) -> Option<String> {
    input?
        .get("session_id")?
        .as_str()
        .filter(|s| valid_session_id(s))
        .map(str::to_string)
}

/// A string field of a hook input, `""` when absent.
fn input_str<'a>(input: Option<&'a Value>, key: &str) -> &'a str {
    input.and_then(|v| v.get(key)?.as_str()).unwrap_or_default()
}

/// The `FileChanged` wake: `Some(bytes)` when `event` is `add` and `file_path` is a
/// readable file directly under `<home>/sessions/<sid>/wake/` (both sides canonicalised, so
/// `sessions/S1/wake-evil/x.md` or `sessions/S10/wake/x.md` never match `S1`); `None` for
/// everything else.
pub fn wake_bytes(home: &Path, sid: &str, input: &Value) -> Option<Vec<u8>> {
    if file_changed_event(input) != Some("add") {
        return None;
    }
    let file = Path::new(input.get("file_path")?.as_str()?)
        .canonicalize()
        .ok()?;
    let wake = route::wake_dir(home, sid).canonicalize().ok()?;
    if file.parent() != Some(wake.as_path()) || !file.is_file() {
        return None;
    }
    std::fs::read(&file).ok()
}

/// `OWLPOST_FOLLOW_SECS` as a duration; unset, empty, unparseable or negative → 5 s.
pub fn follow_interval(raw: Option<&std::ffi::OsStr>) -> Duration {
    raw.and_then(|v| v.to_str()?.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .map_or(Duration::from_secs(5), Duration::from_secs_f64)
}

/// Closes the session-start preview: the model offers `/owlpost:inbox` and runs it only when
/// the user asks — the preview is metadata, the inbox pulls in the peers' text.
pub const OPEN_SENTENCE: &str =
    "owlpost: tell the user in one line and offer /owlpost:inbox; run it only when the user asks.";

/// One session-start preview line for an unseen record: metadata only, like the wake
/// (`route::wake_content`) — never the peer's path or text, which would reach the model as
/// context. `/owlpost:inbox` shows the rest.
pub fn preview(rec: &Record, payload: &Payload, book: &ContactBook) -> String {
    format!(
        "- {} {} [{}] · {}",
        render::peer_name_line(book, &payload.from),
        super::kind_str(payload.kind),
        rec.state,
        payload.id
    )
}

/// `$OWLPOST_HOME/plugin.json` → `watch`; absent file, unparseable JSON or a missing/non-bool
/// key all mean "on". Written by `/owlpost:watch on|off`; also read by the plugin's
/// `hooks/owlpost.tsx`, which treats anything but `true` as off.
pub fn watch_enabled(home: &Path) -> bool {
    std::fs::read(home.join("plugin.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("watch")?.as_bool())
        .unwrap_or(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Plain,
    Claude,
    Codex,
    Kimi,
}

impl Format {
    pub fn parse(s: &str) -> anyhow::Result<Format> {
        Ok(match s {
            "plain" => Format::Plain,
            "claude" => Format::Claude,
            "codex" => Format::Codex,
            "kimi" => Format::Kimi,
            other => bail!("unknown --format {other}: expected plain|claude|codex|kimi"),
        })
    }
}

/// `🦉 owlpost: 2 new questions (Martin 2). Say "show owlpost inbox" or run `owl inbox`.`
/// The [`ICON`] marks a message from a peer; the arm sentence and the preview
/// lines carry none. Peers are ordered by count (descending), then name. Singular below two. `answers` is how
/// many of the counted records are answers: all answers reads `1 new answer`, a mix reads
/// `2 new questions, 1 new answer`.
pub fn sentence(per_peer: &[(String, usize)], answers: usize) -> String {
    let total: usize = per_peer.iter().map(|(_, n)| n).sum();
    let questions = total.saturating_sub(answers);
    let plural = |n: usize, one: &str, many: &str| if n == 1 { one } else { many }.to_string();
    let what = match (questions, answers) {
        (_, 0) => format!("{total} new {}", plural(total, "question", "questions")),
        (0, a) => format!("{a} new {}", plural(a, "answer", "answers")),
        (q, a) => format!(
            "{q} new {}, {a} new {}",
            plural(q, "question", "questions"),
            plural(a, "answer", "answers")
        ),
    };
    let peers = per_peer
        .iter()
        .map(|(name, n)| format!("{name} {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{ICON}owlpost: {what} ({peers}). Say \"show owlpost inbox\" or run `owl inbox`.")
}

/// The exact §9 injection line for `format`; `None` when there is nothing to inject.
/// Non-empty `previews` (session start with unseen records) follow the counter on their own
/// lines, closed by [`OPEN_SENTENCE`]. With `watch_path` (`SessionStart` with the
/// watch enabled) `Format::Claude` emits the line even at count 0, its `hookSpecificOutput`
/// carrying `watchPaths: [<path>]` and `additionalContext` only when there is text. Other
/// formats ignore `previews` and `watch_path`.
pub fn injection(
    format: Format,
    per_peer: &[(String, usize)],
    answers: usize,
    previews: &[String],
    event: &str,
    watch_path: Option<&Path>,
) -> Option<String> {
    let watch_path = watch_path.filter(|_| format == Format::Claude);
    let empty = per_peer.iter().all(|(_, n)| *n == 0);
    if empty && watch_path.is_none() {
        return None;
    }
    let mut text = if empty {
        String::new()
    } else {
        sentence(per_peer, answers)
    };
    if format == Format::Claude && !empty && !previews.is_empty() {
        for p in previews {
            text.push('\n');
            text.push_str(p);
        }
        text.push('\n');
        text.push_str(OPEN_SENTENCE);
    }
    Some(match format {
        Format::Claude | Format::Codex => {
            // Structs keep the §9 key order; `json!` maps would sort keys alphabetically.
            #[derive(Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Hook<'a> {
                hook_specific_output: Inner<'a>,
            }
            #[derive(Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Inner<'a> {
                hook_event_name: &'a str,
                #[serde(skip_serializing_if = "Option::is_none")]
                additional_context: Option<&'a str>,
                #[serde(skip_serializing_if = "Option::is_none")]
                watch_paths: Option<[String; 1]>,
            }
            serde_json::to_string(&Hook {
                hook_specific_output: Inner {
                    hook_event_name: event,
                    additional_context: (!text.is_empty()).then_some(text.as_str()),
                    watch_paths: watch_path.map(|p| [p.to_string_lossy().into_owned()]),
                },
            })
            .expect("hook line is always serialisable")
        }
        Format::Kimi | Format::Plain => text,
    })
}

/// The §3.2 consent prompt for a held question: who, about which project, how the signing
/// key stands in the contact book, and the two commands.
pub fn consent_prompt(book: &ContactBook, payload: &Payload) -> String {
    let project = super::body_project(&payload.body).unwrap_or("?");
    let fp = &payload.from;
    format!(
        "{} wants to ask your agent about {project} — {} — owl allow {fp} [--once|--always] / owl deny {fp}",
        peer_name(book, fp),
        key_standing(book, fp).wording(fp)
    )
}

/// Per-peer counts over `records`, named through the contact book, ordered by count then name.
pub fn per_peer(
    records: &[(String, Record)],
    book: &ContactBook,
) -> anyhow::Result<Vec<(String, usize)>> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (id, rec) in records {
        let p = payload_of(id, rec)?;
        *counts
            .entry(render::peer_name_line(book, &p.from))
            .or_default() += 1;
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(v)
}

pub fn run(home: &Path, opts: Opts) -> anyhow::Result<()> {
    let format = opts.format.as_deref().map(Format::parse).transpose()?;
    let spool = Spool::new(home)?;
    let book = super::contact_book(home)?;
    if opts.follow {
        return follow(home, opts.session.as_deref(), opts.all);
    }
    if opts.count {
        return count(home, &spool, &book, &opts, format, watch_enabled(home));
    }
    let records = spool.list(Dir::Inbox, |r| !opts.new || !r.seen)?;
    // `#N` = position in the full inbox, so `--new` shows the same numbers as the whole listing.
    let all: Vec<String> = spool
        .list(Dir::Inbox, |_| true)?
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let now = envelope::now_unix();
    let mut rows = Vec::new();
    let mut notes = Vec::new();
    for (id, rec) in &records {
        let payload = payload_of(id, rec)?;
        let mut row = summary(id, rec, &payload, &book, now);
        let error = auto_error(rec);
        if let Some(obj) = row.as_object_mut() {
            obj.insert("auto_error".into(), json!(error));
            obj.insert(
                "n".into(),
                json!(all.iter().position(|x| x == id).map_or(0, |p| p + 1)),
            );
            // Only a held question asks the human to judge a key.
            if rec.state == "consent" {
                obj.insert(
                    "key_standing".into(),
                    json!(key_standing(&book, &payload.from).as_str()),
                );
            }
        }
        rows.push(row);
        if rec.state == "consent" {
            notes.push(consent_prompt(&book, &payload));
        }
        if let Some(e) = error {
            let retry = if existing_answer(&spool, id)?.is_some() {
                "send"
            } else {
                "draft"
            };
            notes.push(format!(
                "{id}: auto-accept failed ({}): {e} — owl {retry} {id} to retry by hand",
                peer_name(&book, &payload.from)
            ));
        }
    }
    if opts.json {
        print_json(&Value::Array(rows.clone()))?;
    } else if matches!(format, Some(Format::Claude | Format::Codex | Format::Kimi)) {
        print_rendered(home, &spool, &book, &records, now)?;
    } else {
        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|r| {
                std::iter::once(format!("#{}", r["n"]))
                    .chain(
                        ["id", "from_name", "type", "state", "path", "age", "summary"]
                            .iter()
                            .map(|k| r[k].as_str().unwrap_or_default().to_string()),
                    )
                    .collect()
            })
            .collect();
        let styles: Vec<&str> = rows
            .iter()
            .map(|r| {
                super::row_style(
                    r["type"].as_str().unwrap_or_default(),
                    r["seen"].as_bool().unwrap_or(true),
                )
            })
            .collect();
        super::print_table_styled(
            &["#", "ID", "FROM", "TYPE", "STATE", "PATH", "AGE", "SUMMARY"],
            &cells,
            &styles,
        );
        for n in &notes {
            println!("{n}");
        }
    }
    for (id, rec) in &records {
        if !rec.seen {
            spool.mark_seen(Dir::Inbox, id)?;
        }
        // Listed: no session needs to wake for it.
        route::release(home, id);
    }
    Ok(())
}

/// The `--format claude` listing: one question table (`consent` first), then the answers
/// table, sections separated by one blank line; nothing for an empty listing.
fn print_rendered(
    home: &Path,
    spool: &Spool,
    book: &ContactBook,
    records: &[(String, Record)],
    now: u64,
) -> anyhow::Result<()> {
    let mut consent = Vec::new();
    let mut questions = Vec::new();
    let mut answers = Vec::new();
    for (id, rec) in records {
        let payload = payload_of(id, rec)?;
        match &payload.body {
            // A content request is held for consent and reviewed exactly like a
            // question, so it takes the same two sections.
            Body::Question { .. } | Body::Content { .. } | Body::ToolCall { .. } => {
                let draft = StoredDraft::from_record(id, rec)?;
                let block = render::record_block(spool, book, rec, &payload, draft.as_ref());
                if rec.state == "consent" {
                    consent.push(block);
                } else {
                    questions.push(block);
                }
            }
            Body::Answer { .. } | Body::ContentReply { .. } | Body::ToolReply { .. } => {
                let answer = super::body_text(&payload.body).into_owned();
                let question_id = payload.in_reply_to.clone();
                let question_first_line = question_id
                    .as_deref()
                    .and_then(|q| render::find_question(spool, q))
                    .and_then(|q| match &q.body {
                        Body::Question { question, .. } => Some(question.clone()),
                        Body::Content { .. } => Some(owlpost::content::body_summary(&q.body)),
                        Body::ToolCall { .. } => Some(owlpost::tools::body_summary(&q.body)),
                        Body::Answer { .. }
                        | Body::ContentReply { .. }
                        | Body::ToolReply { .. } => None,
                    })
                    .unwrap_or_default();
                answers.push(AnswerRow {
                    received_at: rec.received_at.clone(),
                    fingerprint: payload.from.clone(),
                    peer_name: peer_name(book, &payload.from),
                    answer,
                    question_id,
                    question_first_line,
                });
            }
        }
    }
    let mut markers = Markers::load(home);
    let table = render::answers_table(&answers, now, &mut markers);
    markers.save(home)?;
    let sections: Vec<String> = consent
        .into_iter()
        .chain(questions)
        .chain((!table.is_empty()).then_some(table))
        .collect();
    if !sections.is_empty() {
        println!("{}", sections.join("\n\n"));
    }
    Ok(())
}

/// Unseen (or, with `all`, every) inbox record with the per-peer and question counts.
struct Counts {
    records: Vec<(String, Record)>,
    per_peer: Vec<(String, usize)>,
    questions: usize,
}

fn counts(spool: &Spool, book: &ContactBook, all: bool) -> anyhow::Result<Counts> {
    let records = spool.list(Dir::Inbox, |r| all || !r.seen)?;
    let per_peer = per_peer(&records, book)?;
    let questions = records
        .iter()
        // A content request wants the owner's attention like a question does.
        .filter(|(id, r)| payload_of(id, r).is_ok_and(|p| p.kind.is_request()))
        .count();
    Ok(Counts {
        records,
        per_peer,
        questions,
    })
}

fn count(
    home: &Path,
    spool: &Spool,
    book: &ContactBook,
    opts: &Opts,
    format: Option<Format>,
    watch: bool,
) -> anyhow::Result<()> {
    let Opts { all, json, .. } = *opts;
    let event = opts.hook_event.as_str();
    let session_start = event == "SessionStart";
    // The Claude hook input: read once, for every event; unusable stdin means no
    // session and every event behaves as before.
    let input = (format == Some(Format::Claude))
        .then(hook_input_from_stdin)
        .flatten();
    let sid = session_id_of(input.as_ref());
    if session_start {
        sweep_markers(home);
    }
    if format == Some(Format::Claude) {
        match event {
            FILE_CHANGED => {
                // The wake: print the session's wake file byte for byte and exit 2; anything
                // else, including unusable stdin, is a silent exit 0.
                if let (Some(sid), Some(input)) = (&sid, &input)
                    && watch
                    && let Some(bytes) = wake_bytes(home, sid, input)
                {
                    let mut err = std::io::stderr().lock();
                    let _ = err.write_all(&bytes);
                    let _ = err.flush();
                    std::process::exit(WAKE_EXIT);
                }
                return Ok(());
            }
            SESSION_END => {
                if let Some(sid) = &sid {
                    route::end_session(home, sid);
                }
                return Ok(());
            }
            "UserPromptSubmit" | "PostToolUse" => {
                if let Some(sid) = &sid {
                    let _ = route::touch_heartbeat(home, sid);
                }
            }
            _ => {}
        }
    }
    // SessionStart with a session: the private wake dir, the marker, the sweep, the backlog.
    let registered = session_start
        && sid.as_ref().is_some_and(|sid| {
            let marker = route::Marker::new(
                sid,
                input_str(input.as_ref(), "cwd"),
                input_str(input.as_ref(), "source"),
            );
            let ok = route::write_marker(home, &marker).is_ok();
            route::sweep_sessions(home);
            route::assign_backlog(home, spool, sid);
            ok
        });
    let Counts {
        records,
        per_peer,
        questions,
    } = counts(spool, book, all)?;
    let total = records.len();
    let previews = if session_start {
        records
            .iter()
            .map(|(id, r)| Ok(preview(r, &payload_of(id, r)?, book)))
            .collect::<anyhow::Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    match format {
        Some(f) => {
            let watch_path = (registered && watch)
                .then(|| sid.as_deref().map(|sid| route::wake_watch_path(home, sid)))
                .flatten();
            if let Some(line) = injection(
                f,
                &per_peer,
                total - questions,
                &previews,
                event,
                watch_path.as_deref(),
            ) {
                println!("{line}");
            }
        }
        None if json => {
            print_json(&json!({
                "count": total,
                "questions": questions,
                "peers": per_peer.iter().map(|(n, c)| json!({"name": n, "count": c})).collect::<Vec<_>>(),
            }))?;
        }
        None => println!("{total}"),
    }
    Ok(())
}

/// One poll of the follow loop: the plain counter sentence, `None` at zero or on any error
/// (a failed poll prints nothing, like the shell loop it replaced).
fn poll(home: &Path, all: bool) -> Option<String> {
    let spool = Spool::new(home).ok()?;
    let book = super::contact_book(home).ok()?;
    let c = counts(&spool, &book, all).ok()?;
    injection(
        Format::Plain,
        &c.per_peer,
        c.records.len() - c.questions,
        &[],
        "",
        None,
    )
}

/// `owl inbox --count --follow [--session <id>]`: see the module doc.
// ponytail: the poll loop is the live-watch fallback for hosts without a `FileChanged` hook
// (Codex, Kimi, an older Claude Code); the Claude plugin no longer references it — drop it
// when every harness offers an event-driven wake.
pub fn follow(home: &Path, session: Option<&str>, all: bool) -> anyhow::Result<()> {
    let marker = session.map(|id| marker_path(home, id));
    if let Some(m) = &marker {
        let dir = m.parent().expect("marker has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(m, format!("{}\n", std::process::id()))
            .with_context(|| format!("writing {}", m.display()))?;
    }
    let interval = follow_interval(std::env::var_os(FOLLOW_SECS_ENV).as_deref());
    let mut prev: Option<String> = None;
    let mut out = std::io::stdout();
    loop {
        let cur = poll(home, all);
        if cur != prev
            && let Some(line) = &cur
            && (writeln!(out, "{line}").is_err() || out.flush().is_err())
        {
            // The reader is gone (the host stopped the loop): a clean end.
            break;
        }
        prev = cur;
        std::thread::sleep(interval);
        if marker.as_ref().is_some_and(|m| !m.exists()) {
            // Removed from outside: the way to stop a watch without knowing its pid.
            break;
        }
    }
    if let Some(m) = &marker {
        let _ = std::fs::remove_file(m);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn peers(v: &[(&str, usize)]) -> Vec<(String, usize)> {
        v.iter().map(|(n, c)| (n.to_string(), *c)).collect()
    }

    #[test]
    fn sentence_matches_design_literal() {
        assert_eq!(
            sentence(&peers(&[("Martin", 2)]), 0),
            "🦉 owlpost: 2 new questions (Martin 2). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Martin", 1)]), 0),
            "🦉 owlpost: 1 new question (Martin 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Ana", 2), ("Martin", 1)]), 0),
            "🦉 owlpost: 3 new questions (Ana 2, Martin 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Martin", 1)]), 1),
            "🦉 owlpost: 1 new answer (Martin 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
        assert_eq!(
            sentence(&peers(&[("Ana", 2), ("Martin", 1)]), 1),
            "🦉 owlpost: 2 new questions, 1 new answer (Ana 2, Martin 1). Say \"show owlpost inbox\" or run `owl inbox`."
        );
    }

    #[test]
    fn injection_shapes_per_format() {
        let p = peers(&[("Martin", 2)]);
        let claude = injection(Format::Claude, &p, 0, &[], "UserPromptSubmit", None).unwrap();
        assert_eq!(
            claude,
            r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"🦉 owlpost: 2 new questions (Martin 2). Say \"show owlpost inbox\" or run `owl inbox`."}}"#
        );
        assert_eq!(
            injection(Format::Codex, &p, 0, &[], "UserPromptSubmit", None).unwrap(),
            claude
        );
        assert_eq!(
            injection(Format::Kimi, &p, 0, &[], "UserPromptSubmit", None).unwrap(),
            sentence(&p, 0)
        );
        assert_eq!(
            injection(Format::Plain, &p, 0, &[], "UserPromptSubmit", None).unwrap(),
            sentence(&p, 0)
        );
        for f in [Format::Plain, Format::Claude, Format::Codex, Format::Kimi] {
            assert_eq!(injection(f, &[], 0, &[], "UserPromptSubmit", None), None);
            assert_eq!(
                injection(
                    f,
                    &peers(&[("Martin", 0)]),
                    0,
                    &[],
                    "UserPromptSubmit",
                    None
                ),
                None
            );
        }
    }

    /// The exact SessionStart line for `context`: serde's own string escaping, §9 key order.
    fn session_start_line(context: &str) -> String {
        format!(
            r#"{{"hookSpecificOutput":{{"hookEventName":"SessionStart","additionalContext":{}}}}}"#,
            serde_json::to_string(context).unwrap()
        )
    }

    /// `watch_path` makes the Claude line go out at count 0 with `watchPaths`
    /// alone; with a count the context comes first and `watchPaths` follows; every other
    /// format ignores it. No output ever carries the retired arm sentence.
    #[test]
    fn watch_path_is_claude_only_and_stands_alone_at_zero() {
        let p = peers(&[("Martin", 2)]);
        let inbox = Path::new("/h/spool/inbox");
        let zero = injection(Format::Claude, &[], 0, &[], "SessionStart", Some(inbox)).unwrap();
        assert_eq!(
            zero,
            r#"{"hookSpecificOutput":{"hookEventName":"SessionStart","watchPaths":["/h/spool/inbox"]}}"#
        );
        assert_eq!(
            injection(
                Format::Claude,
                &peers(&[("Martin", 0)]),
                0,
                &[],
                "SessionStart",
                Some(inbox)
            )
            .unwrap(),
            zero
        );
        let v: serde_json::Value = serde_json::from_str(&zero).unwrap();
        assert!(v["hookSpecificOutput"].get("additionalContext").is_none());
        let two = injection(Format::Claude, &p, 0, &[], "SessionStart", Some(inbox)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&two).unwrap();
        assert_eq!(
            v,
            json!({"hookSpecificOutput": {"hookEventName": "SessionStart",
                "additionalContext": sentence(&p, 0), "watchPaths": ["/h/spool/inbox"]}})
        );
        assert_eq!(
            two,
            format!(
                r#"{{"hookSpecificOutput":{{"hookEventName":"SessionStart","additionalContext":{},"watchPaths":["/h/spool/inbox"]}}}}"#,
                serde_json::to_string(&sentence(&p, 0)).unwrap()
            )
        );
        // Without a watch path the line is the plain counter (or nothing at zero).
        assert_eq!(
            injection(Format::Claude, &p, 0, &[], "SessionStart", None).unwrap(),
            session_start_line(&sentence(&p, 0))
        );
        assert_eq!(
            injection(Format::Claude, &[], 0, &[], "SessionStart", None),
            None
        );
        // Every other format ignores `watch_path`: same as without, nothing at zero.
        for f in [Format::Plain, Format::Codex, Format::Kimi] {
            assert_eq!(
                injection(f, &p, 0, &[], "SessionStart", Some(inbox)),
                injection(f, &p, 0, &[], "SessionStart", None),
                "{f:?}"
            );
            assert_eq!(
                injection(f, &[], 0, &[], "SessionStart", Some(inbox)),
                None,
                "{f:?}"
            );
        }
        for line in [&zero, &two] {
            assert!(
                !line.contains("owlpost: before handling this prompt"),
                "{line}"
            );
            assert!(!line.contains("--follow"), "{line}");
        }
    }

    #[test]
    fn previews_follow_the_counter_and_close_with_open_sentence_claude_only() {
        let p = peers(&[("Martin", 1)]);
        let inbox = Path::new("/h/spool/inbox");
        let pv =
            vec!["- Martin question [consent] · 0191c7a0-0000-7000-8000-000000000001".to_string()];
        let line = injection(Format::Claude, &p, 0, &pv, "SessionStart", Some(inbox)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["additionalContext"],
            format!("{}\n{}\n{OPEN_SENTENCE}", sentence(&p, 0), pv[0])
        );
        assert_eq!(
            v["hookSpecificOutput"]["watchPaths"],
            json!(["/h/spool/inbox"])
        );
        // Watch off: previews still go out, without `watchPaths`.
        let line = injection(Format::Claude, &p, 0, &pv, "SessionStart", None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            v["hookSpecificOutput"]["additionalContext"],
            format!("{}\n{}\n{OPEN_SENTENCE}", sentence(&p, 0), pv[0])
        );
        assert!(v["hookSpecificOutput"].get("watchPaths").is_none());
        // Zero unseen: no previews, no open sentence.
        assert_eq!(
            injection(Format::Claude, &[], 0, &pv, "SessionStart", Some(inbox)).unwrap(),
            injection(Format::Claude, &[], 0, &[], "SessionStart", Some(inbox)).unwrap()
        );
        for f in [Format::Plain, Format::Codex, Format::Kimi] {
            assert_eq!(
                injection(f, &p, 0, &pv, "SessionStart", None),
                injection(f, &p, 0, &[], "SessionStart", None),
                "{f:?}"
            );
        }
    }

    #[test]
    fn wake_watch_path_is_absolute_and_canonical_when_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let wake = home.join("sessions").join("S1").join("wake");
        // Absent: the absolute join, untouched.
        assert_eq!(route::wake_watch_path(home, "S1"), wake);
        // Present: canonicalised (a symlinked home resolves to the real directory).
        std::fs::create_dir_all(&wake).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(home, &link).unwrap();
        assert_eq!(
            route::wake_watch_path(&link, "S1"),
            home.canonicalize()
                .unwrap()
                .join("sessions")
                .join("S1")
                .join("wake")
        );
        assert!(route::wake_watch_path(&link, "S1").is_absolute());
        // A relative home is resolved against the current directory.
        let rel = route::wake_watch_path(Path::new("rel-home"), "S1");
        assert!(rel.is_absolute(), "{}", rel.display());
        assert!(
            rel.ends_with("rel-home/sessions/S1/wake"),
            "{}",
            rel.display()
        );
    }

    #[test]
    fn session_id_of_reads_only_a_valid_string() {
        let v = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        assert_eq!(
            session_id_of(Some(&v(r#"{"session_id":"S-1.x"}"#))),
            Some("S-1.x".into())
        );
        for bad in [
            "{}",
            r#"{"session_id": 5}"#,
            r#"{"session_id": ""}"#,
            r#"{"session_id": "a/b"}"#,
            r#"{"sessionId": "S"}"#,
            "[]",
        ] {
            assert_eq!(session_id_of(Some(&v(bad))), None, "{bad}");
        }
        assert_eq!(session_id_of(None), None);
    }

    #[test]
    fn wake_bytes_only_for_an_add_directly_under_the_session_wake_dir() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let wake = route::wake_dir(home, "S1");
        std::fs::create_dir_all(&wake).unwrap();
        std::fs::create_dir_all(route::wake_dir(home, "S10")).unwrap();
        std::fs::create_dir_all(home.join("sessions").join("S1").join("wake-evil")).unwrap();
        std::fs::create_dir_all(wake.join("nested")).unwrap();
        let body = "| line one |\nline two\n\n";
        std::fs::write(wake.join("r.md"), body).unwrap();
        std::fs::write(route::wake_dir(home, "S10").join("r.md"), "other").unwrap();
        std::fs::write(
            home.join("sessions")
                .join("S1")
                .join("wake-evil")
                .join("r.md"),
            "evil",
        )
        .unwrap();
        std::fs::write(wake.join("nested").join("r.md"), "nested").unwrap();
        let input = |event: &str, path: &Path| json!({"session_id": "S1", "event": event, "file_path": path.to_string_lossy()});
        assert_eq!(
            wake_bytes(home, "S1", &input("add", &wake.join("r.md"))),
            Some(body.as_bytes().to_vec())
        );
        // Through a symlinked home the canonical file still matches.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(home, &link).unwrap();
        assert_eq!(
            wake_bytes(
                &link,
                "S1",
                &input("add", &link.join("sessions/S1/wake/r.md"))
            ),
            Some(body.as_bytes().to_vec())
        );
        for event in ["change", "unlink", "", "Add"] {
            assert_eq!(
                wake_bytes(home, "S1", &input(event, &wake.join("r.md"))),
                None
            );
        }
        for other in [
            route::wake_dir(home, "S10").join("r.md"),
            home.join("sessions/S1/wake-evil/r.md"),
            wake.join("nested/r.md"),
            wake.join("nested"),
            wake.join("missing.md"),
            home.join("spool/inbox/r.json"),
        ] {
            assert_eq!(
                wake_bytes(home, "S1", &input("add", &other)),
                None,
                "{}",
                other.display()
            );
        }
        assert_eq!(
            wake_bytes(home, "S10", &input("add", &wake.join("r.md"))),
            None
        );
        assert_eq!(wake_bytes(home, "S1", &json!({"event": "add"})), None);
        assert_eq!(
            wake_bytes(home, "S1", &json!({"event": "add", "file_path": 5})),
            None
        );
    }

    #[test]
    fn file_changed_event_reads_the_event_string_only() {
        let v = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        assert_eq!(
            file_changed_event(&v(
                r#"{"session_id":"S","hook_event_name":"FileChanged","file_path":"/x","event":"add"}"#
            )),
            Some("add")
        );
        assert_eq!(
            file_changed_event(&v(r#"{"event":"change"}"#)),
            Some("change")
        );
        assert_eq!(
            file_changed_event(&v(r#"{"event":"unlink"}"#)),
            Some("unlink")
        );
        for raw in ["{}", r#"{"event": 5}"#, r#"{"Event": "add"}"#, "[]", "null"] {
            assert_eq!(file_changed_event(&v(raw)), None, "{raw:?}");
        }
    }

    #[test]
    fn session_ids_follow_the_rule() {
        for ok in ["a", "abc-123_x.y", &"z".repeat(128), "0", "A-B"] {
            assert!(valid_session_id(ok), "{ok:?}");
            assert_eq!(parse_session_id(ok).unwrap(), ok);
        }
        for bad in [
            "",
            &"z".repeat(129),
            "a b",
            "../x",
            "a/b",
            "ü",
            "a\n",
            "$(x)",
            "<session id>",
        ] {
            assert!(!valid_session_id(bad), "{bad:?}");
            let err = parse_session_id(bad).unwrap_err();
            assert!(err.contains(SESSION_ID_RULE), "{err}");
        }
        // The marker never leaves `watch/`: the path is home/watch/<id> for a valid id.
        assert_eq!(
            marker_path(Path::new("/h"), "abc"),
            PathBuf::from("/h/watch/abc")
        );
    }

    #[test]
    fn marker_is_live_only_for_a_running_pid() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("m");
        // A spawned child that is still running: live.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        std::fs::write(&marker, format!("{}\n", child.id())).unwrap();
        assert!(marker_is_live(&marker));
        // The same pid once the child has exited and been reaped: dead.
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!marker_is_live(&marker));
        // A pid that cannot exist, a non-number, zero, an empty or missing file: dead.
        for body in ["4194303", "999999999", "abc", "0", "", "-1", "1.5"] {
            std::fs::write(&marker, body).unwrap();
            assert!(!marker_is_live(&marker), "{body:?}");
        }
        std::fs::remove_file(&marker).unwrap();
        assert!(!marker_is_live(&marker));
        // Our own pid: live.
        std::fs::write(&marker, std::process::id().to_string()).unwrap();
        assert!(marker_is_live(&marker));
    }

    /// The sweep removes every dead marker whatever its name, keeps live ones,
    /// leaves non-files alone and never fails on an absent or unreadable directory.
    #[test]
    fn sweep_markers_removes_dead_markers_of_any_session_and_keeps_live_ones() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        sweep_markers(home); // no watch dir: nothing happens
        assert!(!home.join(MARKER_DIR).exists());
        std::fs::create_dir_all(home.join(MARKER_DIR)).unwrap();
        let mut reaped = Command::new("true").spawn().unwrap();
        reaped.wait().unwrap();
        let mut sleeper = Command::new("sleep").arg("30").spawn().unwrap();
        std::fs::write(marker_path(home, "dead-1"), reaped.id().to_string()).unwrap();
        std::fs::write(marker_path(home, "dead-2"), "abc").unwrap();
        std::fs::write(marker_path(home, "dead-3"), "").unwrap();
        std::fs::write(marker_path(home, "live-1"), sleeper.id().to_string()).unwrap();
        std::fs::write(marker_path(home, "live-2"), std::process::id().to_string()).unwrap();
        std::fs::create_dir(marker_path(home, "a-dir")).unwrap();
        sweep_markers(home);
        for gone in ["dead-1", "dead-2", "dead-3"] {
            assert!(!marker_path(home, gone).exists(), "{gone} removed");
        }
        for kept in ["live-1", "live-2", "a-dir"] {
            assert!(marker_path(home, kept).exists(), "{kept} kept");
        }
        // Once the sleeper is gone its marker goes too; the others stay.
        sleeper.kill().unwrap();
        sleeper.wait().unwrap();
        sweep_markers(home);
        assert!(!marker_path(home, "live-1").exists());
        assert!(marker_path(home, "live-2").exists());
        // `watch` being a file, not a directory: nothing to sweep, no error.
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join(MARKER_DIR), "x").unwrap();
        sweep_markers(other.path());
        assert!(other.path().join(MARKER_DIR).is_file());
    }

    #[test]
    fn follow_interval_reads_the_env_var_with_a_five_second_default() {
        use std::ffi::OsStr;
        let secs = |s: &str| follow_interval(Some(OsStr::new(s)));
        assert_eq!(follow_interval(None), Duration::from_secs(5));
        assert_eq!(secs("0.05"), Duration::from_millis(50));
        assert_eq!(secs("2"), Duration::from_secs(2));
        assert_eq!(secs("0"), Duration::ZERO);
        for bad in ["", "x", "-1", "inf", "nan"] {
            assert_eq!(secs(bad), Duration::from_secs(5), "{bad:?}");
        }
    }

    #[test]
    fn watch_enabled_defaults_on_and_reads_plugin_json() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let file = home.join("plugin.json");
        assert!(watch_enabled(home), "absent file");
        std::fs::write(&file, r#"{"watch": false}"#).unwrap();
        assert!(!watch_enabled(home), "watch false");
        std::fs::write(&file, r#"{"watch": true}"#).unwrap();
        assert!(watch_enabled(home), "watch true");
        std::fs::write(&file, r#"{"other": false}"#).unwrap();
        assert!(watch_enabled(home), "missing key");
        std::fs::write(&file, r#"{"watch": "false"}"#).unwrap();
        assert!(watch_enabled(home), "non-bool value");
        std::fs::write(&file, "{not json").unwrap();
        assert!(watch_enabled(home), "unparseable");
        std::fs::write(&file, "").unwrap();
        assert!(watch_enabled(home), "empty file");
    }

    #[test]
    fn consent_prompt_names_peer_project_and_both_commands() {
        use owlpost::contacts::Contact;
        let q = Payload::question("FPA", "FPB", "github.com/x/y", Some("src/a.rs"), "why?");
        let book = |name: &str, source: &str| ContactBook {
            contacts: vec![Contact {
                name: name.into(),
                emails: vec![],
                pubkey: "ed25519:x".into(),
                endpoints: vec![],
                source: source.into(),
                policy: None,
                added_at: None,
                fingerprint: "FPA".into(),
            }],
        };
        let prompt = |standing: &str| {
            format!(
                "Ana wants to ask your agent about github.com/x/y — {standing} — owl allow FPA [--once|--always] / owl deny FPA"
            )
        };
        // The standing sits between the project and the `owl allow` hint, in
        // each of the three wordings of §1.
        assert_eq!(
            consent_prompt(&book("Ana", "local"), &q),
            prompt(
                "known key: contact \"Ana\" — repo peer file (.agents/peers/, reviewed in a PR)"
            )
        );
        assert_eq!(
            consent_prompt(&book("Ana", "global"), &q),
            prompt(
                "known key: contact \"Ana\" — added by hand (global book; fingerprint not verified through a PR)"
            )
        );
        // Unknown peer: the fingerprint stands in for the name, in the display and in the
        // standing, which says the key is not in the book any more.
        assert_eq!(
            consent_prompt(&ContactBook::default(), &q),
            "FPA wants to ask your agent about github.com/x/y — unknown key: no longer in your contacts — deny, or re-add the peer file before allowing — owl allow FPA [--once|--always] / owl deny FPA"
        );
        // A bare policy overlay (empty name): the standing shows the fingerprint in place of
        // the name. `<display>` is unchanged (contact name, else fingerprint), so
        // for a nameless contact it stays the empty name the book holds.
        assert_eq!(
            consent_prompt(&book("", "global"), &q),
            " wants to ask your agent about github.com/x/y — known key: contact \"FPA\" — added by hand (global book; fingerprint not verified through a PR) — owl allow FPA [--once|--always] / owl deny FPA"
        );
        let a = Payload::answer(&q, "because", "fake", 0, false);
        assert!(consent_prompt(&book("Ana", "local"), &a).contains("about ? —"));
    }

    #[test]
    fn format_parse_rejects_unknown() {
        assert_eq!(Format::parse("claude").unwrap(), Format::Claude);
        assert_eq!(Format::parse("codex").unwrap(), Format::Codex);
        assert_eq!(Format::parse("kimi").unwrap(), Format::Kimi);
        assert_eq!(Format::parse("plain").unwrap(), Format::Plain);
        let err = Format::parse("Claude").unwrap_err().to_string();
        assert!(err.contains("unknown --format Claude"), "{err}");
    }
}
