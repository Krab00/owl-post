//! `owl presence` and `owl ping` (§9): who is online.
//!
//! * `owl presence [--json]` — read-only: the daemon's `daemon.status` (rewritten after every
//!   pull loop) against the contact book. One row per contact, in the book's order; a contact
//!   the daemon never probed shows `null` / `-`. No status file yet is exit 4.
//! * `owl ping <peer> [--json]` — probes one peer right now with the very call the pull loop
//!   uses (`GET /v1/outbox`, read-only on the peer; the result is discarded). Online is exit
//!   0, offline prints the result and exits 2 (as `owl card` does). The result is NOT written
//!   into `daemon.status`: the daemon owns that file.

use std::path::Path;

use owlpost::client::{self, Iroh};
use owlpost::contacts::ContactBook;
use owlpost::envelope;
use owlpost::pull::{self, PullStatus};
use serde_json::{Value, json};

use super::{ExitError, print_json, print_table, user_error};

/// `owl presence [--json]`.
pub fn presence(home: &Path, json: bool) -> anyhow::Result<()> {
    let status = pull::read_status(home)?.ok_or_else(|| {
        ExitError::error(
            4,
            "no daemon status yet (daemon.status missing) — is the daemon running? see owl install",
        )
    })?;
    let book = super::contact_book(home)?;
    if json {
        return print_json(&presence_json(&book, &status));
    }
    let now = envelope::now_unix();
    println!(
        "last pull {} ago · {} open ask(s) · {} peer(s) probed",
        super::format_age(super::age_secs(&status.last_pull_at, now)),
        status.open_asks,
        status.peers_probed,
    );
    let rows: Vec<Vec<String>> = book
        .contacts
        .iter()
        .map(|c| match status.peers.get(&c.fingerprint) {
            Some(p) => vec![
                c.name.clone(),
                if p.online { "online" } else { "offline" }.to_string(),
                format!(
                    "{} ago",
                    super::format_age(super::age_secs(&p.probed_at, now))
                ),
                p.last_seen.clone().unwrap_or_else(|| "-".to_string()),
            ],
            None => vec![
                c.name.clone(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
            ],
        })
        .collect();
    print_table(&["PEER", "STATUS", "PROBED", "LAST SEEN"], &rows);
    Ok(())
}

/// The `--json` object: the summary plus one row per contact of the book, in the book's
/// order. Probe entries for fingerprints that are not contacts are not listed.
fn presence_json(book: &ContactBook, status: &PullStatus) -> Value {
    let peers: Vec<Value> = book
        .contacts
        .iter()
        .map(|c| match status.peers.get(&c.fingerprint) {
            Some(p) => json!({
                "fingerprint": c.fingerprint,
                "name": c.name,
                "online": p.online,
                "probed_at": p.probed_at,
                "last_seen": p.last_seen,
            }),
            None => json!({
                "fingerprint": c.fingerprint,
                "name": c.name,
                "online": null,
                "probed_at": null,
                "last_seen": null,
            }),
        })
        .collect();
    json!({
        "last_pull_at": status.last_pull_at,
        "open_asks": status.open_asks,
        "peers_probed": status.peers_probed,
        "peers": peers,
    })
}

/// `owl ping <peer> [--json]`.
pub fn ping(home: &Path, peer: &str, json: bool) -> anyhow::Result<()> {
    let book = super::contact_book(home)?;
    let contact = book.resolve(peer).map_err(|e| user_error(e.to_string()))?;
    let (_cfg, identity) = crate::require_identity(home)?;
    let probed = client::fetch_outbox(&identity, contact, &Iroh::from_home(home));
    let error = probed.err().map(|e| format!("{e:#}"));
    let online = error.is_none();
    if json {
        let mut v = json!({
            "fingerprint": contact.fingerprint,
            "name": contact.name,
            "online": online,
            "probed_at": envelope::rfc3339_now(),
        });
        if let Some(e) = &error {
            v.as_object_mut()
                .expect("ping row is an object")
                .insert("error".into(), json!(e));
        }
        print_json(&v)?;
    } else {
        match &error {
            None => println!("{} is online", contact.name),
            // The client error already leads with "offline: "; do not print it twice (the
            // JSON `error` keeps it).
            Some(e) => println!(
                "{} is offline: {}",
                contact.name,
                e.strip_prefix("offline: ").unwrap_or(e)
            ),
        }
    }
    match error {
        None => Ok(()),
        Some(_) => Err(ExitError::error(2, format!("offline: {}", contact.name))),
    }
}
