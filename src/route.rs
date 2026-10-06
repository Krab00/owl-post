//! One session wakes per record (§8, §9, §11): every interactive Claude Code
//! session owns a private wake directory, the daemon routes each new inbox record to exactly
//! one live session by writing the record's metadata (never its content) into that
//! directory, a lease moves the record on when the session does not handle it, and handling
//! releases it.
//!
//! Layout under `$OWLPOST_HOME`:
//!
//! ```text
//! sessions/<session_id>/marker.json        {session_id, cwd, started_at, heartbeat_at, source}
//! sessions/<session_id>/wake/<record_id>.md the text the FileChanged hook prints (watched)
//! sessions/<session_id>/tmp/                staging for the atomic rename into wake/
//! spool/routing/<record_id>.json            {current: <session_id>|null, routed_at, tried: [..]}
//! ```
//!
//! `marker.json` lives outside `wake/` so heartbeat writes never produce `FileChanged`
//! events. Session ids follow [`valid_session_id`] (`[A-Za-z0-9._-]{1,128}`), so a path built
//! from one never leaves `sessions/`.
//!
//! Routing (`route`): candidates are the live markers not yet in the record's `tried` list,
//! ordered by (1) `cwd` equal to the configured checkout of the record's project
//! (`config.projects`, both canonicalised; a record whose project has no mapping skips this
//! rule), (2) newest `heartbeat_at`, (3) session id. The wake file is written to `tmp/` and
//! renamed into `wake/` (one `add` for the watcher), the routing records `current`,
//! `routed_at` and appends the session to `tried`; no candidate leaves `current` null and
//! writes nothing (the next `SessionStart` assigns the backlog).
//!
//! Liveness (`is_live`): the marker exists; when `~/.claude/sessions/*.json` names the session
//! id, one of those pids must be alive (a dead pid is dead regardless of the heartbeat); and
//! `heartbeat_at` is younger than `OWLPOST_SESSION_STALE_SECS` (default 21600).
//!
//! Lease (`lease_tick`, every 30 s in the daemon): a routing whose record left `inbox/` is
//! released; a seen record is left alone (a human looked at it); a `current` whose marker died
//! is re-routed at once; a `current` older than `OWLPOST_WAKE_LEASE_SECS` (default 600) has its
//! wake file removed and is re-routed (the session is already in `tried`); a routing with no
//! `current` is offered to any live session not yet tried.
//!
//! Release (`release`): `owl show`, `owl draft`, `owl edit`, the `owl inbox` listing and
//! `answer::finish` (the move to `done/`, so `owl send`, `owl reject`, `owl deny` and the auto
//! scheduler) remove `sessions/*/wake/<id>.md` and `spool/routing/<id>.json`, best effort.
//!
//! Every function here is synchronous file system work; the daemon runs the lease loop off
//! the async runtime (`lease_loop`). Writers race benignly: a hook's `SessionStart` and the
//! daemon's tick may both rewrite a routing, each write is atomic and the next tick repairs
//! a lost update.

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::contacts::ContactBook;
use crate::envelope::{self, Body, Payload};
use crate::render;
use crate::server::AppState;
use crate::spool::{Dir, Record, Spool};

/// `$OWLPOST_HOME/sessions/`.
pub const SESSIONS_DIR: &str = "sessions";
/// `$OWLPOST_HOME/spool/routing/`.
pub const ROUTING_DIR: &str = "routing";
pub const MARKER_FILE: &str = "marker.json";
pub const WAKE_DIR: &str = "wake";
pub const TMP_DIR: &str = "tmp";
/// Session ids: `[A-Za-z0-9._-]{1,128}`.
pub const SESSION_ID_RULE: &str = "[A-Za-z0-9._-]{1,128}";
/// Seconds a session keeps a record before the lease moves it on; default 600.
pub const LEASE_SECS_ENV: &str = "OWLPOST_WAKE_LEASE_SECS";
pub const DEFAULT_LEASE_SECS: u64 = 600;
/// Seconds after the last heartbeat a marker counts as dead; default 21600 (6 h).
pub const STALE_SECS_ENV: &str = "OWLPOST_SESSION_STALE_SECS";
pub const DEFAULT_STALE_SECS: u64 = 21_600;
/// The directory holding Claude Code's `sessions/<pid>.json` files; default `$HOME/.claude`.
pub const CLAUDE_HOME_ENV: &str = "OWLPOST_CLAUDE_HOME";
/// The daemon's lease loop period.
pub const LEASE_TICK_SECS: u64 = 30;

/// `[A-Za-z0-9._-]{1,128}`.
pub fn valid_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

pub fn sessions_dir(home: &Path) -> PathBuf {
    home.join(SESSIONS_DIR)
}

/// `sessions/<sid>`; the id must pass [`valid_session_id`] (callers check, this asserts).
pub fn session_dir(home: &Path, sid: &str) -> PathBuf {
    debug_assert!(valid_session_id(sid), "session id {sid:?} breaks the rule");
    sessions_dir(home).join(sid)
}

pub fn wake_dir(home: &Path, sid: &str) -> PathBuf {
    session_dir(home, sid).join(WAKE_DIR)
}

pub fn tmp_dir(home: &Path, sid: &str) -> PathBuf {
    session_dir(home, sid).join(TMP_DIR)
}

pub fn marker_path(home: &Path, sid: &str) -> PathBuf {
    session_dir(home, sid).join(MARKER_FILE)
}

/// `sessions/<sid>/wake/<id>.md`.
pub fn wake_file(home: &Path, sid: &str, id: &str) -> PathBuf {
    wake_dir(home, sid).join(format!("{id}.md"))
}

pub fn routing_dir(home: &Path) -> PathBuf {
    home.join("spool").join(ROUTING_DIR)
}

/// `spool/routing/<id>.json`.
pub fn routing_path(home: &Path, id: &str) -> PathBuf {
    routing_dir(home).join(format!("{id}.json"))
}

/// The directory Claude Code watches for this session: `<home>/sessions/<sid>/wake`,
/// absolute — canonicalised when it exists, otherwise resolved against the current directory.
pub fn wake_watch_path(home: &Path, sid: &str) -> PathBuf {
    let wake = wake_dir(home, sid);
    if let Ok(real) = wake.canonicalize() {
        return real;
    }
    if wake.is_absolute() {
        wake
    } else {
        std::env::current_dir().map_or(wake.clone(), |cwd| cwd.join(wake))
    }
}

/// `sessions/<sid>/marker.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub session_id: String,
    /// The session's working directory, canonicalised when it existed at `SessionStart`.
    pub cwd: String,
    pub started_at: String,
    pub heartbeat_at: String,
    /// `SessionStart`'s `source` (startup, resume, clear, compact, fork); empty when absent.
    #[serde(default)]
    pub source: String,
}

impl Marker {
    /// A fresh marker for `sid`: both timestamps now, `cwd` canonicalised when it exists.
    pub fn new(sid: &str, cwd: &str, source: &str) -> Marker {
        let now = envelope::rfc3339_now();
        Marker {
            session_id: sid.to_string(),
            cwd: canonical_string(cwd),
            started_at: now.clone(),
            heartbeat_at: now,
            source: source.to_string(),
        }
    }
}

/// `path` canonicalised when it exists, else unchanged.
fn canonical_string(path: &str) -> String {
    Path::new(path)
        .canonicalize()
        .map_or_else(|_| path.to_string(), |p| p.to_string_lossy().into_owned())
}

fn canonical_path(path: &str) -> PathBuf {
    Path::new(path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(path))
}

/// `<path>.tmp` + rename over `path`; a failed rename removes the temp file.
fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("renaming to {}", path.display()));
    }
    Ok(())
}

/// The marker of `sid`; `None` when absent or unreadable.
pub fn read_marker(home: &Path, sid: &str) -> Option<Marker> {
    if !valid_session_id(sid) {
        return None;
    }
    let bytes = std::fs::read(marker_path(home, sid)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Creates `sessions/<sid>/{wake,tmp}` and writes the marker atomically (temp file inside
/// `sessions/<sid>/`, never inside `wake/`).
pub fn write_marker(home: &Path, marker: &Marker) -> anyhow::Result<()> {
    anyhow::ensure!(
        valid_session_id(&marker.session_id),
        "session id must match {SESSION_ID_RULE}"
    );
    let sid = &marker.session_id;
    for dir in [wake_dir(home, sid), tmp_dir(home, sid)] {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    write_atomic(&marker_path(home, sid), &serde_json::to_vec_pretty(marker)?)
}

/// Sets `heartbeat_at` to now on an existing marker (read-modify-write, atomic). A missing
/// or unreadable marker is left as it is: nothing is created, `Ok`.
pub fn touch_heartbeat(home: &Path, sid: &str) -> anyhow::Result<()> {
    let Some(mut marker) = read_marker(home, sid) else {
        return Ok(());
    };
    marker.heartbeat_at = envelope::rfc3339_now();
    write_marker(home, &marker)
}

/// Removes `sessions/<sid>/` with everything in it; never fails.
pub fn remove_session(home: &Path, sid: &str) {
    if valid_session_id(sid) {
        let _ = std::fs::remove_dir_all(session_dir(home, sid));
    }
}

/// `spool/routing/<id>.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routing {
    /// The session holding the record, `null` when none is.
    pub current: Option<String>,
    /// When `current` got it (RFC 3339); the lease counts from here.
    pub routed_at: Option<String>,
    /// Every session the record was routed or assigned to, in order.
    #[serde(default)]
    pub tried: Vec<String>,
}

/// The routing of `id`; absent or unreadable → the default (never routed).
pub fn load_routing(home: &Path, id: &str) -> Routing {
    std::fs::read(routing_path(home, id))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_routing(home: &Path, id: &str, routing: &Routing) -> anyhow::Result<()> {
    let dir = routing_dir(home);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    write_atomic(
        &routing_path(home, id),
        &serde_json::to_vec_pretty(routing)?,
    )
}

pub fn remove_routing(home: &Path, id: &str) {
    let _ = std::fs::remove_file(routing_path(home, id));
}

/// `OWLPOST_CLAUDE_HOME`, else `$HOME/.claude`: where Claude Code keeps `sessions/<pid>.json`.
pub fn claude_home() -> PathBuf {
    if let Some(dir) = std::env::var_os(CLAUDE_HOME_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    std::env::var_os("HOME").map_or_else(
        || PathBuf::from(".claude"),
        |h| Path::new(&h).join(".claude"),
    )
}

fn env_secs(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

/// `OWLPOST_SESSION_STALE_SECS` or 21600.
pub fn stale_secs() -> u64 {
    env_secs(STALE_SECS_ENV, DEFAULT_STALE_SECS)
}

/// `OWLPOST_WAKE_LEASE_SECS` or 600.
pub fn lease_secs() -> u64 {
    env_secs(LEASE_SECS_ENV, DEFAULT_LEASE_SECS)
}

#[cfg(target_os = "linux")]
pub fn pid_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(not(target_os = "linux"))]
pub fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The pids Claude Code's `<claude_home>/sessions/*.json` files name for session `sid`
/// (`{"pid": N, "sessionId": "<uuid>", …}`, one file per live process; the `.key` siblings
/// and anything else are skipped). Empty when the directory is absent or nothing names it.
// ponytail: `~/.claude/sessions/<pid>.json` is Claude Code's internal format (2.1.263) and the
// hook does not receive the pid — the lookup is a liveness hint only; without a match the
// heartbeat alone decides. Read the pid from the hook input when Claude Code exposes it.
pub fn session_pids(claude_home: &Path, sid: &str) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(claude_home.join("sessions")) else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if v.get("sessionId").and_then(|s| s.as_str()) != Some(sid) {
            continue;
        }
        if let Some(pid) = v
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|p| u32::try_from(p).ok())
            .filter(|p| *p > 0)
        {
            pids.push(pid);
        }
    }
    pids
}

/// A marker is live when its file exists, every Claude Code sessions file naming it is not
/// contradicted (at least one named pid alive when any names it), and `heartbeat_at` is
/// younger than `stale_secs`.
pub fn is_live(
    home: &Path,
    claude_home: &Path,
    marker: &Marker,
    now: u64,
    stale_secs: u64,
) -> bool {
    if !valid_session_id(&marker.session_id) || !marker_path(home, &marker.session_id).is_file() {
        return false;
    }
    let pids = session_pids(claude_home, &marker.session_id);
    if !pids.is_empty() && !pids.iter().any(|p| pid_alive(*p)) {
        return false;
    }
    let Some(beat) = envelope::parse_rfc3339_to_unix(&marker.heartbeat_at) else {
        return false;
    };
    now.saturating_sub(beat) < stale_secs
}

/// The liveness inputs read from the environment: `OWLPOST_CLAUDE_HOME`, now,
/// `OWLPOST_SESSION_STALE_SECS`.
fn liveness_now() -> (PathBuf, u64, u64) {
    (claude_home(), envelope::now_unix(), stale_secs())
}

/// Every marker under `sessions/` (valid ids only), live or not, with its verdict.
fn all_markers(home: &Path) -> Vec<(Marker, bool)> {
    let (claude, now, stale) = liveness_now();
    let Ok(entries) = std::fs::read_dir(sessions_dir(home)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(sid) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !valid_session_id(&sid) || !entry.path().is_dir() {
            continue;
        }
        if let Some(marker) = read_marker(home, &sid) {
            let live = is_live(home, &claude, &marker, now, stale);
            out.push((marker, live));
        }
    }
    out
}

/// The live markers under `sessions/`.
pub fn live_markers(home: &Path) -> Vec<Marker> {
    all_markers(home)
        .into_iter()
        .filter_map(|(m, live)| live.then_some(m))
        .collect()
}

/// Removes every session directory whose marker is dead, missing or unreadable; live ones
/// stay. Never fails: an absent `sessions/` is nothing to sweep.
pub fn sweep_sessions(home: &Path) {
    let live: BTreeSet<String> = live_markers(home)
        .into_iter()
        .map(|m| m.session_id)
        .collect();
    let Ok(entries) = std::fs::read_dir(sessions_dir(home)) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(sid) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if valid_session_id(&sid) && entry.path().is_dir() && !live.contains(&sid) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The project a record is about: a question's own, an answer's from the question it
/// replies to; `None` when unknown or empty.
fn project_of(spool: &Spool, payload: &Payload) -> Option<String> {
    let project = match &payload.body {
        Body::Question { project, .. } => project.clone(),
        // A content request names its project too; a reply borrows the request's.
        Body::Content { project, .. } => project.clone().unwrap_or_default(),
        // A tool call names the project its tool runs in, when it names one.
        Body::ToolCall { project, .. } => project.clone().unwrap_or_default(),
        Body::Answer { .. } | Body::ContentReply { .. } | Body::ToolReply { .. } => match payload
            .in_reply_to
            .as_deref()
            .and_then(|q| render::find_question(spool, q))
            .map(|q| q.body)
        {
            Some(Body::Question { project, .. }) => project,
            Some(Body::Content { project, .. }) | Some(Body::ToolCall { project, .. }) => {
                project.unwrap_or_default()
            }
            _ => return None,
        },
    };
    (!project.is_empty()).then_some(project)
}

/// The one line a wake opens with. The wake carries metadata only — never a byte the peer
/// wrote — so the woken turn can tell the human that something arrived and nothing more;
/// the content is read when the human asks for it.
pub const WAKE_INSTRUCTION: &str = "A new owlpost message arrived. Tell the user in one line who it is from and what kind it is, using only the fields below, and offer /owlpost:inbox. Do not answer, draft, summarise or comment, and do not run `owl show <id>` or /owlpost:inbox unless the user asks.";

/// The wake file's text for a record: [`WAKE_INSTRUCTION`], a blank line, then one
/// `key: value` line each for `from` (the contact name from our own book, the fingerprint
/// for an unknown key), `id`, `type`, `state` and `received`, one trailing newline. Every
/// value is ours (the book, the spool record, the validated id); no peer-written string —
/// question, context, path, project, tool input or output — ever reaches the file.
pub fn wake_content(home: &Path, id: &str, rec: &Record, payload: &Payload) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let book = ContactBook::load(home, &cwd).unwrap_or_default();
    let from = render::peer_name_line(&book, &payload.from);
    let kind = serde_json::to_value(payload.kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    format!(
        "{WAKE_INSTRUCTION}\n\nfrom: {from}\nid: {id}\ntype: {kind}\nstate: {}\nreceived: {}\n",
        rec.state, rec.received_at
    )
}

/// Writes `sessions/<sid>/wake/<id>.md` by rename from `sessions/<sid>/tmp/<id>.md`, so
/// the watcher sees exactly one `add` and never a partial file.
pub fn write_wake(home: &Path, sid: &str, id: &str, content: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        valid_session_id(sid),
        "session id must match {SESSION_ID_RULE}"
    );
    for dir in [wake_dir(home, sid), tmp_dir(home, sid)] {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let tmp = tmp_dir(home, sid).join(format!("{id}.md"));
    let dst = wake_file(home, sid, id);
    std::fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &dst) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("renaming to {}", dst.display()));
    }
    Ok(())
}

/// Removes `sessions/<sid>/wake/<id>.md` (and a leftover in `tmp/`); never fails.
pub fn remove_wake(home: &Path, sid: &str, id: &str) {
    if valid_session_id(sid) {
        let _ = std::fs::remove_file(wake_file(home, sid, id));
        let _ = std::fs::remove_file(tmp_dir(home, sid).join(format!("{id}.md")));
    }
}

/// Routes inbox record `id` to one live session (see the module doc) and returns its id;
/// `None` when no live session is left to try (routing saved with `current: null`) or when
/// the record is not in `inbox/` any more (its routing is removed).
pub fn route(
    home: &Path,
    config: &Config,
    spool: &Spool,
    id: &str,
) -> anyhow::Result<Option<String>> {
    let Some(rec) = spool.get(Dir::Inbox, id)? else {
        release(home, id);
        return Ok(None);
    };
    let payload: Payload = serde_json::from_str(&rec.raw)
        .with_context(|| format!("record {id}: payload is malformed"))?;
    let checkout: Option<PathBuf> = project_of(spool, &payload)
        .and_then(|p| config.projects.get(&p).cloned())
        .map(|p| canonical_path(&p));
    let before = load_routing(home, id);
    let mut routing = before.clone();
    let mut candidates: Vec<Marker> = live_markers(home)
        .into_iter()
        .filter(|m| !routing.tried.contains(&m.session_id))
        .collect();
    candidates.sort_by_key(|m| {
        let elsewhere = checkout
            .as_ref()
            .is_some_and(|c| canonical_path(&m.cwd) != *c);
        (
            elsewhere,
            Reverse(envelope::parse_rfc3339_to_unix(&m.heartbeat_at).unwrap_or(0)),
            m.session_id.clone(),
        )
    });
    let Some(target) = candidates.into_iter().next() else {
        routing.current = None;
        routing.routed_at = None;
        if routing != before {
            save_routing(home, id, &routing)?;
        }
        return Ok(None);
    };
    let sid = target.session_id;
    let content = wake_content(home, id, &rec, &payload);
    write_wake(home, &sid, id, &content)?;
    routing.current = Some(sid.clone());
    routing.routed_at = Some(envelope::rfc3339_now());
    if !routing.tried.contains(&sid) {
        routing.tried.push(sid.clone());
    }
    save_routing(home, id, &routing)?;
    Ok(Some(sid))
}

/// Forgets record `id`: every `sessions/*/wake/<id>.md` and `spool/routing/<id>.json` go.
/// Best effort, never fails.
pub fn release(home: &Path, id: &str) {
    if let Ok(entries) = std::fs::read_dir(sessions_dir(home)) {
        for entry in entries.flatten() {
            if let Some(sid) = entry.file_name().to_str() {
                remove_wake(home, sid, id);
            }
        }
    }
    remove_routing(home, id);
}

/// `SessionStart`: every unseen inbox record whose routing has no live `current` is assigned
/// to `sid` (`current`, `routed_at`, `tried`) without a wake file — the start-up previews
/// already surface the backlog and the watcher is not armed yet. Never fails.
pub fn assign_backlog(home: &Path, spool: &Spool, sid: &str) {
    if !valid_session_id(sid) {
        return;
    }
    let live: BTreeSet<String> = live_markers(home)
        .into_iter()
        .map(|m| m.session_id)
        .collect();
    let Ok(records) = spool.list_lenient(Dir::Inbox) else {
        return;
    };
    for (id, rec) in records {
        if rec.seen {
            continue;
        }
        let mut routing = load_routing(home, &id);
        if routing.current.as_ref().is_some_and(|c| live.contains(c)) {
            continue;
        }
        routing.current = Some(sid.to_string());
        routing.routed_at = Some(envelope::rfc3339_now());
        if !routing.tried.iter().any(|t| t == sid) {
            routing.tried.push(sid.to_string());
        }
        let _ = save_routing(home, &id, &routing);
    }
}

/// `SessionEnd`: removes `sessions/<sid>/` and clears `current` in every routing naming
/// `sid` (the lease loop offers those records to another session). Never fails.
pub fn end_session(home: &Path, sid: &str) {
    if !valid_session_id(sid) {
        return;
    }
    remove_session(home, sid);
    let Ok(entries) = std::fs::read_dir(routing_dir(home)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let mut routing = load_routing(home, id);
        if routing.current.as_deref() == Some(sid) {
            routing.current = None;
            routing.routed_at = None;
            let _ = save_routing(home, id, &routing);
        }
    }
}

/// One lease tick over `spool/routing/*.json` (see the module doc), in path order so every
/// tick walks the routings the same way. `now` is unix seconds, `lease_secs` the lease.
/// Per-record errors are logged, never propagated: an unreadable record file or a record
/// whose payload does not parse is skipped (its routing and wake file stay as they are, the
/// record stays visible in the inbox for a human) and the tick moves on to the next routing.
pub fn lease_tick(home: &Path, config: &Config, spool: &Spool, now: u64, lease_secs: u64) {
    let Ok(entries) = std::fs::read_dir(routing_dir(home)) else {
        return;
    };
    let live: BTreeSet<String> = live_markers(home)
        .into_iter()
        .map(|m| m.session_id)
        .collect();
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let rec = match spool.get(Dir::Inbox, &id) {
            Ok(Some(rec)) => rec,
            Ok(None) => {
                release(home, &id);
                continue;
            }
            Err(e) => {
                tracing::warn!(id, error = %format!("{e:#}"), "lease: unreadable record");
                continue;
            }
        };
        if rec.seen {
            continue;
        }
        // A payload that does not parse could never be routed: leave its routing and wake
        // file alone rather than tearing the wake down for a `route` that is bound to fail.
        if let Err(e) = serde_json::from_str::<Payload>(&rec.raw) {
            tracing::warn!(id, error = %e, "lease: malformed record skipped");
            continue;
        }
        let routing = load_routing(home, &id);
        let again = match &routing.current {
            Some(sid) if !live.contains(sid) => {
                remove_wake(home, sid, &id);
                true
            }
            Some(sid) => {
                let routed = routing
                    .routed_at
                    .as_deref()
                    .and_then(envelope::parse_rfc3339_to_unix)
                    .unwrap_or(0);
                if now.saturating_sub(routed) > lease_secs {
                    remove_wake(home, sid, &id);
                    true
                } else {
                    false
                }
            }
            None => true,
        };
        if !again {
            continue;
        }
        match route(home, config, spool, &id) {
            Ok(Some(sid)) => tracing::info!(id, session = %sid, "lease: routed"),
            Ok(None) => tracing::debug!(id, "lease: no live session"),
            Err(e) => tracing::warn!(id, error = %format!("{e:#}"), "lease: routing failed"),
        }
    }
}

/// The daemon's lease loop: [`lease_tick`] every [`LEASE_TICK_SECS`], off the async runtime.
/// Ends only when the task is aborted.
pub async fn lease_loop(state: Arc<AppState>) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(LEASE_TICK_SECS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let st = state.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || {
            lease_tick(
                &st.home,
                &st.config,
                &st.spool,
                envelope::now_unix(),
                lease_secs(),
            );
        })
        .await
        {
            tracing::error!(error = %e, "lease tick panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_follow_the_rule_and_paths_stay_under_sessions() {
        for ok in ["a", "abc-123_x.y", &"z".repeat(128), "0", "A-B"] {
            assert!(valid_session_id(ok), "{ok:?}");
        }
        for bad in ["", &"z".repeat(129), "a b", "../x", "a/b", "ü", "a\n"] {
            assert!(!valid_session_id(bad), "{bad:?}");
        }
        let h = Path::new("/h");
        assert_eq!(wake_dir(h, "S1"), PathBuf::from("/h/sessions/S1/wake"));
        assert_eq!(tmp_dir(h, "S1"), PathBuf::from("/h/sessions/S1/tmp"));
        assert_eq!(
            marker_path(h, "S1"),
            PathBuf::from("/h/sessions/S1/marker.json")
        );
        assert_eq!(
            wake_file(h, "S1", "r"),
            PathBuf::from("/h/sessions/S1/wake/r.md")
        );
        assert_eq!(
            routing_path(h, "r"),
            PathBuf::from("/h/spool/routing/r.json")
        );
    }

    #[test]
    fn marker_round_trips_and_heartbeat_only_touches_existing_markers() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        touch_heartbeat(home, "S1").unwrap();
        assert!(!sessions_dir(home).exists(), "nothing created");
        let mut m = Marker::new("S1", "/nonexistent/x", "startup");
        m.heartbeat_at = "2020-01-01T00:00:00Z".into();
        write_marker(home, &m).unwrap();
        assert!(wake_dir(home, "S1").is_dir() && tmp_dir(home, "S1").is_dir());
        assert_eq!(read_marker(home, "S1"), Some(m.clone()));
        assert_eq!(m.cwd, "/nonexistent/x", "a missing cwd stays as given");
        touch_heartbeat(home, "S1").unwrap();
        let after = read_marker(home, "S1").unwrap();
        assert!(after.heartbeat_at > m.heartbeat_at);
        assert_eq!(after.started_at, m.started_at);
        assert!(
            std::fs::read_dir(wake_dir(home, "S1"))
                .unwrap()
                .next()
                .is_none()
        );
        assert!(!session_dir(home, "S1").join("marker.json.tmp").exists());
        // A cwd that exists is canonicalised.
        let real = Marker::new("S2", home.to_str().unwrap(), "resume");
        assert_eq!(real.cwd, home.canonicalize().unwrap().to_string_lossy());
        assert!(write_marker(home, &Marker::new("a/b", "/", "")).is_err());
        remove_session(home, "S1");
        assert!(!session_dir(home, "S1").exists());
        assert_eq!(read_marker(home, "../x"), None);
    }

    #[test]
    fn routing_defaults_when_absent_and_saves_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        assert_eq!(load_routing(home, "r"), Routing::default());
        let r = Routing {
            current: Some("S1".into()),
            routed_at: Some("2026-09-07T00:00:00Z".into()),
            tried: vec!["S1".into()],
        };
        save_routing(home, "r", &r).unwrap();
        assert_eq!(load_routing(home, "r"), r);
        assert!(!routing_dir(home).join("r.json.tmp").exists());
        std::fs::write(routing_path(home, "r"), "{broken").unwrap();
        assert_eq!(load_routing(home, "r"), Routing::default());
        remove_routing(home, "r");
        assert!(!routing_path(home, "r").exists());
        remove_routing(home, "r");
    }

    #[test]
    fn env_secs_fall_back_to_defaults() {
        assert_eq!(env_secs("OWLPOST_TEST_UNSET_SECS_X", 7), 7);
    }

    /// `OWLPOST_CLAUDE_HOME` wins when set and non-empty; otherwise `$HOME/.claude`. Nothing
    /// else in this binary reads the variable, and every read goes through `std::env`.
    #[test]
    fn claude_home_is_the_env_override_or_home_dot_claude() {
        let home = std::env::var_os("HOME").expect("HOME is set in the test environment");
        let fallback = Path::new(&home).join(".claude");
        // SAFETY: see above.
        unsafe {
            std::env::set_var(CLAUDE_HOME_ENV, "/tmp/owlpost-test-claude");
        }
        assert_eq!(claude_home(), PathBuf::from("/tmp/owlpost-test-claude"));
        unsafe {
            std::env::set_var(CLAUDE_HOME_ENV, "");
        }
        assert_eq!(claude_home(), fallback, "empty means unset");
        unsafe {
            std::env::remove_var(CLAUDE_HOME_ENV);
        }
        assert_eq!(claude_home(), fallback);
    }
}
