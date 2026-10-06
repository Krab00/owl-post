//! `owl request <peer> <project> <path> [--ref <ref>] [--reply-to <id>]` and
//! `owl request <peer> --memory <key> [--reply-to <id>]` (§9): ask a peer for one
//! file at a ref of a named project, or for one entry of their memory store.
//!
//! The send path is `owl ask`'s, minus everything a question needs and content does not:
//! no asker cache (content is keyed by ref and by the owner's consent, not by question
//! text), no `git blame` peer proposal, and no `--wait` — a human reads a file and approves
//! it by hand, so the wait is human-scale and `owl status` already reports where it stands.

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

const USAGE: &str =
    "usage: owl request <peer> <project> <path> [--ref <ref>] | owl request <peer> --memory <key>";

#[derive(Debug, Args)]
pub struct RequestArgs {
    /// `<peer> <project> <path>`; with `--memory` only `<peer>`
    #[arg(value_names = ["PEER", "[PROJECT]", "[PATH]"])]
    pub args: Vec<String>,
    /// Ref to read the file at (default: the owner's current branch)
    #[arg(long = "ref", value_name = "REF")]
    pub git_ref: Option<String>,
    /// Ask for this entry of the peer's memory store instead of a file
    #[arg(long, value_name = "KEY")]
    pub memory: Option<String>,
    /// Continue an earlier exchange with this peer: reuse its thread id
    #[arg(long, value_name = "ID")]
    pub reply_to: Option<String>,
}

/// `(peer, project, path)` for a file request, or `(peer, None, None)` with `--memory`.
#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    pub peer: String,
    pub project: Option<String>,
    pub path: Option<String>,
}

/// With `--memory` exactly one positional is expected, without it exactly three.
pub fn parse_positionals(args: &[String], memory: Option<&str>) -> anyhow::Result<Parsed> {
    let mut it = args.iter().cloned();
    let peer = it
        .next()
        .with_context(|| format!("missing <peer>\n{USAGE}"))?;
    let rest: Vec<String> = it.collect();
    if memory.is_some() {
        if let Some(extra) = rest.first() {
            anyhow::bail!("unexpected argument {extra:?}\n{USAGE}");
        }
        return Ok(Parsed {
            peer,
            project: None,
            path: None,
        });
    }
    let [project, path] = rest.as_slice() else {
        anyhow::bail!("missing <project> <path>\n{USAGE}");
    };
    if project.trim().is_empty() || path.trim().is_empty() {
        anyhow::bail!("project and path must not be empty\n{USAGE}");
    }
    Ok(Parsed {
        peer,
        project: Some(project.clone()),
        path: Some(path.clone()),
    })
}

pub fn run(home: &Path, args: RequestArgs, json: bool) -> anyhow::Result<()> {
    let (_cfg, identity) = crate::require_identity(home)?;
    let cwd = std::env::current_dir().context("reading the current directory")?;
    let book = ContactBook::load(home, &cwd)?;
    let parsed = parse_positionals(&args.args, args.memory.as_deref())?;
    let contact: Contact = book.resolve(&parsed.peer)?.clone();
    let spool = Spool::new(home)?;
    // A thread continues with `--reply-to`, else starts here.
    let context_id = match &args.reply_to {
        Some(rid) => super::ask::thread_of(&spool, &book, rid, &contact)?,
        None => uuid::Uuid::now_v7().to_string(),
    };
    let own = identity::fingerprint(&identity.verifying_key());
    let mut payload = Payload::content(
        &own,
        &contact.fingerprint,
        parsed.project.as_deref(),
        args.git_ref.as_deref(),
        parsed.path.as_deref(),
        args.memory.as_deref(),
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
        // No `hash`: a content request has none (see `pull::open_ask`).
        meta: json!({ "peer": contact.fingerprint, "threaded": true }),
    };
    // Record birth.
    owlpost::events::push(&mut asked, "content-requested", Some("human"), None);
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
        // A responder that answers a content request straight from a cache does not exist:
        // every one is held for consent, so an inline reply here is a protocol violation.
        SendOutcome::Answer(hit) => Err(ExitError::error(
            1,
            format!(
                "{} replied to a content request inline ({}); every content request is held for consent",
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

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn positionals_take_three_for_a_file_and_one_for_a_memory_key() {
        assert_eq!(
            parse_positionals(&v(&["martin", "p", "src/a.rs"]), None).unwrap(),
            Parsed {
                peer: "martin".into(),
                project: Some("p".into()),
                path: Some("src/a.rs".into()),
            }
        );
        assert_eq!(
            parse_positionals(&v(&["martin"]), Some("notes/a.md")).unwrap(),
            Parsed {
                peer: "martin".into(),
                project: None,
                path: None,
            }
        );
    }

    /// Every way of getting the positionals wrong is a usage error, never a silent default.
    #[test]
    fn positionals_refuse_the_wrong_count_and_empty_parts() {
        assert!(parse_positionals(&v(&[]), None).is_err(), "no peer");
        assert!(parse_positionals(&v(&["martin"]), None).is_err(), "no path");
        assert!(
            parse_positionals(&v(&["martin", "p"]), None).is_err(),
            "project without path"
        );
        assert!(
            parse_positionals(&v(&["martin", "p", "a", "b"]), None).is_err(),
            "one too many"
        );
        assert!(
            parse_positionals(&v(&["martin", "p", " "]), None).is_err(),
            "empty path"
        );
        assert!(
            parse_positionals(&v(&["martin", " ", "a"]), None).is_err(),
            "empty project"
        );
        assert!(
            parse_positionals(&v(&["martin", "p"]), Some("k")).is_err(),
            "--memory takes no path"
        );
    }
}
