//! `owl reject <id>`: discard an inbox record — moved to `done/` with state `rejected` (§8).

use std::path::Path;

use owlpost::events::Ev;
use owlpost::spool::Spool;
use serde_json::json;

use super::{finish, inbox_record, print_json};

pub fn run(home: &Path, id: &str, json: bool) -> anyhow::Result<()> {
    let spool = Spool::new(home)?;
    let rec = inbox_record(&spool, id)?;
    let previous = json!(rec.state);
    finish(
        &spool,
        id,
        rec,
        "rejected",
        &[("previous_state", previous)],
        Ev::by("rejected", "human"),
    )?;
    if json {
        print_json(&json!({ "id": id, "state": "rejected" }))?;
    } else {
        println!("rejected {id}");
    }
    Ok(())
}
