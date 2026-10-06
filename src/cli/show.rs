//! `owl show <id|all> [--format plain|claude|codex|kimi]`: the full question or answer, the
//! draft when present; marks seen (§9). `--format claude|codex|kimi` prints the Markdown
//! message table instead (`owlpost::render`): a question as a one-column
//! table, on a `consent` record with the fingerprint in the header; a drafted one followed by
//! `draft:`, the draft in a plain text block, `harness: <name>` and the language note when the
//! draft's language differs from the question's; an answer as the same table, project and path
//! taken from the question it replies to. The wake's instruction line (`owlpost::route`) is
//! never printed here. `--json` wins over `--format`. A received content reply carries its
//! digest verdict in every mode: the `sha256 … — verified` / `sha256 mismatch …`
//! line in plain and `--format claude|codex|kimi`, `sha256_verified` under `--json`, and a
//! mismatch exits 1 whichever mode printed it. Every shown record is released from the
//! session wake routing (`owlpost::route`).

use std::path::Path;

use anyhow::Context;
use owlpost::config::Config;
use owlpost::content;
use owlpost::envelope::{self, Body, Kind};
use owlpost::render;
use owlpost::route;
use owlpost::spool::{Dir, Record, Spool};
use owlpost::tools;
use serde_json::{Value, json};

use super::inbox::Format;
use super::{StoredDraft, kind_str, payload_of, peer_name, print_json, summary};

pub fn run(home: &Path, id: &str, json: bool, format: Option<&str>) -> anyhow::Result<()> {
    let format = format.map(Format::parse).transpose()?;
    // `--json` wins: the json branch below is checked first.
    let rendered = matches!(format, Some(Format::Claude | Format::Codex | Format::Kimi));
    let spool = Spool::new(home)?;
    let config = Config::load(home)?;
    let book = super::contact_book(home)?;
    let records: Vec<(String, Record)> = if id == "all" {
        spool.list(Dir::Inbox, |_| true)?
    } else {
        let rec = spool
            .get(Dir::Inbox, id)?
            .with_context(|| format!("no inbox record {id}"))?;
        vec![(id.to_string(), rec)]
    };
    let now = envelope::now_unix();
    let mut out = Vec::new();
    // A received reply whose content does not match its digest is shown in full and
    // then exits 1 — the human sees what arrived and that it cannot be trusted.
    let mut mismatch = false;
    for (i, (id, rec)) in records.iter().enumerate() {
        let payload = payload_of(id, rec)?;
        let draft = StoredDraft::from_record(id, rec)?;
        // The digest verdict is computed once, before the format branch — every
        // mode says the same thing about the same bytes, and a mismatch exits 1 in all three.
        let verdict = match &payload.body {
            Body::ContentReply {
                content,
                sha256,
                truncated,
                ..
            } => Some(content::verify_line(content, sha256, *truncated)),
            _ => None,
        };
        if let Some((_, false)) = &verdict {
            mismatch = true;
        }
        if json {
            let mut v = summary(id, rec, &payload, &book, now);
            v["payload"] = serde_json::to_value(&payload)?;
            v["draft"] = draft.as_ref().map_or(Value::Null, StoredDraft::to_value);
            if let Some((_, ok)) = &verdict {
                v["sha256_verified"] = json!(ok);
            }
            out.push(v);
        } else if rendered {
            if i > 0 {
                println!();
            }
            println!(
                "{}",
                render::record_block(&spool, &book, rec, &payload, draft.as_ref())
            );
            if let Some((line, _)) = &verdict {
                println!("{line}");
            }
        } else {
            if i > 0 {
                println!();
            }
            println!("id:       {id}");
            println!(
                "from:     {} ({})",
                peer_name(&book, &payload.from),
                payload.from
            );
            println!("type:     {}", kind_str(payload.kind));
            println!("state:    {}", rec.state);
            println!("received: {}", rec.received_at);
            match &payload.body {
                Body::Question {
                    project,
                    path,
                    question,
                    context,
                } => {
                    println!("project:  {project}");
                    println!("path:     {}", path.as_deref().unwrap_or("-"));
                    println!("question:");
                    println!("{question}");
                    // The asker's snippet after the question, when it sent one.
                    if let Some(ctx) = context {
                        println!("context:");
                        println!("{ctx}");
                    }
                }
                Body::Answer {
                    answer,
                    harness,
                    redactions,
                    cached,
                } => {
                    if let Some(q) = &payload.in_reply_to {
                        println!("reply to: {q}");
                    }
                    println!("harness:  {harness}");
                    println!("redactions: {redactions}");
                    println!("cached:   {cached}");
                    println!("answer:");
                    println!("{answer}");
                }
                // The request the peer sent, so the human sees exactly what was
                // asked for before anything is read from disk.
                Body::Content {
                    project,
                    git_ref,
                    path,
                    memory,
                } => {
                    println!("project:  {}", project.as_deref().unwrap_or("-"));
                    println!("ref:      {}", git_ref.as_deref().unwrap_or("HEAD"));
                    println!("path:     {}", path.as_deref().unwrap_or("-"));
                    if let Some(key) = memory {
                        println!("memory:   {key}");
                    }
                }
                // What would run, before anything runs: the tool, the argv it
                // resolves to, the directory it would run in and the input in full.
                Body::ToolCall {
                    tool,
                    input,
                    project,
                } => {
                    println!("tool:     {tool}");
                    match tools::lookup(&config, tool) {
                        Ok(t) => {
                            println!("argv:     {}", t.argv.join(" "));
                            println!(
                                "cwd:      {}",
                                tools::cwd_of(&config, home, t)
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_else(|e| e.to_string())
                            );
                        }
                        // The owner removed the tool since the request arrived: say so
                        // rather than printing an argv this machine would not run.
                        Err(e) => println!("argv:     {e}"),
                    }
                    println!("project:  {}", project.as_deref().unwrap_or("-"));
                    println!("input:");
                    println!(
                        "{}",
                        serde_json::to_string_pretty(input).unwrap_or_else(|_| "{}".into())
                    );
                }
                // A reply we received — the run's line, then its output.
                Body::ToolReply {
                    output,
                    exit_code,
                    duration_ms,
                    truncated,
                    redactions,
                    harness,
                } => {
                    if let Some(q) = &payload.in_reply_to {
                        println!("reply to: {q}");
                    }
                    println!("harness:  {harness}");
                    println!("redactions: {redactions}");
                    println!(
                        "{}",
                        tools::Run::show_line(*exit_code, *duration_ms, output.len())
                    );
                    println!("output:");
                    println!("{}", render::fenced_text(&tools::display(output)));
                    if *truncated {
                        println!("truncated: {} bytes received", output.len());
                    }
                }
                // A reply we received — the content, then the digest verdict.
                Body::ContentReply {
                    content,
                    sha256,
                    ref_resolved,
                    truncated: _,
                    redactions,
                    harness,
                } => {
                    if let Some(q) = &payload.in_reply_to {
                        println!("reply to: {q}");
                    }
                    println!("harness:  {harness}");
                    println!("redactions: {redactions}");
                    if let Some(r) = ref_resolved {
                        println!("ref:      {r}");
                    }
                    println!("content:");
                    println!(
                        "{}",
                        render::fenced_text(&content::display(content, sha256))
                    );
                    let (line, _) = verdict.as_ref().expect("a content reply has a verdict");
                    println!("{line}");
                }
            }
            // Our own drafted content goes in a plain text block under
            // `content:`, never as the generic draft dump — the human reads the exact bytes
            // (or, above the display cap, the byte count and the digest) before `owl send`.
            if let Some(r) = tools::stored(rec).filter(|_| payload.kind == Kind::ToolCall) {
                // Our own run, once drafted — the line and the exact output the
                // human reads before `owl send`.
                println!("{}", r.draft_line());
                println!("output:");
                println!("{}", render::fenced_text(&tools::display(&r.output)));
            } else if let Some(c) = content::stored(rec).filter(|_| payload.kind == Kind::Content) {
                println!("content:");
                println!(
                    "{}",
                    render::fenced_text(&content::display(&c.text, &c.sha256))
                );
                println!("sha256:   {}", c.sha256);
                if c.truncated {
                    println!("truncated: {} of {} bytes", c.text.len(), c.full_bytes);
                }
            } else if let Some(d) = &draft {
                println!(
                    "draft ({} via {}, redactions: {}, {}):",
                    d.status, d.harness, d.redactions, d.drafted_at
                );
                println!("{}", d.text);
            }
        }
        if !rec.seen {
            spool.mark_seen(Dir::Inbox, id)?;
        }
        // Shown: no session needs to wake for it.
        route::release(home, id);
    }
    if json {
        print_json(&if id == "all" {
            Value::Array(out)
        } else {
            out.into_iter().next().unwrap_or(json!(null))
        })?;
    }
    if mismatch {
        return Err(super::ExitError::error(
            1,
            "sha256 mismatch — the content does not match its digest",
        ));
    }
    Ok(())
}
