//! `owl status [<id>]` (§9): where every open ask stands. For each `asks/` record
//! (or the one id) the peer's A2A Task (`GET /v1/questions/{id}`) is fetched and its text
//! printed in a table `ID  PEER  PATH  STATE  SINCE`; a peer that cannot be reached prints
//! `offline`, one that holds no record `not found`. `--json` prints the Task objects (an
//! `{"id", "peer", "error"}` object for an ask without one). A `REJECTED` Task moves the ask
//! to `done/` as `declined`. Exit 0 always; exit 4 `no open questions` when nothing is open.

use std::collections::BTreeMap;
use std::path::Path;

use owlpost::client::{self, Iroh};
use owlpost::pull;
use owlpost::spool::{Dir, Spool};
use serde_json::{Value, json};

use super::{ExitError, age_secs, format_age, peer_name, print_json, print_table};

pub fn run(home: &Path, id: Option<&str>, json: bool) -> anyhow::Result<()> {
    let (_cfg, identity) = crate::require_identity(home)?;
    let book = super::contact_book(home)?;
    let spool = Spool::new(home)?;
    let iroh = Iroh::from_home(home);
    let asks: Vec<_> = spool
        .list_lenient(Dir::Asks)?
        .into_iter()
        .filter(|(aid, _)| id.is_none_or(|want| want == aid))
        .filter_map(|(aid, rec)| pull::open_ask(&aid, &rec).map(|ask| (ask, rec)))
        .collect();
    if asks.is_empty() {
        return Err(ExitError::error(
            4,
            match id {
                Some(id) => format!("no open question {id}"),
                None => "no open questions".to_string(),
            },
        ));
    }
    let now = owlpost::envelope::now_unix();
    // One reachability verdict per peer: an offline peer is not dialed once per ask.
    let mut offline: BTreeMap<String, String> = BTreeMap::new();
    let mut rows = Vec::new();
    let mut tasks = Vec::new();
    for (ask, rec) in asks {
        let name = peer_name(&book, &ask.peer);
        let contact = book.contacts.iter().find(|c| c.fingerprint == ask.peer);
        let (state, task): (String, Option<Value>) = match contact {
            None => ("unknown contact".into(), None),
            Some(_) if offline.contains_key(&ask.peer) => ("offline".into(), None),
            Some(contact) => match client::fetch_task(&identity, contact, &iroh, &ask.id) {
                Ok(Some(task)) => {
                    pull::note_task_state(&spool, &ask.id, &task);
                    if task.rejected() {
                        pull::close_declined(&spool, &ask.id)?;
                    }
                    (task.text, Some(task.value))
                }
                Ok(None) => ("not found".into(), None),
                Err(e) => {
                    let why = format!("{e:#}");
                    if why.starts_with("offline") {
                        offline.insert(ask.peer.clone(), why);
                        ("offline".into(), None)
                    } else {
                        (why, None)
                    }
                }
            },
        };
        tasks.push(
            task.unwrap_or_else(|| json!({ "id": ask.id, "peer": ask.peer, "error": state })),
        );
        rows.push(vec![
            ask.id.clone(),
            name,
            ask.path.clone(),
            state,
            format_age(age_secs(&rec.received_at, now)),
        ]);
    }
    if json {
        print_json(&Value::Array(tasks))?;
    } else {
        print_table(&["ID", "PEER", "PATH", "STATE", "SINCE"], &rows);
    }
    Ok(())
}
