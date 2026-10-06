//! `owl edit <id>`: open the stored draft in `$EDITOR`, store the result (§3.4 step 4).
//!
//! The draft goes to `<home>/tmp/draft-<id>.md`; `$EDITOR` runs through `sh -c` so a value with
//! arguments (`code --wait`) works. A non-zero editor exit leaves the draft untouched.

use std::path::Path;
use std::process::Command;

use anyhow::Context;
use owlpost::envelope;
use owlpost::route;
use owlpost::spool::{Dir, Spool};
use serde_json::json;

use super::{StoredDraft, existing_answer, inbox_record, payload_of, print_json, user_error};

pub fn run(home: &Path, id: &str, json: bool) -> anyhow::Result<()> {
    let spool = Spool::new(home)?;
    let mut rec = inbox_record(&spool, id)?;
    // Editing a content draft would change the bytes while `draft.sha256` — the
    // digest `owl send` signs — kept describing the file, and the asker's `owl show` would
    // report a tamper. Content is the file or it is nothing.
    if payload_of(id, &rec)?.kind == owlpost::envelope::Kind::Content {
        return Err(user_error(format!(
            "record {id} is a content request — its draft is the file itself and cannot be edited; `owl reject {id}` declines it"
        )));
    }
    // The same rule for a run. The draft is what the tool printed, next to the
    // exit code it printed it with; an edited output would be a claim about a run that
    // never happened.
    if payload_of(id, &rec)?.kind == owlpost::envelope::Kind::ToolCall {
        return Err(user_error(format!(
            "record {id} is a tool-call request — its draft is what the tool printed and cannot be edited; `owl reject {id}` declines it"
        )));
    }
    let mut draft = StoredDraft::from_record(id, &rec)?.ok_or_else(|| {
        user_error(format!(
            "record {id} has no draft — run `owl draft {id}` first"
        ))
    })?;
    if let Some((aid, _)) = existing_answer(&spool, id)? {
        return Err(user_error(format!(
            "record {id} already has its answer spooled as outbox/{aid}.json — run `owl send {id}` to finish it (the draft is not editable any more)"
        )));
    }
    let editor = std::env::var("EDITOR")
        .ok()
        .filter(|e| !e.trim().is_empty())
        .ok_or_else(|| user_error("EDITOR is not set — export EDITOR=<your editor> and retry"))?;

    let dir = home.join("tmp");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let file = dir.join(format!("draft-{id}.md"));
    std::fs::write(&file, &draft.text).with_context(|| format!("writing {}", file.display()))?;
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("owl-edit")
        .arg(&file)
        .status()
        .with_context(|| format!("running EDITOR {editor:?}"));
    let result = status.and_then(|s| {
        if !s.success() {
            anyhow::bail!("EDITOR {editor:?} exited with {s}; draft left unchanged");
        }
        std::fs::read_to_string(&file).with_context(|| format!("reading back {}", file.display()))
    });
    let _ = std::fs::remove_file(&file);
    let text = result?;
    let text = text.trim_end_matches(['\n', '\r']).to_string();
    if text.trim().is_empty() {
        return Err(user_error(format!(
            "edited draft for {id} is empty; draft left unchanged"
        )));
    }
    draft.text = text;
    draft.status = "edited".into();
    let mut v = draft.to_value();
    if let Some(m) = v.as_object_mut() {
        m.insert("edited_at".into(), json!(envelope::rfc3339_now()));
    }
    rec.draft = Some(v);
    owlpost::events::push(&mut rec, "edited", Some("human"), None);
    spool.put(Dir::Inbox, id, &rec)?;
    // Being handled: no session needs to wake for it.
    route::release(home, id);
    if json {
        print_json(&json!({ "id": id, "state": rec.state, "draft": rec.draft }))?;
    } else {
        println!("{}", draft.text);
        println!("draft updated ({id})");
    }
    Ok(())
}
