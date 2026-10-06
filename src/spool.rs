//! Spool state machine (§8): `inbox/ outbox/ asks/ done/ cache/` under `$OWLPOST_HOME/spool`,
//! one JSON record per id, atomic writes (temp file + rename).

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Inbox,
    Outbox,
    Asks,
    Done,
    Cache,
}

impl Dir {
    pub const ALL: [Dir; 5] = [Dir::Inbox, Dir::Outbox, Dir::Asks, Dir::Done, Dir::Cache];

    pub fn name(self) -> &'static str {
        match self {
            Dir::Inbox => "inbox",
            Dir::Outbox => "outbox",
            Dir::Asks => "asks",
            Dir::Done => "done",
            Dir::Cache => "cache",
        }
    }
}

/// Stored shape from §6 plus `seen` and `meta`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub raw: String,
    pub sig: String,
    pub state: String,
    #[serde(default)]
    pub seen: bool,
    pub received_at: String,
    #[serde(default)]
    pub draft: Option<serde_json::Value>,
    #[serde(default)]
    pub meta: serde_json::Value,
}

/// The longest id a peer may pick. owl's own ids are 36-character UUIDs; a longer
/// peer id is malformed rather than a file name the filesystem refuses.
pub const MAX_ID_LEN: usize = 128;

/// Whether a peer-chosen id may name a record file: 1 to `MAX_ID_LEN` ASCII
/// alphanumerics and `-`, not starting with `-` (a `--help` file looks like a flag). Anything
/// else (`/`, `..`, `.`) could name a path the record scans never see.
pub fn is_record_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && !id.starts_with('-')
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// `Spool::put_new` refused because the id is taken.
pub fn is_taken(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists)
}

pub struct Spool {
    root: PathBuf,
}

impl Spool {
    /// Creates `$home/spool/{inbox,outbox,asks,done,cache}`.
    pub fn new(home: &Path) -> anyhow::Result<Spool> {
        let root = home.join("spool");
        for d in Dir::ALL {
            let p = root.join(d.name());
            std::fs::create_dir_all(&p).with_context(|| format!("creating {}", p.display()))?;
        }
        Ok(Spool { root })
    }

    /// The `$OWLPOST_HOME` this spool lives under (`spool/`'s parent).
    pub fn home(&self) -> &Path {
        self.root.parent().unwrap_or(&self.root)
    }

    pub fn path(&self, dir: Dir, id: &str) -> PathBuf {
        self.root.join(dir.name()).join(format!("{id}.json"))
    }

    /// Writes `<id>.json.tmp` then renames over `<id>.json`.
    pub fn put(&self, dir: Dir, id: &str, rec: &Record) -> anyhow::Result<()> {
        let path = self.path(dir, id);
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(rec)?;
        std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("renaming to {}", path.display()));
        }
        Ok(())
    }

    /// `put` for a record that must not exist yet. Refused with an `AlreadyExists`
    /// error (`is_taken`) when `same_id` finds the id in a record dir, or when `<id>.json`
    /// exists by the time it is published: the temp file is hard-linked into place, which never
    /// replaces a file (on a case-insensitive filesystem not a case variant either). The check
    /// and the link run under one process-wide lock, so the daemon's server and pull loop
    /// cannot both pass the check for one id; another process is held off by the link alone.
    /// The temp name is unique per call, so two writers never share one.
    pub fn put_new(&self, dir: Dir, id: &str, rec: &Record) -> anyhow::Result<()> {
        static PUBLISH: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = PUBLISH.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.path(dir, id);
        if !self.same_id(id)?.is_empty() {
            return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
                .with_context(|| format!("publishing {}: the id names a record", path.display()));
        }
        let tmp = path.with_extension(format!("json.{}.tmp", uuid::Uuid::now_v7().simple()));
        let json = serde_json::to_vec_pretty(rec)?;
        std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        let linked = std::fs::hard_link(&tmp, &path);
        let _ = std::fs::remove_file(&tmp);
        linked.with_context(|| format!("publishing {}", path.display()))
    }

    /// Missing → None. Malformed → error.
    pub fn get(&self, dir: Dir, id: &str) -> anyhow::Result<Option<Record>> {
        let path = self.path(dir, id);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// The files in inbox/, outbox/, asks/ and done/ whose record id equals `id`
    /// ignoring ASCII case. A peer-chosen id must not replace or shadow a record, and on a
    /// case-insensitive filesystem a case variant names the same file — so both spellings
    /// count. (`cache/` is keyed by question hash, not a peer-chosen id; `<id>.json.tmp`
    /// files are interrupted writes, not records.)
    pub fn same_id(&self, id: &str) -> anyhow::Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for dir in [Dir::Inbox, Dir::Outbox, Dir::Asks, Dir::Done] {
            let dir_path = self.root.join(dir.name());
            for entry in std::fs::read_dir(&dir_path)
                .with_context(|| format!("listing {}", dir_path.display()))?
            {
                let path = entry?.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|stem| stem.eq_ignore_ascii_case(id))
                {
                    out.push(path);
                }
            }
        }
        Ok(out)
    }

    /// All `*.json` records in `dir` passing `filter`, sorted by id.
    pub fn list(
        &self,
        dir: Dir,
        filter: impl Fn(&Record) -> bool,
    ) -> anyhow::Result<Vec<(String, Record)>> {
        let dir_path = self.root.join(dir.name());
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir_path)
            .with_context(|| format!("listing {}", dir_path.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Some(rec) = self.get(dir, id)?
                && filter(&rec)
            {
                out.push((id.to_string(), rec));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// Like `list`, but a corrupt record is skipped with a warning instead of failing the
    /// whole listing (one bad file must not stall the outbox or the pull loop).
    pub fn list_lenient(&self, dir: Dir) -> anyhow::Result<Vec<(String, Record)>> {
        let dir_path = self.root.join(dir.name());
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir_path)
            .with_context(|| format!("listing {}", dir_path.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            match self.get(dir, id) {
                Ok(Some(rec)) => out.push((id.to_string(), rec)),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %format!("{e:#}"), "skipping corrupt record")
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    fn update(&self, dir: Dir, id: &str, f: impl FnOnce(&mut Record)) -> anyhow::Result<()> {
        let mut rec = self
            .get(dir, id)?
            .with_context(|| format!("no record {id} in {}", dir.name()))?;
        f(&mut rec);
        self.put(dir, id, &rec)
    }

    pub fn set_state(&self, dir: Dir, id: &str, state: &str) -> anyhow::Result<()> {
        self.update(dir, id, |r| r.state = state.to_string())
    }

    /// `set_state` plus one `meta.events` entry, in a single read-modify-write:
    /// the state change and the log line it explains can never come apart.
    pub fn set_state_with_event(
        &self,
        dir: Dir,
        id: &str,
        state: &str,
        kind: &str,
        by: Option<&str>,
        detail: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        self.update(dir, id, |r| {
            r.state = state.to_string();
            crate::events::push(r, kind, by, detail);
        })
    }

    /// Appends one `meta.events` entry without touching the state: what a
    /// `state-seen` poll records on an open ask.
    pub fn push_event(
        &self,
        dir: Dir,
        id: &str,
        kind: &str,
        by: Option<&str>,
        detail: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        self.update(dir, id, |r| crate::events::push(r, kind, by, detail))
    }

    pub fn mark_seen(&self, dir: Dir, id: &str) -> anyhow::Result<()> {
        self.update(dir, id, |r| r.seen = true)
    }

    /// Moves `<id>.json` between directories (same filesystem, so a rename).
    pub fn move_to(&self, from: Dir, id: &str, to: Dir) -> anyhow::Result<()> {
        let src = self.path(from, id);
        let dst = self.path(to, id);
        std::fs::rename(&src, &dst)
            .with_context(|| format!("moving {} to {}", src.display(), dst.display()))
    }

    pub fn count_unseen(&self, dir: Dir) -> anyhow::Result<usize> {
        Ok(self.list(dir, |r| !r.seen)?.len())
    }

    pub fn cache_get(&self, hash: &str) -> anyhow::Result<Option<Record>> {
        self.get(Dir::Cache, hash)
    }

    pub fn cache_put(&self, hash: &str, rec: &Record) -> anyhow::Result<()> {
        self.put(Dir::Cache, hash, rec)
    }
}
