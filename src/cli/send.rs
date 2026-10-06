//! `owl send <id>`: the human presses send — `owlpost::answer::send` in `manual` mode signs
//! the answer, writes it to `outbox/` (state `unacked`), logs it to `log/outgoing.jsonl`
//! (`mode: "manual"`), moves the question to `done/` (state `answered`) and updates the
//! responder cache (§3.4 step 5, §8). The daemon's auto-accept uses the very same function.

use std::path::Path;

use owlpost::answer::{self, SendMode};
use owlpost::spool::Spool;
use serde_json::json;

use super::{inbox_record, print_json};

pub fn run(home: &Path, id: &str, json: bool) -> anyhow::Result<()> {
    let spool = Spool::new(home)?;
    let rec = inbox_record(&spool, id)?;
    let sent = answer::send(home, &spool, id, rec, SendMode::Manual)?;
    if json {
        print_json(&json!({
            "id": sent.answer_id,
            "in_reply_to": id,
            "to": sent.to,
            "outbox": sent.outbox,
            "redactions": sent.redactions,
            "harness": sent.harness,
        }))?;
    } else {
        println!("sent {} (reply to {id}, to {})", sent.answer_id, sent.to);
    }
    Ok(())
}
