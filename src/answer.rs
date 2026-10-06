//! Answering a spooled question (§3.4, §8): the one implementation of *draft* and *send*
//! shared by `owl draft` / `owl send` and the daemon's auto-accept scheduler (`crate::auto`),
//! plus the record helpers they need and the outgoing log (`log/outgoing.jsonl`).
//!
//! Nothing here prints: the CLI wraps these functions with its own output and exit codes.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::envelope::{self, Body, Envelope, Kind, Payload};
use crate::events::{self, Ev};
use crate::identity::{self, Identity};
use crate::runner;
use crate::spool::{Dir, Record, Spool};

/// `log/outgoing.jsonl` relative to the home: one line per answer that left this machine.
pub const OUTGOING_LOG: &str = "log/outgoing.jsonl";
/// `meta` key naming why the scheduler's last automatic attempt failed (`crate::auto`).
pub const AUTO_ERROR: &str = "auto_error";

/// Draft as stored on an inbox record (`record.draft`), §3.4 step 4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDraft {
    pub text: String,
    pub harness: String,
    pub redactions: u32,
    pub status: String,
    pub drafted_at: String,
}

impl StoredDraft {
    pub fn from_runner(d: &runner::Draft) -> StoredDraft {
        StoredDraft {
            text: d.text.clone(),
            harness: d.harness.clone(),
            redactions: u32::try_from(d.redactions).unwrap_or(u32::MAX),
            status: d.status.as_str().to_string(),
            drafted_at: envelope::rfc3339_now(),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "text": self.text,
            "harness": self.harness,
            "redactions": self.redactions,
            "status": self.status,
            "drafted_at": self.drafted_at,
        })
    }

    /// `None` when the record has no draft; an error when it has one of the wrong shape.
    pub fn from_record(id: &str, rec: &Record) -> anyhow::Result<Option<StoredDraft>> {
        let Some(v) = rec.draft.as_ref().filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let field = |name: &str| -> anyhow::Result<&str> {
            v.get(name)
                .and_then(Value::as_str)
                .with_context(|| format!("record {id}: draft is malformed (missing {name})"))
        };
        let redactions = v
            .get("redactions")
            .and_then(Value::as_u64)
            .with_context(|| format!("record {id}: draft is malformed (missing redactions)"))?;
        Ok(Some(StoredDraft {
            text: field("text")?.to_string(),
            harness: field("harness")?.to_string(),
            redactions: u32::try_from(redactions).unwrap_or(u32::MAX),
            status: field("status")?.to_string(),
            drafted_at: field("drafted_at")?.to_string(),
        }))
    }
}

/// Parses the stored raw payload. The bytes were verified on receipt (§3.2), so no
/// signature check here.
pub fn payload_of(id: &str, rec: &Record) -> anyhow::Result<Payload> {
    serde_json::from_str(&rec.raw).with_context(|| format!("record {id}: payload is malformed"))
}

/// The inbox record for `id`, or a user error naming the id.
pub fn inbox_record(spool: &Spool, id: &str) -> anyhow::Result<Record> {
    spool
        .get(Dir::Inbox, id)?
        .with_context(|| format!("no inbox record {id}"))
}

/// `rec.meta` as an object. Records carry an object there (`{peer, hash}` from the daemon), but
/// the value is stored JSON, so a stray scalar or array is replaced by an empty object rather
/// than indexed into (never `IndexMut` a parsed `Value`).
pub fn meta_object(rec: &mut Record) -> &mut Map<String, Value> {
    if !rec.meta.is_object() {
        rec.meta = Value::Object(Map::new());
    }
    match &mut rec.meta {
        Value::Object(m) => m,
        _ => unreachable!("meta was just set to an object"),
    }
}

/// `meta.auto_error` of a record, if the scheduler left one (§3.4 auto failures).
pub fn auto_error(rec: &Record) -> Option<&str> {
    rec.meta.get(AUTO_ERROR).and_then(Value::as_str)
}

/// Finishes an inbox record: `state`, `meta.done_at` and the `extra` meta keys are set on the
/// record, which is written atomically (temp file + rename, `Spool::put`) into `done/`; only
/// after that succeeds is the `inbox/` file removed. Two failure shapes, both retryable by
/// running the same command again:
///
/// * writing `done/` fails (destination blocked): nothing was touched, the inbox record is
///   byte-for-byte as it was;
/// * removing the inbox file fails (e.g. `inbox/` not writable): the finished copy sits in
///   `done/` next to the untouched original in `inbox/`; the retry rewrites `done/` in place
///   and removes the original.
///
/// Either way the error is returned, never swallowed, so the caller exits non-zero. The
/// record's wake files and routing (`crate::route`) are released once `done/` holds it.
pub fn finish(
    spool: &Spool,
    id: &str,
    mut rec: Record,
    state: &str,
    extra: &[(&str, Value)],
    event: Ev<'_>,
) -> anyhow::Result<()> {
    rec.state = state.to_string();
    let meta = meta_object(&mut rec);
    meta.insert("done_at".into(), json!(envelope::rfc3339_now()));
    for (k, v) in extra {
        meta.insert((*k).to_string(), v.clone());
    }
    // The closing event is the last thing the record carries into `done/`.
    events::push_ev(&mut rec, event);
    spool
        .put(Dir::Done, id, &rec)
        .with_context(|| format!("finishing record {id}"))?;
    // The record left the inbox: no session should wake for it any more.
    crate::route::release(spool.home(), id);
    let src = spool.path(Dir::Inbox, id);
    std::fs::remove_file(&src).with_context(|| format!("removing {}", src.display()))
}

/// `(answer id, recipient)` of the outbox envelope already answering question `id`, if any.
/// `send` reuses it instead of signing a second one and `edit` refuses to change a draft that
/// has already been shipped this way.
pub fn existing_answer(spool: &Spool, id: &str) -> anyhow::Result<Option<(String, String)>> {
    let found = spool.list(Dir::Outbox, |r| {
        r.meta.get("question_id").and_then(Value::as_str) == Some(id)
    })?;
    let Some((aid, arec)) = found.into_iter().next() else {
        return Ok(None);
    };
    let answer = payload_of(&aid, &arec)?;
    Ok(Some((aid, answer.to)))
}

/// The `(project, path, question)` of a question record; an error for answer records.
pub fn question_body<'a>(
    id: &str,
    payload: &'a Payload,
) -> anyhow::Result<(&'a str, Option<&'a str>, &'a str)> {
    match &payload.body {
        Body::Question {
            project,
            path,
            question,
            ..
        } if payload.kind == Kind::Question => Ok((project, path.as_deref(), question)),
        _ => bail!("record {id} is an answer, not a question"),
    }
}

/// The `context` of a question record's payload (`None` for answers).
pub fn question_context(payload: &Payload) -> Option<&str> {
    match &payload.body {
        Body::Question { context, .. } => context.as_deref(),
        // A content request carries no snippet, and a reply is not a question at all.
        Body::Answer { .. }
        | Body::Content { .. }
        | Body::ContentReply { .. }
        | Body::ToolCall { .. }
        | Body::ToolReply { .. } => None,
    }
}

/// Every parseable record of `dir` whose payload carries thread `context_id`, except the one
/// with id `except` (the record being looked at itself). Corrupt files are skipped.
fn thread_records(
    spool: &Spool,
    dir: Dir,
    context_id: &str,
    except: &str,
) -> Vec<(String, Record, Payload)> {
    spool
        .list_lenient(dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|(id, _)| id != except)
        .filter_map(|(id, rec)| {
            let payload = payload_of(&id, &rec).ok()?;
            (payload.context_id.as_deref() == Some(context_id)).then_some((id, rec, payload))
        })
        .collect()
}

/// True when `done/` holds an earlier exchange of thread `context_id` other than `except`:
/// what the `↩ follow-up in thread` line and the prompt history key on. Signed
/// content only — our own records, never the incoming payload's word.
pub fn thread_has_earlier(spool: &Spool, context_id: &str, except: &str) -> bool {
    !thread_records(spool, Dir::Done, context_id, except).is_empty()
}

/// True when `inbox/` or `done/` holds any record of thread `context_id` other than `except`:
/// such a question is never served from or written to the responder cache.
pub fn thread_known(spool: &Spool, context_id: &str, except: &str) -> bool {
    thread_has_earlier(spool, context_id, except)
        || !thread_records(spool, Dir::Inbox, context_id, except).is_empty()
}

/// How many earlier exchanges the prompt shows at most.
pub const THREAD_HISTORY_MAX: usize = 3;

/// The `(question, answer we sent)` pairs of thread `context_id` from our own `done/`
/// question records other than `except`, the [`THREAD_HISTORY_MAX`] most recent, oldest
/// first. A question whose answer record cannot be found is left out.
pub fn thread_history(spool: &Spool, context_id: &str, except: &str) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String, String, String)> =
        thread_records(spool, Dir::Done, context_id, except)
            .into_iter()
            .filter_map(|(id, rec, payload)| {
                let (_, _, question) = question_body(&id, &payload).ok()?;
                let answer_id = rec.meta.get("answer_id").and_then(Value::as_str)?;
                let answer = [Dir::Outbox, Dir::Done]
                    .into_iter()
                    .find_map(|d| spool.get(d, answer_id).ok().flatten())
                    .and_then(|a| match payload_of(answer_id, &a).ok()?.body {
                        Body::Answer { answer, .. } => Some(answer),
                        // Only an answer's words belong in the responder prompt's history.
                        Body::Question { .. }
                        | Body::Content { .. }
                        | Body::ContentReply { .. }
                        | Body::ToolCall { .. }
                        | Body::ToolReply { .. } => None,
                    })?;
                Some((rec.received_at.clone(), id, question.to_string(), answer))
            })
            .collect();
    // Chronological: `received_at` first, the (time-ordered UUIDv7) id as the tiebreak.
    pairs.sort();
    let skip = pairs.len().saturating_sub(THREAD_HISTORY_MAX);
    pairs
        .into_iter()
        .skip(skip)
        .map(|(_, _, q, a)| (q, a))
        .collect()
}

/// Runs the responder for question `rec` (§3.4 steps 1–3) and returns the record with the
/// draft stored on it (state `drafted`) plus the runner's verdict. Nothing is written: the
/// caller decides whether the drafted record goes back to the spool (`owl draft` always does;
/// the scheduler only when the status is `ok`). Runner errors (unknown project, unknown or
/// disabled harness, executable missing) leave `rec` untouched. The prompt carries the
/// asker's `context` and the thread's earlier exchanges from `done/` (§10).
pub fn draft(
    config: &Config,
    home: &Path,
    id: &str,
    mut rec: Record,
    harness: Option<&str>,
) -> anyhow::Result<(Record, runner::Draft)> {
    let payload = payload_of(id, &rec)?;
    let (project, path, question) = question_body(id, &payload)?;
    // The `Project:` line and the notes path carry the project unfenced: only one the owner
    // configured, as [`draft`] demands (the daemon refuses others; a record from before that
    // rule still stops here).
    anyhow::ensure!(
        config.projects.contains_key(project),
        "unknown project {project}"
    );
    let history = thread_history_of(home, id, &payload)?;
    let extras = runner::Extras {
        context: question_context(&payload),
        history: &history,
    };
    let d = runner::draft_with(config, home, harness, project, path, question, &extras)?;
    rec.draft = Some(StoredDraft::from_runner(&d).to_value());
    rec.state = "drafted".into();
    // A fresh draft supersedes whatever the scheduler failed on; the inbox stops nagging.
    meta_object(&mut rec).remove(AUTO_ERROR);
    events::push(
        &mut rec,
        "drafted",
        Some(&d.harness),
        Some(json!({ "redactions": d.redactions, "status": d.status.as_str() })),
    );
    Ok((rec, d))
}

/// The responder prompt for a question record — what [`draft`] hands the harness — for a
/// caller that answers it itself (`owl draft --prompt`, the plugin's Agent flow).
pub fn prompt(config: &Config, home: &Path, id: &str, rec: &Record) -> anyhow::Result<String> {
    let payload = payload_of(id, rec)?;
    let (project, path, question) = question_body(id, &payload)?;
    // The `Project:` line and the notes path carry the project unfenced: only one the owner
    // configured, as [`draft`] demands (the daemon refuses others; a record from before that
    // rule still stops here).
    anyhow::ensure!(
        config.projects.contains_key(project),
        "unknown project {project}"
    );
    let history = thread_history_of(home, id, &payload)?;
    let extras = runner::Extras {
        context: question_context(&payload),
        history: &history,
    };
    Ok(runner::build_prompt_with(
        config, home, project, path, question, &extras,
    ))
}

fn thread_history_of(
    home: &Path,
    id: &str,
    payload: &Payload,
) -> anyhow::Result<Vec<(String, String)>> {
    Ok(match payload.context_id.as_deref() {
        Some(cid) => thread_history(&Spool::new(home)?, cid, id),
        None => Vec::new(),
    })
}

/// Stores a human-written `text` as the draft (harness `human`, no redactions, status `ok`)
/// without running a harness; the record becomes `drafted` like [`draft`] leaves it.
pub fn draft_text(rec: Record, text: &str) -> Record {
    store_text(rec, text.to_string(), "human", 0)
}

/// Stores an in-session agent's answer as the draft: harness `agent`, redacted like a harness
/// answer (`responder.redact`), status `ok`.
pub fn draft_agent(config: &Config, rec: Record, text: &str) -> anyhow::Result<Record> {
    let (text, n) = runner::redact(&config.responder.redact, text)?;
    Ok(store_text(
        rec,
        text,
        "agent",
        u32::try_from(n).unwrap_or(u32::MAX),
    ))
}

fn store_text(mut rec: Record, text: String, harness: &str, redactions: u32) -> Record {
    let stored = StoredDraft {
        text,
        harness: harness.into(),
        redactions,
        status: "ok".into(),
        drafted_at: envelope::rfc3339_now(),
    };
    rec.draft = Some(stored.to_value());
    rec.state = "drafted".into();
    meta_object(&mut rec).remove(AUTO_ERROR);
    events::push(
        &mut rec,
        "drafted",
        Some(harness),
        Some(json!({ "redactions": redactions, "status": "ok" })),
    );
    rec
}

/// Who pressed send: the human (`owl send`) or the scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SendMode {
    Manual,
    Auto,
}

/// What `send` produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub answer_id: String,
    pub to: String,
    pub harness: String,
    pub redactions: u32,
    pub outbox: PathBuf,
}

/// One line of `log/outgoing.jsonl`. A struct rather than `json!` so the key order is the
/// documented one (`ts, to, question_id, harness, redactions, answer_sha256, mode`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutgoingLine {
    pub ts: String,
    pub to: String,
    pub question_id: String,
    pub harness: String,
    pub redactions: u32,
    /// Hex SHA-256 of the (redacted) answer text as sent.
    pub answer_sha256: String,
    pub mode: SendMode,
}

impl OutgoingLine {
    pub fn new(
        to: &str,
        question_id: &str,
        harness: &str,
        redactions: u32,
        answer_text: &str,
        mode: SendMode,
    ) -> OutgoingLine {
        OutgoingLine {
            ts: envelope::rfc3339_now(),
            to: to.to_string(),
            question_id: question_id.to_string(),
            harness: harness.to_string(),
            redactions,
            answer_sha256: hex(&Sha256::digest(answer_text.as_bytes())),
            mode,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Appends one line to `<home>/log/outgoing.jsonl`, creating `log/` on demand. `O_APPEND`, so
/// concurrent writers (the daemon and a CLI `owl send`) never interleave a line.
pub fn append_outgoing(home: &Path, line: &OutgoingLine) -> anyhow::Result<()> {
    let path = home.join(OUTGOING_LOG);
    let dir = path.parent().expect("OUTGOING_LOG has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut text = serde_json::to_string(line)?;
    text.push('\n');
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .with_context(|| format!("appending to {}", path.display()))
}

/// True when `log/outgoing.jsonl` already holds a line for `question_id`. A missing file is an
/// empty log; a line that is not JSON (a torn write, a hand edit) is skipped, not an error.
pub fn has_outgoing_line(home: &Path, question_id: &str) -> anyhow::Result<bool> {
    let path = home.join(OUTGOING_LOG);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    Ok(text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .any(|v| v.get("question_id").and_then(Value::as_str) == Some(question_id)))
}

/// §3.4 step 5 for a `drafted` question record: sign the answer, write it to `outbox/`
/// (state `unacked`) and the responder cache, log it to `outgoing.jsonl`, move the question to
/// `done/` (state `answered`, `meta.answer_id`).
///
/// Outbox and cache records share one shape — `{raw, sig, state, seen, received_at, draft,
/// meta}` with `meta = {peer, question_id, hash}` — so the daemon's `GET /v1/outbox` listing
/// and its cache-hit path read them unchanged.
///
/// Order: outbox + cache put → log line → finish. The log line goes in *before* the question
/// is finished so an answer can never end up served from the outbox yet absent from the log
/// (a finished question cannot be retried: the inbox record is gone). The step is not atomic:
/// a previous `send` may have spooled the envelope and then failed on the log or on `finish`.
/// The outbox record carries `meta.question_id`, so an existing envelope for this question is
/// reused rather than signed and spooled twice, and the log line is appended only if the log
/// has none for this question yet — a retry logs exactly once, never zero times.
pub fn send(
    home: &Path,
    spool: &Spool,
    id: &str,
    rec: Record,
    mode: SendMode,
) -> anyhow::Result<Sent> {
    let question = payload_of(id, &rec)?;
    if !question.kind.is_request() {
        bail!("record {id} is an answer, not a question — nothing to send");
    }
    let draft = StoredDraft::from_record(id, &rec)?;
    let draft = match (rec.state.as_str(), draft) {
        ("drafted", Some(d)) => d,
        (_, None) => bail!("record {id} has no draft — run `owl draft {id}` first"),
        (state, Some(_)) => {
            bail!("record {id} is in state {state}, not drafted — run `owl draft {id}` first")
        }
    };
    let identity = Identity::load(home).context("loading own key (run `owl init`)")?;
    let own = identity::fingerprint(&identity.verifying_key());
    if question.to != own {
        bail!(
            "record {id} is addressed to {}, not to this identity ({own})",
            question.to
        );
    }
    // Content is keyed by ref and by consent, not by question text — it has no
    // question hash, so no `meta.hash` on the outbox record and no cache entry either way.
    let hash = match question.kind {
        // A tool run has no question hash either — it is one run, never a cached
        // answer — so no `meta.hash` on the outbox record and no cache entry.
        Kind::Content | Kind::ToolCall => None,
        _ => {
            let (project, path, text) = question_body(id, &question)?;
            Some(envelope::question_hash(project, path, text))
        }
    };
    // A question with a context snippet, or one continuing a thread we already hold, is
    // answered for that thread only: it never feeds the responder cache.
    let cacheable = hash.is_some()
        && question_context(&question).is_none()
        && !question
            .context_id
            .as_deref()
            .is_some_and(|cid| thread_known(spool, cid, id));

    let (answer_id, answer_to) = match existing_answer(spool, id)? {
        Some((aid, ato)) => (aid, ato),
        None => {
            let answer = match question.kind {
                Kind::Content => {
                    let c = crate::content::stored(&rec).with_context(|| {
                        format!("record {id} has no content draft — run `owl draft {id}` first")
                    })?;
                    Payload::content_reply(
                        &question,
                        &c.text,
                        &c.sha256,
                        c.ref_resolved.as_deref(),
                        c.truncated,
                        c.redactions,
                    )
                }
                // The output of the one run, with its exit code and duration.
                Kind::ToolCall => {
                    let r = crate::tools::stored(&rec).with_context(|| {
                        format!("record {id} has no tool draft — run `owl draft {id}` first")
                    })?;
                    Payload::tool_reply(
                        &question,
                        &r.output,
                        r.exit_code,
                        r.duration_ms,
                        r.truncated,
                        r.redactions,
                    )
                }
                _ => Payload::answer(
                    &question,
                    &draft.text,
                    &draft.harness,
                    draft.redactions,
                    false,
                ),
            };
            let env = Envelope::sign(&answer, &identity);
            let mut meta = json!({ "peer": question.from, "question_id": id });
            if let Some(h) = &hash {
                meta["hash"] = json!(h);
            }
            let out = Record {
                raw: env.raw,
                sig: env.sig,
                state: "unacked".into(),
                seen: false,
                received_at: envelope::rfc3339_now(),
                draft: None,
                meta,
            };
            spool.put(Dir::Outbox, &answer.id, &out)?;
            if let (true, Some(h)) = (cacheable, &hash) {
                spool.cache_put(h, &out)?;
            }
            (answer.id, answer.to)
        }
    };
    if !has_outgoing_line(home, id)? {
        append_outgoing(
            home,
            &OutgoingLine::new(
                &answer_to,
                id,
                &draft.harness,
                draft.redactions,
                &draft.text,
                mode,
            ),
        )?;
    }
    finish(
        spool,
        id,
        rec,
        "answered",
        &[("answer_id", json!(answer_id))],
        Ev {
            // The content timeline names its own kinds.
            kind: match question.kind {
                Kind::Content => "content-sent",
                Kind::ToolCall => "tool-sent",
                _ => "sent",
            },
            by: Some(match mode {
                SendMode::Manual => "human",
                SendMode::Auto => "auto",
            }),
            detail: Some(json!({ "answer_id": answer_id })),
        },
    )?;
    Ok(Sent {
        outbox: spool.path(Dir::Outbox, &answer_id),
        answer_id,
        to: answer_to,
        harness: draft.harness,
        redactions: draft.redactions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(draft: Value) -> Record {
        Record {
            raw: String::new(),
            sig: String::new(),
            state: "drafted".into(),
            seen: false,
            received_at: "2026-09-01T10:00:00Z".into(),
            draft: Some(draft),
            meta: Value::Null,
        }
    }

    #[test]
    fn stored_draft_round_trips_and_rejects_malformed() {
        let d = StoredDraft {
            text: "t".into(),
            harness: "fake".into(),
            redactions: 1,
            status: "ok".into(),
            drafted_at: "2026-09-01T10:00:00Z".into(),
        };
        let back = StoredDraft::from_record("x", &rec(d.to_value()))
            .unwrap()
            .unwrap();
        assert_eq!(back, d);
        assert!(
            StoredDraft::from_record("x", &rec(Value::Null))
                .unwrap()
                .is_none()
        );
        let mut r = rec(Value::Null);
        r.draft = None;
        assert!(StoredDraft::from_record("x", &r).unwrap().is_none());
        for bad in [
            json!({"harness": "fake", "redactions": 0, "status": "ok", "drafted_at": "x"}),
            json!({"text": "t", "harness": "fake", "redactions": "1", "status": "ok", "drafted_at": "x"}),
            json!({"text": 5, "harness": "fake", "redactions": 0, "status": "ok", "drafted_at": "x"}),
            json!("just a string"),
            json!([]),
        ] {
            let err = StoredDraft::from_record("x", &rec(bad.clone()))
                .unwrap_err()
                .to_string();
            assert!(err.contains("record x: draft is malformed"), "{bad}: {err}");
        }
    }

    /// Every non-object `meta` shape becomes an empty object; an object is kept as is.
    #[test]
    fn meta_object_repairs_non_objects_and_keeps_objects() {
        for bad in [
            Value::Null,
            json!("oops"),
            json!([1]),
            json!(7),
            json!(true),
        ] {
            let mut r = rec(Value::Null);
            r.meta = bad.clone();
            meta_object(&mut r).insert("k".into(), json!(1));
            assert_eq!(r.meta, json!({ "k": 1 }), "{bad}");
            assert_eq!(auto_error(&r), None);
        }
        let mut r = rec(Value::Null);
        r.meta = json!({ "peer": "p", "hash": "h" });
        meta_object(&mut r).insert("k".into(), json!(1));
        assert_eq!(r.meta, json!({ "peer": "p", "hash": "h", "k": 1 }));
        r.meta = json!({ "auto_error": "unknown project p" });
        assert_eq!(auto_error(&r), Some("unknown project p"));
        r.meta = json!({ "auto_error": 7 });
        assert_eq!(auto_error(&r), None, "non-string auto_error is ignored");
    }

    /// The documented key order, the `mode` spelling and a real SHA-256 of the text.
    #[test]
    fn outgoing_line_shape() {
        let line = OutgoingLine::new("fp-b", "q-1", "fake", 1, "abc", SendMode::Auto);
        let text = serde_json::to_string(&line).unwrap();
        // serde_json's Map sorts keys; check the raw text for the documented order.
        let order = [
            "\"ts\"",
            "\"to\"",
            "\"question_id\"",
            "\"harness\"",
            "\"redactions\"",
            "\"answer_sha256\"",
            "\"mode\"",
        ];
        let positions: Vec<usize> = order.iter().map(|k| text.find(k).unwrap()).collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{text}");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["to"], "fp-b");
        assert_eq!(v["question_id"], "q-1");
        assert_eq!(v["harness"], "fake");
        assert_eq!(v["redactions"], 1);
        assert_eq!(v["mode"], "auto");
        assert_eq!(
            v["answer_sha256"],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(envelope::parse_rfc3339_to_unix(v["ts"].as_str().unwrap()).is_some());
        let manual = OutgoingLine::new("fp-b", "q-1", "fake", 0, "", SendMode::Manual);
        let v: Value = serde_json::to_value(&manual).unwrap();
        assert_eq!(v["mode"], "manual");
        assert_eq!(
            v["answer_sha256"],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// `log/` is created on demand and lines are appended, never truncated.
    #[test]
    fn append_outgoing_creates_dir_and_appends() {
        let home = tempfile::tempdir().unwrap();
        assert!(!home.path().join("log").exists());
        let a = OutgoingLine::new("b", "q1", "fake", 0, "x", SendMode::Manual);
        let b = OutgoingLine::new("b", "q2", "fake", 2, "y", SendMode::Auto);
        append_outgoing(home.path(), &a).unwrap();
        append_outgoing(home.path(), &b).unwrap();
        let body = std::fs::read_to_string(home.path().join(OUTGOING_LOG)).unwrap();
        let lines: Vec<Value> = body
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2, "{body}");
        assert_eq!(lines[0]["question_id"], "q1");
        assert_eq!(lines[0]["mode"], "manual");
        assert_eq!(lines[1]["question_id"], "q2");
        assert_eq!(lines[1]["mode"], "auto");
        assert!(body.ends_with('\n'));
        // `log` blocked by a file: the append fails with a path in the message, no panic.
        let blocked = tempfile::tempdir().unwrap();
        std::fs::write(blocked.path().join("log"), "not a dir").unwrap();
        let err = format!("{:#}", append_outgoing(blocked.path(), &a).unwrap_err());
        assert!(err.contains("log"), "{err}");
    }

    #[test]
    fn has_outgoing_line_tolerates_missing_file_and_garbage() {
        let home = tempfile::tempdir().unwrap();
        assert!(
            !has_outgoing_line(home.path(), "q1").unwrap(),
            "no file yet"
        );
        let dir = home.path().join("log");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("outgoing.jsonl"),
            "not json\n{\"question_id\":\"q1\"}\n{\"question_id\":7}\n[1]\n",
        )
        .unwrap();
        assert!(has_outgoing_line(home.path(), "q1").unwrap());
        assert!(!has_outgoing_line(home.path(), "q2").unwrap());
        assert!(!has_outgoing_line(home.path(), "7").unwrap());
        // Unreadable file (a directory in its place): an error naming the path, not `false`.
        let blocked = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(blocked.path().join(OUTGOING_LOG)).unwrap();
        let err = format!("{:#}", has_outgoing_line(blocked.path(), "q1").unwrap_err());
        assert!(err.contains("outgoing.jsonl"), "{err}");
    }

    #[test]
    fn question_body_rejects_answers() {
        let q = Payload::question("a", "b", "p", Some("f"), "why?");
        assert_eq!(question_body("x", &q).unwrap(), ("p", Some("f"), "why?"));
        let a = Payload::answer(&q, "because", "fake", 0, false);
        let err = question_body("x", &a).unwrap_err().to_string();
        assert!(err.contains("record x is an answer"), "{err}");
    }
}
