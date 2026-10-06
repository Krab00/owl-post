//! `owl archive` / `owl unarchive` / `owl delete` / `owl undo`: hiding a chat or one
//! thread from `owl thread`, and a trash that keeps the deleted records for undo.
//!
//! Archiving only flips marks in `$OWLPOST_HOME/archive.json` (`{"chats": [<fp>, ...],
//! "threads": [[<fp>, <context_id>], ...]}`); the records stay where they are and `owl thread`
//! filters them out. Every save first keeps a still-parsing current file as
//! `archive.json.bak`, and a corrupt `archive.json` falls back to that backup with a warning.
//! Deleting moves the record files to `trash/<batch>/` (a time-ordered UUIDv7 per call, so the
//! newest batch sorts last) after writing `batch.json` first, and `owl undo` walks the batches
//! back newest first, refusing to overwrite a record that exists again. Nothing here ever
//! removes a record: undo cleans up with `remove_file`/`remove_dir` only, so a file that was
//! not moved back can never be deleted. Only the daemon's trash GC
//! (`owlpost::daemon::gc_trash`) removes a batch, once it is older than 30 days.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context;
use owlpost::daemon::write_atomic;
use owlpost::spool::{Dir, Spool};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{ExitError, contact_book, peer_name, print_json, thread, user_error};

const FILE: &str = "archive.json";
const BAK: &str = "archive.json.bak";

/// The `archive.json` shape: whole chats by fingerprint, threads by (fingerprint, context id).
/// Both are sorted sets so the file is stable.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Archive {
    pub chats: BTreeSet<String>,
    pub threads: BTreeSet<(String, String)>,
}

/// Loads the archive marks. A missing file means nothing is archived (no warning); a file
/// that does not parse falls back to `archive.json.bak` — used with a warning when it parses,
/// otherwise the archive counts as empty, also with a warning. Only an I/O error other than
/// NotFound on the file itself is an error.
pub fn load(home: &Path) -> anyhow::Result<Archive> {
    let path = home.join(FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Archive::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    match serde_json::from_slice::<Archive>(&bytes) {
        Ok(marks) => Ok(marks),
        Err(err) => match std::fs::read(home.join(BAK)).ok().and_then(|b| parse(&b)) {
            Some(marks) => {
                eprintln!(
                    "warning: {} is corrupt ({err}); using {BAK}",
                    path.display()
                );
                Ok(marks)
            }
            None => {
                eprintln!(
                    "warning: {} is corrupt ({err}) and there is no usable backup; nothing is archived",
                    path.display()
                );
                Ok(Archive::default())
            }
        },
    }
}

/// Parses the bytes of an `archive.json`; `None` means corrupt.
fn parse(bytes: &[u8]) -> Option<Archive> {
    serde_json::from_slice(bytes).ok()
}

impl Archive {
    /// Writes the marks atomically; a current `archive.json` that still parses is first kept
    /// as `archive.json.bak`, so a good backup is never replaced by corrupt content.
    fn save(&self, home: &Path) -> anyhow::Result<()> {
        if let Ok(bytes) = std::fs::read(home.join(FILE))
            && parse(&bytes).is_some()
        {
            write_atomic(home, BAK, &bytes)?;
        }
        write_atomic(home, FILE, &serde_json::to_vec_pretty(self)?)
    }
}

/// What `run` does with the target: hide it, bring it back, or move it to the trash.
pub enum Action {
    Archive,
    Unarchive,
    Delete,
}

pub fn run(
    home: &Path,
    peer: &str,
    context: Option<&str>,
    action: Action,
    json: bool,
) -> anyhow::Result<()> {
    // Unarchive only flips a mark: the records may be gone (`owl delete` keeps the mark), so
    // it resolves the peer and never looks at the spool.
    if let Action::Unarchive = action {
        let book = contact_book(home)?;
        let contact = book.resolve(peer).map_err(|e| user_error(e.to_string()))?;
        return mark(
            home,
            &contact.fingerprint,
            &contact.name,
            context,
            false,
            json,
        );
    }
    let spool = Spool::new(home)?;
    let target = targets(home, &spool, peer, context)?;
    let (fingerprint, name) = (target.fingerprint.as_str(), target.name.as_str());
    match action {
        Action::Archive => mark(home, fingerprint, name, context, true, json),
        Action::Delete => delete(
            home,
            &spool,
            fingerprint,
            name,
            context,
            &target.records,
            json,
        ),
        Action::Unarchive => unreachable!("handled above"),
    }
}

/// The records one archive/delete call acts on: who they belong to and which
/// (directory, id) pairs matched.
struct Target {
    fingerprint: String,
    name: String,
    records: Vec<(Dir, String)>,
}

/// Resolves `<peer>` like `owl thread` does: `no conversation with <name>` when the peer has
/// no record at all, `no thread <id> with <name>` when `--context` names none of them.
fn targets(
    home: &Path,
    spool: &Spool,
    peer: &str,
    context: Option<&str>,
) -> anyhow::Result<Target> {
    let book = contact_book(home)?;
    let contact = book.resolve(peer).map_err(|e| user_error(e.to_string()))?;
    let (fingerprint, name) = (contact.fingerprint.clone(), contact.name.clone());
    let own = thread::own_fingerprint(home);
    let mut conversation = false;
    let mut records = Vec::new();
    for (dir, id, _rec, payload, peer_fp) in thread::records(spool, own.as_deref()) {
        if peer_fp != fingerprint {
            continue;
        }
        conversation = true;
        if context.is_none() || payload.context_id.as_deref() == context {
            records.push((dir, id));
        }
    }
    if !conversation {
        return Err(user_error(format!("no conversation with {name}")));
    }
    if let Some(c) = context
        && records.is_empty()
    {
        return Err(user_error(format!("no thread {c} with {name}")));
    }
    Ok(Target {
        fingerprint,
        name,
        records,
    })
}

/// `Anna` for a whole chat, `thread <id> with Anna` for one thread of it.
fn target_str(name: &str, context: Option<&str>) -> String {
    match context {
        Some(c) => format!("thread {c} with {name}"),
        None => name.to_string(),
    }
}

/// Flips one archive mark. Archiving is idempotent; unarchiving something not archived is a
/// user error.
fn mark(
    home: &Path,
    fingerprint: &str,
    name: &str,
    context: Option<&str>,
    archive: bool,
    json: bool,
) -> anyhow::Result<()> {
    let mut marks = load(home)?;
    let on = match (context, archive) {
        (Some(c), true) => marks
            .threads
            .insert((fingerprint.to_string(), c.to_string())),
        (None, true) => marks.chats.insert(fingerprint.to_string()),
        (Some(c), false) => marks
            .threads
            .remove(&(fingerprint.to_string(), c.to_string())),
        (None, false) => marks.chats.remove(fingerprint),
    };
    if !archive && !on {
        return Err(user_error(format!(
            "{} is not archived",
            target_str(name, context)
        )));
    }
    marks.save(home)?;
    if json {
        print_json(&json!({
            "peer": fingerprint,
            "context_id": context,
            "archived": archive,
        }))?;
    } else {
        let verb = if archive { "archived" } else { "unarchived" };
        println!("{verb} {}", target_str(name, context));
    }
    Ok(())
}

/// Moves the target records to `trash/<batch>/`, byte-identical by rename; `batch.json` lands
/// before the first move, so a stopped run still undoes what already moved.
fn delete(
    home: &Path,
    spool: &Spool,
    fingerprint: &str,
    name: &str,
    context: Option<&str>,
    picked: &[(Dir, String)],
    json: bool,
) -> anyhow::Result<()> {
    let batch = uuid::Uuid::now_v7().to_string();
    let dir = home.join("trash").join(&batch);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let meta = serde_json::to_vec_pretty(&json!({
        "peer": fingerprint,
        "context_id": context,
    }))?;
    let meta_path = dir.join("batch.json");
    std::fs::write(&meta_path, meta).with_context(|| format!("writing {}", meta_path.display()))?;
    for (d, id) in picked {
        let sub = dir.join(d.name());
        std::fs::create_dir_all(&sub).with_context(|| format!("creating {}", sub.display()))?;
        let dst = sub.join(format!("{id}.json"));
        std::fs::rename(spool.path(*d, id), &dst)
            .with_context(|| format!("moving to {}", dst.display()))?;
    }
    let n = picked.len();
    if json {
        print_json(&json!({
            "peer": fingerprint,
            "context_id": context,
            "records": n,
        }))?;
    } else {
        println!(
            "deleted {n} record{} of {} — owl undo restores them",
            if n == 1 { "" } else { "s" },
            target_str(name, context),
        );
    }
    Ok(())
}

/// The `batch.json` of one trash batch: who the records belonged to and, for a thread delete,
/// which thread.
#[derive(Debug, Deserialize)]
struct Batch {
    peer: String,
    context_id: Option<String>,
}

/// Restores the newest trash batch with a readable `batch.json` (the largest name — UUIDv7
/// sorts by time), newest first on repeated calls. A dir without one (a crash before it was
/// written, or the leftover of an earlier undo) is skipped and never touched. Refuses the
/// whole undo, and keeps the whole batch in trash, when any record's id names a record in the
/// spool again (any dir, any letter case); the batch dir is removed only
/// after every file is back, and a stray file that keeps the dir is left in place.
pub fn undo(home: &Path, json: bool) -> anyhow::Result<()> {
    let spool = Spool::new(home)?;
    let trash = home.join("trash");
    let mut batches: Vec<String> = match std::fs::read_dir(&trash) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("listing {}", trash.display())),
    };
    batches.sort();
    let (batch, meta) = loop {
        let Some(batch) = batches.pop() else {
            return Err(ExitError::error(4, "nothing to undo"));
        };
        let meta = std::fs::read(trash.join(&batch).join("batch.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Batch>(&bytes).ok());
        if let Some(meta) = meta {
            break (batch, meta);
        }
    };
    let dir = trash.join(&batch);
    let meta_path = dir.join("batch.json");

    // Every record the batch holds, sorted so a refusal names a deterministic file.
    let mut moved: Vec<(Dir, String)> = Vec::new();
    for d in thread::DIRS {
        let Ok(rd) = std::fs::read_dir(dir.join(d.name())) else {
            continue;
        };
        for e in rd {
            let path = e?.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            moved.push((d, id.to_string()));
        }
    }
    moved.sort_by(|a, b| (a.0.name(), &a.1).cmp(&(b.0.name(), &b.1)));

    // Nothing moves while any id is taken again — in any record dir, case variants included:
    // a peer may have reused a deleted record's id meanwhile, and a restored copy
    // next to its record would be overwritten by the next `owl reject`/`send` of it.
    for (d, id) in &moved {
        if let Some(taken) = spool.same_id(id)?.first() {
            let own = format!("{}/{id}.json", d.name());
            let found = taken
                .strip_prefix(spool.home().join("spool"))
                .unwrap_or(taken);
            let found = found.to_string_lossy();
            return Err(user_error(if found == own {
                format!("cannot undo: {own} exists again")
            } else {
                format!("cannot undo: {own} — its id is taken again by {found}")
            }));
        }
    }
    for (d, id) in &moved {
        let dst = spool.path(*d, id);
        std::fs::rename(dir.join(d.name()).join(format!("{id}.json")), &dst)
            .with_context(|| format!("moving to {}", dst.display()))?;
    }
    // Cleanup is best-effort: a stray non-record file keeps its dir (batch.json is gone, so
    // the next undo skips the dir and walks on to the older batch). Nothing is ever unlinked
    // but the meta file and only empty dirs are removed.
    let _ = std::fs::remove_file(&meta_path);
    for d in thread::DIRS {
        let sub = dir.join(d.name());
        if sub.exists() {
            let _ = std::fs::remove_dir(&sub);
        }
    }
    let _ = std::fs::remove_dir(&dir);

    let name = peer_name(&contact_book(home)?, &meta.peer);
    let n = moved.len();
    if json {
        print_json(&json!({
            "peer": meta.peer,
            "context_id": meta.context_id,
            "records": n,
        }))?;
    } else {
        println!(
            "restored {n} record{} of {}",
            if n == 1 { "" } else { "s" },
            target_str(&name, meta.context_id.as_deref()),
        );
    }
    Ok(())
}
