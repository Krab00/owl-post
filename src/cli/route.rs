//! `owl route <id>`: route one inbox record to one live session (`owlpost::route`),
//! the same call the daemon makes when a record is born — for scripts and the e2e test, no
//! daemon needed. Prints `routed <id> -> <session id>` or `no live session for <id>` (exit 0
//! both ways; `--json`: `{"id", "session"}` with `null` for none); an unknown record is a user
//! error (exit 1).

use std::path::Path;

use owlpost::config::Config;
use owlpost::route;
use owlpost::spool::Spool;
use serde_json::json;

use super::{inbox_record, print_json};

pub fn run(home: &Path, id: &str, json: bool) -> anyhow::Result<()> {
    let config = Config::load(home)?;
    let spool = Spool::new(home)?;
    inbox_record(&spool, id)?;
    let session = route::route(home, &config, &spool, id)?;
    if json {
        print_json(&json!({ "id": id, "session": session }))?;
    } else if let Some(sid) = session {
        println!("routed {id} -> {sid}");
    } else {
        println!("no live session for {id}");
    }
    Ok(())
}
