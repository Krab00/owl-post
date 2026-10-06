//! `owl draft <id> [--harness <name> | --text <text> [--agent] | --prompt]`: run the
//! runner through the shared `owlpost::answer::draft`, store the draft on the inbox record
//! (state `drafted`), print it with a redaction summary (§3.4, §10). `--text` stores the
//! human's own answer as the draft instead (harness `human`, no redactions); `--text --agent`
//! stores an in-session agent's answer (harness `agent`, redacted like a harness answer).
//! `--prompt` prints the responder prompt for the record and stops — the plugin hands it to
//! the Agent tool and brings the answer back with `--text --agent`.
//!
//! State gate (§8): `pending` and `drafted` (re-draft) are accepted; `consent` is refused
//! with a pointer to `owl allow`; anything else is refused. A `drafted` record whose signed
//! answer already sits in `outbox/` (a `send` that failed after spooling the envelope) is
//! refused too, pointing at `owl send`: re-drafting would replace the draft while the retry
//! ships the old envelope — the same `existing_answer` guard `owl edit` applies. Runner status `timeout` /
//! `extract_failed`: the draft is still stored and printed, a warning goes to stderr and the
//! command exits 1 so a caller can tell the draft needs a look before `owl send`.

use std::path::Path;

use owlpost::answer;
use owlpost::config::Config;
use owlpost::content;
use owlpost::envelope::Kind;
use owlpost::route;
use owlpost::runner::DraftStatus;
use owlpost::spool::{Dir, Spool};
use owlpost::tools;
use serde_json::json;

use super::{
    ExitError, StoredDraft, existing_answer, inbox_record, payload_of, print_json, user_error,
};

pub struct Opts<'a> {
    pub harness: Option<&'a str>,
    pub text: Option<String>,
    pub agent: bool,
    pub prompt: bool,
}

pub fn run(home: &Path, id: &str, opts: Opts<'_>, json: bool) -> anyhow::Result<()> {
    let Opts {
        harness,
        text,
        agent,
        prompt,
    } = opts;
    let config = Config::load(home)?;
    let spool = Spool::new(home)?;
    let rec = inbox_record(&spool, id)?;
    let payload = payload_of(id, &rec)?;
    match payload.kind {
        Kind::Question => {}
        // A content request is drafted with no harness and no model, so every flag
        // that picks or replaces one is a usage error rather than a silently ignored word.
        Kind::Content => {
            if harness.is_some() || text.is_some() || agent || prompt {
                return Err(user_error(format!(
                    "record {id} is a content request — run owl draft {id} with no flags"
                )));
            }
        }
        // The same refusal, for the same reason — the tool is the answer, so a
        // flag that picks a harness or replaces the text has nothing to act on.
        Kind::ToolCall => {
            if harness.is_some() || text.is_some() || agent || prompt {
                return Err(user_error(format!(
                    "record {id} is a tool-call request — run owl draft {id} with no flags"
                )));
            }
        }
        Kind::Answer | Kind::ContentReply | Kind::ToolReply => {
            return Err(user_error(format!(
                "record {id} is an answer, not a question — nothing to draft"
            )));
        }
    }
    match rec.state.as_str() {
        "pending" | "drafted" => {}
        "consent" => {
            return Err(user_error(format!(
                "record {id} is held for consent — run `owl allow {}` first",
                payload.from
            )));
        }
        other => {
            return Err(user_error(format!(
                "record {id} is in state {other}; only pending or drafted records can be drafted"
            )));
        }
    }
    if let Some((aid, _)) = existing_answer(&spool, id)? {
        return Err(user_error(format!(
            "record {id} already has its answer spooled as outbox/{aid}.json — run `owl send {id}` to finish it (drafting again would not change what is sent)"
        )));
    }
    if payload.kind == Kind::Content {
        return draft_content(&config, &spool, home, id, rec, &payload, json);
    }
    if payload.kind == Kind::ToolCall {
        return draft_tool(&config, &spool, home, id, rec, &payload, json);
    }
    if prompt {
        if json {
            print_json(&json!({"id": id, "prompt": answer::prompt(&config, home, id, &rec)?}))?;
        } else {
            print!("{}", answer::prompt(&config, home, id, &rec)?);
        }
        return Ok(());
    }
    // `--text`: the human (or, with `--agent`, the session's agent) wrote the answer; no
    // harness runs.
    let (rec, status) = match text {
        Some(text) if agent => (answer::draft_agent(&config, rec, &text)?, DraftStatus::Ok),
        Some(text) => (answer::draft_text(rec, &text), DraftStatus::Ok),
        None => {
            let (rec, draft) = answer::draft(&config, home, id, rec, harness)?;
            (rec, draft.status)
        }
    };
    let stored = StoredDraft::from_record(id, &rec)?.expect("a drafted record has a draft");
    spool.put(Dir::Inbox, id, &rec)?;
    // Being handled: no session needs to wake for it.
    route::release(home, id);

    if json {
        let mut v = rec.draft.clone().unwrap_or_default();
        if let Some(obj) = v.as_object_mut() {
            obj.insert("id".into(), json!(id));
            obj.insert("state".into(), json!("drafted"));
        }
        print_json(&v)?;
    } else {
        println!("{}", stored.text);
        println!("harness: {}", stored.harness);
        println!("redactions: {}", stored.redactions);
        println!("state: drafted ({id})");
    }
    match status {
        DraftStatus::Ok => Ok(()),
        status => Err(ExitError {
            code: 1,
            message: format!(
                "draft for {id} stored with status {}; review it with `owl edit {id}` before `owl send {id}`",
                status.as_str()
            ),
        }
        .into()),
    }
}

/// `owl draft <id>` on a content record: resolve the ref, read the one object the
/// peer named, refuse anything that is not text, redact, cut, store. No harness, no model.
fn draft_content(
    config: &Config,
    spool: &Spool,
    home: &Path,
    id: &str,
    mut rec: owlpost::spool::Record,
    payload: &owlpost::envelope::Payload,
    json: bool,
) -> anyhow::Result<()> {
    let request = content::request_of(id, payload).map_err(|e| user_error(e.to_string()))?;
    let resolved = content::resolve(config, &request).map_err(|e| user_error(format!("{e:#}")))?;
    let label = match &request {
        content::Request::Path { path, .. } => (*path).to_string(),
        content::Request::Memory { key } => format!("memory:{key}"),
    };
    rec.state = "drafted".into();
    rec.draft = Some(resolved.to_draft());
    owlpost::events::push(
        &mut rec,
        "content-drafted",
        Some("human"),
        Some(json!({
            "bytes": resolved.text.len(),
            "redactions": resolved.redactions,
            "truncated": resolved.truncated,
        })),
    );
    spool.put(Dir::Inbox, id, &rec)?;
    // Being handled: no session needs to wake for it.
    route::release(home, id);
    if json {
        let mut v = rec.draft.clone().unwrap_or_default();
        if let Some(obj) = v.as_object_mut() {
            obj.insert("id".into(), json!(id));
            obj.insert("state".into(), json!("drafted"));
        }
        print_json(&v)?;
    } else {
        println!("{}", resolved.draft_line(&label));
        println!("state: drafted ({id})");
    }
    Ok(())
}

/// `owl draft <id>` on a tool-call record: the **only** place the owner's machine
/// ever spawns a peer-named tool. The record is already allowed by hand (the `consent` gate
/// above), the registry is the allowlist, the input goes to the child on stdin and the
/// redacted, cut output is stored as the draft.
///
/// A non-zero exit is not a CLI failure: a failing build is a legitimate answer and the
/// human decides whether to send it. Only a tool that could not be started at all is exit 1.
fn draft_tool(
    config: &Config,
    spool: &Spool,
    home: &Path,
    id: &str,
    mut rec: owlpost::spool::Record,
    payload: &owlpost::envelope::Payload,
    json: bool,
) -> anyhow::Result<()> {
    let request = tools::request_of(id, payload).map_err(|e| user_error(e.to_string()))?;
    let tool = tools::lookup(config, request.tool).map_err(|e| user_error(e.to_string()))?;
    let run = tools::run(
        config,
        home,
        request.tool,
        tool,
        request.input,
        &payload.from,
    )
    .map_err(|e| user_error(format!("{e:#}")))?;
    rec.state = "drafted".into();
    rec.draft = Some(run.to_draft());
    owlpost::events::push(
        &mut rec,
        "tool-run",
        Some("human"),
        Some(json!({
            "tool": run.tool,
            "exit_code": run.exit_code,
            "duration_ms": run.duration_ms,
        })),
    );
    spool.put(Dir::Inbox, id, &rec)?;
    // Being handled: no session needs to wake for it.
    route::release(home, id);
    if json {
        let mut v = rec.draft.clone().unwrap_or_default();
        if let Some(obj) = v.as_object_mut() {
            obj.insert("id".into(), json!(id));
            obj.insert("state".into(), json!("drafted"));
        }
        print_json(&v)?;
    } else {
        println!("{}", run.draft_line());
        println!("{}", run.output);
        println!("state: drafted ({id})");
    }
    Ok(())
}
