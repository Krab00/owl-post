//! `owl call <peer> <tool> --input <file|-> [--project <id>] [--reply-to <id>]` (§9):
//! ask a peer to run one tool of **their** registry and send back what it printed.
//!
//! The send path is `owl request`'s: no asker cache, no `--wait`. Every tool-call request is
//! held for the owner's consent and the tool runs only when they type `owl draft`, so the
//! wait is human-scale and `owl status` already reports where it stands.

use std::path::Path;

use anyhow::Context;
use clap::Args;
use owlpost::client::{self, Iroh, SendOutcome};
use owlpost::contacts::{Contact, ContactBook};
use owlpost::envelope::{self, Envelope, Payload};
use owlpost::identity;
use owlpost::spool::{Record, Spool};
use serde_json::json;

use super::ExitError;

#[derive(Debug, Args)]
pub struct CallArgs {
    /// The peer whose tool to run
    pub peer: String,
    /// The tool's name in the peer's registry
    pub tool: String,
    /// JSON object to send the tool on its stdin (`-` reads stdin)
    #[arg(long, value_name = "FILE")]
    pub input: String,
    /// The peer project the tool should run in, when their registry leaves it open
    #[arg(long, value_name = "ID")]
    pub project: Option<String>,
    /// Continue an earlier exchange with this peer: reuse its thread id
    #[arg(long, value_name = "ID")]
    pub reply_to: Option<String>,
}

/// The input as a JSON **object**. A bare string, an array or a number is a usage error: the
/// tool reads one object from its stdin and nothing else is meaningful to it.
fn read_input(source: &str) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    let text = if source == "-" {
        std::io::read_to_string(std::io::stdin()).context("reading the input from stdin")?
    } else {
        std::fs::read_to_string(source).with_context(|| format!("reading {source}"))?
    };
    match serde_json::from_str(&text) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        _ => Err(ExitError::error(1, "input must be a JSON object")),
    }
}

pub fn run(home: &Path, args: CallArgs, json: bool) -> anyhow::Result<()> {
    let (_cfg, identity) = crate::require_identity(home)?;
    let cwd = std::env::current_dir().context("reading the current directory")?;
    let book = ContactBook::load(home, &cwd)?;
    let contact: Contact = book.resolve(&args.peer)?.clone();
    let input = read_input(&args.input)?;
    let spool = Spool::new(home)?;
    // A thread continues with `--reply-to`, else starts here.
    let context_id = match &args.reply_to {
        Some(rid) => super::ask::thread_of(&spool, &book, rid, &contact)?,
        None => uuid::Uuid::now_v7().to_string(),
    };
    let own = identity::fingerprint(&identity.verifying_key());
    let mut payload = Payload::tool_call(
        &own,
        &contact.fingerprint,
        &args.tool,
        input,
        args.project.as_deref(),
    );
    payload.context_id = Some(context_id);
    let envelope = Envelope::sign(&payload, &identity);
    let iroh = Iroh::from_home(home);
    let mut asked = Record {
        raw: envelope.raw.clone(),
        sig: envelope.sig.clone(),
        state: "waiting".into(),
        seen: false,
        received_at: envelope::rfc3339_now(),
        draft: None,
        // No `hash`: a tool call has none (see `pull::open_ask`).
        meta: json!({ "peer": contact.fingerprint, "threaded": true }),
    };
    // Record birth.
    owlpost::events::push(&mut asked, "tool-requested", Some("human"), None);
    // Recorded before it is sent (see `ask::Reserved`).
    let reserved = super::ask::Reserved::new(&spool, &payload.id, &asked)?;
    match client::send_question(&identity, &contact, &iroh, &envelope)? {
        SendOutcome::Accepted { id, state } => {
            reserved.keep();
            if json {
                println!(
                    "{}",
                    json!({ "status": "accepted", "id": id, "state": state })
                );
            } else {
                println!("accepted {id} — waiting for the owner's consent");
            }
            Ok(())
        }
        // Every tool call is held for consent, so an inline reply is a protocol violation.
        SendOutcome::Answer(hit) => Err(ExitError::error(
            1,
            format!(
                "{} replied to a tool call inline ({}); every tool call is held for consent",
                contact.name, hit.payload.id
            ),
        )),
        SendOutcome::Unavailable => Err(ExitError::error(
            2,
            format!(
                "unavailable: {} does not answer questions (policy never or responder disabled)",
                contact.name
            ),
        )),
        SendOutcome::RateLimited { retry_after_secs } => Err(ExitError::error(
            3,
            match retry_after_secs {
                Some(s) => format!("rate limited, retry after {s}s"),
                None => "rate limited".to_string(),
            },
        )),
        SendOutcome::Offline { errors } => Err(ExitError::error(
            2,
            format!(
                "offline: no endpoint of {} reachable ({})",
                contact.name,
                errors.join("; ")
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a JSON object is an input; every other shape is a usage error, and so is a file
    /// that does not parse at all.
    #[test]
    fn input_must_be_a_json_object() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, text).unwrap();
            p.display().to_string()
        };
        let ok = read_input(&write("ok.json", r#"{"package":"auth"}"#)).unwrap();
        assert_eq!(ok["package"], json!("auth"));
        assert!(read_input(&write("empty.json", "{}")).unwrap().is_empty());
        for (name, text) in [
            ("s.json", "\"hello\""),
            ("a.json", "[1,2]"),
            ("n.json", "7"),
            ("null.json", "null"),
            ("bad.json", "{nope"),
        ] {
            let err = read_input(&write(name, text)).unwrap_err().to_string();
            assert!(err.contains("input must be a JSON object"), "{name}: {err}");
        }
    }
}
