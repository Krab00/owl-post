//! Daemon run loop: the mTLS listener, the iroh listener (`crate::iroh`), the pull loop
//! (`crate::pull`), the auto-accept scan (`crate::auto`) and the session wake lease loop
//! (`crate::route`) as sibling tasks; the event consumer notifies and triggers
//! auto-accept.

use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Context;
use axum_server::Handle;
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::contacts::ContactBook;
use crate::identity::{self, Identity};
use crate::notify::{self, Kind};
use crate::server::{AppState, DaemonEvent, PeerAcceptor};
use crate::spool::Dir;
use crate::tls::{self, AllowedKeys};

pub const ADDR_FILE: &str = "daemon.addr";

/// A daemon running inside this process.
pub struct Running {
    pub addr: SocketAddr,
    pub state: Arc<AppState>,
    handle: Handle<SocketAddr>,
    task: JoinHandle<std::io::Result<()>>,
    /// The pull loop; aborted on `shutdown` / `wait`.
    pull: JoinHandle<()>,
    /// The auto-accept scan; aborted with the pull loop.
    auto: JoinHandle<()>,
    /// The session wake lease loop (`crate::route`); aborted with the pull loop.
    lease: JoinHandle<()>,
    /// The iroh endpoint (closed on `shutdown` / `wait`) and its accept loop.
    iroh: iroh::Endpoint,
    accept: JoinHandle<()>,
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running").field("addr", &self.addr).finish()
    }
}

impl Running {
    /// Stops accepting, drops open connections, closes the iroh endpoint and stops the pull
    /// loop.
    pub fn shutdown(&self) {
        self.handle.shutdown();
        self.pull.abort();
        self.auto.abort();
        self.lease.abort();
        self.accept.abort();
        let endpoint = self.iroh.clone();
        tokio::spawn(async move { endpoint.close().await });
    }

    /// Waits for the listener task to end (after `shutdown`, or on a listener error); the pull
    /// loop and the iroh endpoint are stopped with it.
    pub async fn wait(self) -> anyhow::Result<()> {
        let result = self.task.await.context("listener task");
        self.pull.abort();
        self.auto.abort();
        self.lease.abort();
        self.accept.abort();
        self.iroh.close().await;
        result??;
        Ok(())
    }

    /// The daemon's iroh endpoint.
    pub fn iroh(&self) -> &iroh::Endpoint {
        &self.iroh
    }

    /// True while the pull loop task is alive.
    pub fn pull_running(&self) -> bool {
        !self.pull.is_finished()
    }
}

/// Client keys allowed through the TLS handshake: every contact in the merged book, plus
/// the owner's own key (the CLI uses it for the iroh forward route).
pub fn allowed_keys(book: &ContactBook, owner: &Identity) -> AllowedKeys {
    book.contacts
        .iter()
        .filter_map(|c| identity::parse_pubkey(&c.pubkey).ok())
        .map(|pk| *pk.as_bytes())
        .chain(std::iter::once(*owner.verifying_key().as_bytes()))
        .collect()
}

/// Binds `config.listen` (port 0 allowed) and serves the API on the tokio runtime.
pub async fn spawn(home: &Path, config: Config) -> anyhow::Result<Running> {
    let identity = Identity::load(home)?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let book = ContactBook::load(home, &cwd)?;
    let listen: SocketAddr = config
        .listen
        .parse()
        .with_context(|| format!("config.listen {:?} is not host:port", config.listen))?;
    let tls_config = tls::server_config(&identity, allowed_keys(&book, &identity), true)?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DaemonEvent>();
    let scheduler = crate::auto::Scheduler::new();
    let state = Arc::new(AppState::new(
        home.to_path_buf(),
        cwd,
        config,
        identity,
        Some(tx),
    )?);
    let events_state = state.clone();
    let events_scheduler = scheduler.clone();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let DaemonEvent::Question(q) = &ev
                && q.auto
            {
                events_scheduler.spawn(events_state.clone(), q.id.clone());
            }
            handle_event(&events_state, ev);
        }
    });
    let endpoint =
        crate::iroh::endpoint(&state.identity, state.config.relay_urls.as_deref()).await?;
    let _ = state.iroh.set(endpoint.clone());
    let accept = tokio::spawn(crate::iroh::accept_loop(endpoint.clone(), state.clone()));
    let app = crate::server::router(state.clone());
    let acceptor = PeerAcceptor::new(RustlsAcceptor::new(RustlsConfig::from_config(tls_config)));
    let handle = Handle::new();
    let server = axum_server::bind(listen)
        .acceptor(acceptor)
        .handle(handle.clone());
    let task = tokio::spawn(server.serve(app.into_make_service()));
    let Some(addr) = handle.listening().await else {
        let err = match task.await {
            Ok(Err(e)) => anyhow::Error::from(e),
            Ok(Ok(())) => anyhow::anyhow!("listener exited before binding"),
            Err(e) => anyhow::Error::from(e),
        };
        accept.abort();
        endpoint.close().await;
        return Err(err.context(format!("binding {listen}")));
    };
    let _ = state.bound.set(addr);
    tracing::info!(%addr, fingerprint = %state.fingerprint(), iroh = %endpoint.id().fmt_short(), "listening");
    let pull = tokio::spawn(crate::pull::run_loop(state.clone()));
    let auto = tokio::spawn(scheduler.run_scan(state.clone()));
    let lease = tokio::spawn(crate::route::lease_loop(state.clone()));
    Ok(Running {
        addr,
        state,
        handle,
        task,
        pull,
        auto,
        lease,
        iroh: endpoint,
        accept,
    })
}

/// One daemon-loop event: log it and fire the OS notification (§11).
fn handle_event(state: &AppState, ev: DaemonEvent) {
    match ev {
        DaemonEvent::Question(q) => {
            tracing::info!(id = %q.id, peer = %q.peer, state = %q.state, auto = q.auto, "spooled");
            let path = question_path(state, &q.id);
            notify::notify(
                &state.config,
                Kind::Question,
                &peer_name(state, &q.peer),
                &path,
            );
        }
        DaemonEvent::Answer(a) => {
            tracing::info!(id = %a.id, peer = %a.peer, path = %a.path, "answer ingested");
            notify::notify(
                &state.config,
                Kind::Answer,
                &peer_name(state, &a.peer),
                &a.path,
            );
        }
        DaemonEvent::Declined(d) => {
            tracing::info!(id = %d.id, peer = %d.peer, "question declined");
            notify::notify(
                &state.config,
                Kind::Declined,
                &peer_name(state, &d.peer),
                "-",
            );
        }
    }
}

/// Display name for a fingerprint: the contact's name, else the fingerprint itself.
pub fn peer_name(state: &AppState, fingerprint: &str) -> String {
    ContactBook::load(&state.home, &state.cwd)
        .ok()
        .and_then(|book| {
            book.contacts
                .into_iter()
                .find(|c| c.fingerprint == fingerprint)
                .map(|c| c.name)
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| fingerprint.to_string())
}

/// `body.path` of the spooled question `id`; `"-"` for a repo-level question, `"?"` when the
/// record cannot be read.
fn question_path(state: &AppState, id: &str) -> String {
    let rec = match state.spool.get(Dir::Inbox, id) {
        Ok(Some(rec)) => rec,
        _ => return "?".to_string(),
    };
    match serde_json::from_str::<crate::envelope::Payload>(&rec.raw) {
        Ok(crate::envelope::Payload {
            body: crate::envelope::Body::Question { path, .. },
            ..
        }) => path.unwrap_or_else(|| "-".to_string()),
        _ => "?".to_string(),
    }
}

/// Atomically writes `<home>/<name>`: `<name>.tmp` + rename, like the spool; a failed rename
/// removes the temp file.
pub fn write_atomic(home: &Path, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let path = home.join(name);
    let tmp = home.join(format!("{name}.tmp"));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("renaming to {}", path.display()));
    }
    Ok(())
}

/// Atomically writes `<home>/daemon.addr` (`host:port\n`).
pub fn write_addr_file(home: &Path, addr: SocketAddr) -> anyhow::Result<()> {
    write_atomic(home, ADDR_FILE, format!("{addr}\n").as_bytes())
}

/// A wildcard bind address is not connectable; use the loopback of the same family.
pub fn connect_addr(addr: SocketAddr) -> SocketAddr {
    if addr.ip().is_unspecified() {
        let ip: IpAddr = match addr.ip() {
            IpAddr::V4(_) => "127.0.0.1".parse().expect("literal"),
            IpAddr::V6(_) => "::1".parse().expect("literal"),
        };
        SocketAddr::new(ip, addr.port())
    } else {
        addr
    }
}

/// The connectable address of the local daemon from `<home>/daemon.addr`; `None` when the
/// file is missing or does not hold `host:port`.
pub fn local_addr(home: &Path) -> Option<SocketAddr> {
    std::fs::read_to_string(home.join(ADDR_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
        .map(connect_addr)
}

/// How long a trash batch is kept before the daemon removes it for good.
pub const TRASH_MAX_AGE: Duration = Duration::from_secs(30 * 86_400);

/// True when `path` lies exactly one normal path component below `trash`: the check
/// `gc_trash` runs on every entry before deleting anything.
fn is_direct_child(trash: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(trash) else {
        return false;
    };
    let mut components = rel.components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

/// Removes every direct child of `<home>/trash/` older than `TRASH_MAX_AGE` (strictly),
/// valid batch or not, and returns how many were removed. A child's age comes from its name
/// parsed as a UUIDv7 (the embedded timestamp); any other name falls back to the child's own
/// mtime (`symlink_metadata`, never followed). A name in the future counts as age 0: kept.
///
/// This is the only recursive delete in the codebase, so it is defensive. A `trash` that is
/// missing, a file or a symlink means no action and 0. A child is deleted only after a path
/// check — its parent is exactly `trash` and its name a single normal component. A real
/// directory goes with `remove_dir_all` (std never follows symlinks inside); anything else, a
/// symlink included, is unlinked with `remove_file`, so a symlink's target is never touched.
/// An error on one child is warned about and the walk goes on.
pub fn gc_trash(home: &Path, now: SystemTime) -> usize {
    let trash = home.join("trash");
    let Ok(meta) = std::fs::symlink_metadata(&trash) else {
        return 0;
    };
    if !meta.is_dir() {
        return 0;
    }
    let entries = match std::fs::read_dir(&trash) {
        Ok(rd) => rd,
        Err(e) => {
            tracing::warn!(path = %trash.display(), error = %e, "listing the trash failed");
            return 0;
        }
    };
    // What a child's age counts from: its UUIDv7 name's timestamp, else its own mtime.
    let stamp_of = |path: &Path, name: &str| -> Option<SystemTime> {
        if let Ok(u) = uuid::Uuid::parse_str(name)
            && u.get_version_num() == 7
            && let Some(ts) = u.get_timestamp()
        {
            let (secs, nanos) = ts.to_unix();
            return Some(SystemTime::UNIX_EPOCH + Duration::new(secs, nanos));
        }
        std::fs::symlink_metadata(path)
            .and_then(|m| m.modified())
            .ok()
    };
    let mut removed = 0;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "reading a trash entry failed");
                continue;
            }
        };
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(stamp) = stamp_of(&path, name) else {
            tracing::warn!(path = %path.display(), "trash child metadata unreadable; kept");
            continue;
        };
        let age = now.duration_since(stamp).unwrap_or(Duration::ZERO);
        if age <= TRASH_MAX_AGE {
            continue;
        }
        if !is_direct_child(&trash, &path) {
            tracing::warn!(path = %path.display(), "trash child failed the path check; kept");
            continue;
        }
        // `file_type` does not follow symlinks: a symlink is unlinked, its target untouched.
        let result = match entry.file_type() {
            Ok(kind) if kind.is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "trash child type unreadable; kept");
                continue;
            }
        };
        match result {
            Ok(()) => removed += 1,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "removing a trash child failed")
            }
        }
    }
    removed
}

/// `owl daemon --foreground`: serve until the listener stops (or the process is killed).
pub async fn run_foreground(home: &Path, config: Config) -> anyhow::Result<()> {
    let running = spawn(home, config).await?;
    write_addr_file(home, running.addr)?;
    tracing::info!(path = %home.join(ADDR_FILE).display(), "wrote daemon.addr");
    running.wait().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr_file_is_written_atomically() {
        let home = tempfile::tempdir().unwrap();
        let addr: SocketAddr = "127.0.0.1:4242".parse().unwrap();
        write_addr_file(home.path(), addr).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.path().join(ADDR_FILE)).unwrap(),
            "127.0.0.1:4242\n"
        );
        assert!(!home.path().join("daemon.addr.tmp").exists());
        // Overwrite on restart.
        write_addr_file(home.path(), "127.0.0.1:1".parse().unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.path().join(ADDR_FILE)).unwrap(),
            "127.0.0.1:1\n"
        );
    }

    #[test]
    fn addr_file_failure_leaves_no_tmp() {
        let home = tempfile::tempdir().unwrap();
        // Destination blocked by a non-empty directory: rename must fail.
        let blocker = home.path().join(ADDR_FILE);
        std::fs::create_dir_all(blocker.join("child")).unwrap();
        let err = write_addr_file(home.path(), "127.0.0.1:1".parse().unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("renaming to"), "{err}");
        assert!(!home.path().join("daemon.addr.tmp").exists());
        assert!(blocker.is_dir(), "blocker untouched");
        // Missing home: the temp write itself fails.
        let err = write_addr_file(&home.path().join("missing"), "127.0.0.1:1".parse().unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("writing"), "{err}");
    }

    #[test]
    fn allowed_keys_skips_bad_pubkeys_and_adds_the_owner() {
        use crate::contacts::Contact;
        let good = Identity::from_seed([5u8; 32]);
        let owner = Identity::from_seed([6u8; 32]);
        let mk = |pubkey: &str| Contact {
            name: String::new(),
            emails: vec![],
            pubkey: pubkey.into(),
            endpoints: vec![],
            source: "local".into(),
            policy: None,
            added_at: None,
            fingerprint: String::new(),
        };
        let book = ContactBook {
            contacts: vec![
                mk(&identity::pubkey_string(&good.verifying_key())),
                mk("ed25519:AAAA"),
                mk("garbage"),
            ],
        };
        let keys = allowed_keys(&book, &owner);
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(good.verifying_key().as_bytes()));
        assert!(keys.contains(owner.verifying_key().as_bytes()));
        let only_owner = allowed_keys(&ContactBook::default(), &owner);
        assert_eq!(only_owner.len(), 1);
        assert!(only_owner.contains(owner.verifying_key().as_bytes()));
    }

    #[test]
    fn local_addr_reads_daemon_addr_and_maps_wildcards() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(local_addr(home.path()), None, "no file");
        std::fs::write(home.path().join(ADDR_FILE), "garbage\n").unwrap();
        assert_eq!(local_addr(home.path()), None, "not host:port");
        write_addr_file(home.path(), "0.0.0.0:7411".parse().unwrap()).unwrap();
        assert_eq!(
            local_addr(home.path()),
            Some("127.0.0.1:7411".parse().unwrap())
        );
        write_addr_file(home.path(), "[::]:7411".parse().unwrap()).unwrap();
        assert_eq!(local_addr(home.path()), Some("[::1]:7411".parse().unwrap()));
        write_addr_file(home.path(), "10.0.0.5:1".parse().unwrap()).unwrap();
        assert_eq!(local_addr(home.path()), Some("10.0.0.5:1".parse().unwrap()));
    }

    /// A UUIDv7 name whose embedded timestamp is `secs` (the batch-name shape `owl delete`
    /// writes).
    fn v7_name(secs: u64) -> String {
        uuid::Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, secs, 0)).to_string()
    }

    /// Every file and symlink under `root`, relative path → bytes; a symlink is recorded as
    /// its target with an `L:` marker, so a link can never pass for the file behind it.
    fn tree(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        let mut out = std::collections::BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let path = e.unwrap().path();
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                let meta = std::fs::symlink_metadata(&path).unwrap();
                if meta.is_dir() {
                    stack.push(path);
                } else if meta.file_type().is_symlink() {
                    let target = std::fs::read_link(&path).unwrap();
                    let mut bytes = b"L:".to_vec();
                    bytes.extend(target.to_str().unwrap().as_bytes());
                    out.insert(rel, bytes);
                } else {
                    out.insert(rel, std::fs::read(&path).unwrap());
                }
            }
        }
        out
    }

    /// The path check before any delete: exactly one normal component below `trash`.
    #[test]
    fn is_direct_child_accepts_only_one_normal_component_below_trash() {
        let trash = Path::new("/home/trash");
        assert!(is_direct_child(
            trash,
            &trash.join("01a06ea2-62b8-7854-8e46-d39763f746e8")
        ));
        assert!(
            !is_direct_child(trash, &trash.join("a").join("b")),
            "trash/a/b"
        );
        assert!(
            !is_direct_child(trash, &trash.join("..").join("x")),
            "trash/../x"
        );
        assert!(
            !is_direct_child(trash, &trash.join("..")),
            "trash/..: the relative part is a single ParentDir, not Normal"
        );
        assert!(
            !is_direct_child(trash, Path::new("/home/other/x")),
            "a sibling dir's child"
        );
        assert!(!is_direct_child(trash, trash), "trash itself");
        assert!(
            !is_direct_child(trash, &trash.join("x").join("..")),
            "trash/x/.."
        );
    }

    /// The GC removes only the strictly-old children of `trash/` — by UUIDv7 name or by
    /// mtime, valid batch or junk — and never follows a symlink: the outside dirs and files
    /// behind links survive byte-identical, and everything young is untouched.
    #[test]
    fn gc_trash_removes_only_old_children() {
        const DAY: u64 = 86_400;
        let home = tempfile::tempdir().unwrap();
        let trash = home.path().join("trash");
        std::fs::create_dir_all(&trash).unwrap();
        let now_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // Truncated to seconds, the UUIDv7 resolution: a 30-day-old name is then exactly at
        // the boundary, not a subsecond past it.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(now_secs);
        let secs = now_secs;

        // Old valid batch: batch.json, a record, and a symlink pointing OUT of the trash.
        let outside_inner = home.path().join("outside-inner");
        std::fs::create_dir_all(&outside_inner).unwrap();
        std::fs::write(outside_inner.join("keep.txt"), b"keep me").unwrap();
        let old_valid = trash.join(v7_name(secs - 31 * DAY));
        std::fs::create_dir_all(old_valid.join("inbox")).unwrap();
        std::fs::write(old_valid.join("batch.json"), b"{\"peer\":\"x\"}").unwrap();
        std::fs::write(old_valid.join("inbox").join("r1.json"), b"record one").unwrap();
        std::os::unix::fs::symlink(&outside_inner, old_valid.join("link-out")).unwrap();
        // Kept by name: 29 days, exactly 30 days (not strictly greater), one day in the
        // future (age 0).
        let young29 = trash.join(v7_name(secs - 29 * DAY));
        std::fs::create_dir_all(&young29).unwrap();
        std::fs::write(young29.join("batch.json"), b"{\"peer\":\"y\"}").unwrap();
        let edge30 = trash.join(v7_name(secs - 30 * DAY));
        std::fs::create_dir_all(&edge30).unwrap();
        std::fs::write(edge30.join("batch.json"), b"{\"peer\":\"z\"}").unwrap();
        // Strictly past the boundary by one second: removed.
        let edge30plus = trash.join(v7_name(secs - 30 * DAY - 1));
        std::fs::create_dir_all(&edge30plus).unwrap();
        std::fs::write(edge30plus.join("batch.json"), b"{\"peer\":\"w\"}").unwrap();
        let future = trash.join(v7_name(secs + DAY));
        std::fs::create_dir_all(&future).unwrap();
        std::fs::write(future.join("note.txt"), b"from the future").unwrap();
        // Junk by mtime: `junk` set 31 days back goes, fresh `junk-young` stays.
        let junk = trash.join("junk");
        std::fs::create_dir_all(&junk).unwrap();
        std::fs::write(junk.join("note.txt"), b"stale").unwrap();
        std::fs::File::open(&junk)
            .unwrap()
            .set_modified(now - Duration::from_secs(31 * DAY))
            .unwrap();
        let junk_young = trash.join("junk-young");
        std::fs::create_dir_all(&junk_young).unwrap();
        std::fs::write(junk_young.join("note.txt"), b"fresh").unwrap();
        // An old-named symlink pointing at a dir OUTSIDE the trash: unlinked, target kept.
        let outside = home.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("data.txt"), b"outside data").unwrap();
        let link = trash.join(v7_name(secs - 31 * DAY));
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        // An old REGULAR FILE directly in the trash (not a batch dir): unlinked like the rest.
        let old_file = trash.join(v7_name(secs - 31 * DAY));
        std::fs::write(&old_file, b"loose old file").unwrap();
        // A FRESH symlink with a non-UUID name whose TARGET's mtime is 31 days back: aged by
        // its own fresh mtime (symlink_metadata never follows), so it is kept. Following the
        // link would see the old target mtime and unlink it.
        let outside_old = home.path().join("outside-old");
        std::fs::create_dir_all(&outside_old).unwrap();
        std::fs::write(outside_old.join("data.txt"), b"old-looking").unwrap();
        std::fs::File::open(&outside_old)
            .unwrap()
            .set_modified(now - Duration::from_secs(31 * DAY))
            .unwrap();
        let link_young = trash.join("link-young");
        std::os::unix::fs::symlink(&outside_old, &link_young).unwrap();

        // Precondition: the whole fixture is in place.
        for p in [
            &old_valid,
            &young29,
            &edge30,
            &edge30plus,
            &future,
            &junk,
            &junk_young,
        ] {
            assert!(p.is_dir(), "{p:?}");
        }
        assert!(
            old_file.is_file()
                && !std::fs::symlink_metadata(&old_file)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
            "an old regular file, not a dir or symlink"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::symlink_metadata(&link_young)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let before = tree(home.path());

        assert_eq!(gc_trash(home.path(), now), 5);
        assert!(!old_valid.exists(), "the old batch is gone");
        assert!(
            !edge30plus.exists(),
            "30 days + 1 second is strictly old: gone"
        );
        assert!(!old_file.exists(), "the old regular file is unlinked");
        assert!(!junk.exists(), "the old junk dir is gone");
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "the old symlink is unlinked"
        );
        assert_eq!(
            std::fs::read_link(&link_young).unwrap(),
            outside_old,
            "the fresh symlink is kept (its own mtime is young), still pointing outside"
        );
        // Everything else is byte-identical: drop the five removed children from the
        // before-picture and the rest must match exactly (`starts_with` is component-wise,
        // so `junk` does not eat `junk-young`).
        let mut want = before;
        let under_trash = |name: &std::ffi::OsStr| Path::new("trash").join(name);
        want.retain(|rel, _| {
            !rel.starts_with(under_trash(old_valid.file_name().unwrap()))
                && !rel.starts_with(under_trash(edge30plus.file_name().unwrap()))
                && !rel.starts_with(under_trash(old_file.file_name().unwrap()))
                && !rel.starts_with(under_trash(std::ffi::OsStr::new("junk")))
                && !rel.starts_with(under_trash(link.file_name().unwrap()))
        });
        assert_eq!(tree(home.path()), want);
        assert_eq!(
            std::fs::read(outside.join("data.txt")).unwrap(),
            b"outside data",
            "the symlink target outside the trash is untouched"
        );
        assert_eq!(
            std::fs::read(outside_inner.join("keep.txt")).unwrap(),
            b"keep me",
            "the target of the link inside the removed batch is untouched"
        );
    }

    /// A `trash` that is missing, a regular file or a symlink is never walked into: 0 and no
    /// deletion, even when the symlink's target holds an old-named child.
    #[test]
    fn gc_trash_never_follows_the_trash_root() {
        let home = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        assert_eq!(gc_trash(home.path(), now), 0, "missing trash");
        assert!(!home.path().join("trash").exists(), "nothing is created");

        std::fs::write(home.path().join("trash"), b"not a dir").unwrap();
        assert_eq!(gc_trash(home.path(), now), 0, "trash as a file");
        assert_eq!(
            std::fs::read(home.path().join("trash")).unwrap(),
            b"not a dir"
        );
        std::fs::remove_file(home.path().join("trash")).unwrap();

        let outside = tempfile::tempdir().unwrap();
        let secs = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let child = outside.path().join(v7_name(secs - 31 * 86_400));
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(child.join("batch.json"), b"{}").unwrap();
        std::os::unix::fs::symlink(outside.path(), home.path().join("trash")).unwrap();
        assert_eq!(gc_trash(home.path(), now), 0, "trash as a symlink");
        assert!(
            child.join("batch.json").is_file(),
            "the symlink target is untouched"
        );
        assert!(
            std::fs::symlink_metadata(home.path().join("trash"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink itself is left in place"
        );
    }

    /// A removal error on one child is warned about and the walk goes on: every other old
    /// child is still removed and counted, the failing one stays behind byte-identical.
    #[test]
    fn gc_trash_keeps_walking_after_a_remove_error() {
        use std::os::unix::fs::PermissionsExt;
        const DAY: u64 = 86_400;
        let now_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(now_secs);
        // `read_dir` order is undefined: three fresh homes with fresh names, so an early
        // return after the first error cannot survive every round by luck alone.
        for round in 0..3 {
            let home = tempfile::tempdir().unwrap();
            let trash = home.path().join("trash");
            std::fs::create_dir_all(&trash).unwrap();
            // The stuck batch: an unwritable subdir makes `remove_dir_all` fail on the file
            // inside it (running as a non-root user, where the permission check applies).
            let stuck = trash.join(v7_name(now_secs - 31 * DAY));
            let locked = stuck.join("locked");
            std::fs::create_dir_all(&locked).unwrap();
            std::fs::write(locked.join("data.txt"), b"stuck payload").unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
            // Six good old batches, each with content of its own.
            let mut good = vec![];
            for i in 0..6u32 {
                let g = trash.join(v7_name(now_secs - 31 * DAY));
                std::fs::create_dir_all(&g).unwrap();
                std::fs::write(g.join("batch.json"), format!("round {round} batch {i}")).unwrap();
                good.push(g);
            }
            // Precondition: the fixture is in place.
            assert_eq!(
                std::fs::read(locked.join("data.txt")).unwrap(),
                b"stuck payload"
            );
            for g in &good {
                assert!(g.join("batch.json").is_file(), "{g:?}");
            }
            let before = tree(home.path());

            assert_eq!(
                gc_trash(home.path(), now),
                good.len(),
                "round {round}: the good batches are removed despite the failure"
            );
            for g in &good {
                assert!(!g.exists(), "round {round}: {g:?} is gone");
            }
            assert!(
                stuck.is_dir(),
                "round {round}: the failing batch is left behind"
            );
            assert_eq!(
                std::fs::read(locked.join("data.txt")).unwrap(),
                b"stuck payload",
                "round {round}: the failing batch is intact"
            );
            // Everything else is byte-identical: only the good batches leave the picture.
            let mut want = before;
            want.retain(|rel, _| {
                !good
                    .iter()
                    .any(|g| rel.starts_with(Path::new("trash").join(g.file_name().unwrap())))
            });
            assert_eq!(tree(home.path()), want, "round {round}");
            // Restore permissions so the tempdir cleanup can remove the locked subdir.
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    /// Only a UUIDv7 name is aged by its embedded timestamp: a v6-named child whose name says
    /// 31 days back but whose mtime is fresh (just created) is kept, while a real old v7
    /// batch in the same trash still goes.
    #[test]
    fn gc_trash_ignores_old_timestamps_in_non_v7_names() {
        const DAY: u64 = 86_400;
        let home = tempfile::tempdir().unwrap();
        let trash = home.path().join("trash");
        std::fs::create_dir_all(&trash).unwrap();
        let now_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(now_secs);

        // Hand-built v6 name (the uuid crate's v6 feature is not enabled): the 60-bit
        // timestamp is 100-ns ticks since 1582-10-15, laid out as 48 high bits, the version
        // nibble 6 with the 12 low bits, then the variant nibble, clock-seq and node.
        let ticks = (now_secs - 31 * DAY) * 10_000_000 + 0x01B2_1DD2_1381_4000;
        let v6 = format!(
            "{:08x}-{:04x}-6{:03x}-{:04x}-{:012x}",
            ticks >> 28,
            (ticks >> 12) & 0xFFFF,
            ticks & 0xFFF,
            0x8123u16,
            0xABCD_EF01_2345u64
        );
        // Preconditions: the name is a valid v6 UUID carrying a 31-day-old timestamp.
        let parsed = uuid::Uuid::parse_str(&v6).unwrap();
        assert_eq!(parsed.get_version_num(), 6);
        let (ts_secs, _) = parsed
            .get_timestamp()
            .expect("a v6 UUID carries a timestamp")
            .to_unix();
        assert_eq!(ts_secs, now_secs - 31 * DAY);

        // Freshly created, so its own mtime is young: aged by mtime, not by the v6 name.
        let v6_child = trash.join(&v6);
        std::fs::create_dir_all(&v6_child).unwrap();
        std::fs::write(v6_child.join("batch.json"), b"{\"peer\":\"v6\"}").unwrap();
        // A real old v7 batch, so the gc still removes something.
        let old_v7 = trash.join(v7_name(now_secs - 31 * DAY));
        std::fs::create_dir_all(&old_v7).unwrap();
        std::fs::write(old_v7.join("batch.json"), b"{\"peer\":\"v7\"}").unwrap();

        assert!(v6_child.is_dir());
        assert!(old_v7.is_dir());
        let before = tree(home.path());

        assert_eq!(gc_trash(home.path(), now), 1);
        assert!(
            v6_child.is_dir(),
            "the v6-named child is kept: aged by its fresh mtime, not its name"
        );
        assert!(!old_v7.exists(), "the old v7 batch is gone");
        let mut want = before;
        want.retain(|rel, _| {
            !rel.starts_with(Path::new("trash").join(old_v7.file_name().unwrap()))
        });
        assert_eq!(tree(home.path()), want);
    }

    /// The iroh endpoint id is the identity's public key, byte for byte, and the card
    /// repeats it as `iroh.id` next to `owlpost.pubkey`.
    #[tokio::test]
    async fn iroh_endpoint_id_is_identity_pubkey() {
        let home = tempfile::tempdir().unwrap();
        let id = Identity::from_seed([7u8; 32]);
        id.save(home.path()).unwrap();
        let cfg = Config {
            listen: "127.0.0.1:0".into(),
            relay_urls: Some(vec![]),
            ..Default::default()
        };
        let running = spawn(home.path(), cfg).await.unwrap();
        assert_eq!(
            running.iroh().id().as_bytes(),
            id.verifying_key().as_bytes()
        );
        assert_eq!(
            running.state.iroh.get().unwrap().id().as_bytes(),
            id.verifying_key().as_bytes()
        );
        let card = crate::server::card_json(&running.state);
        let pubkey = identity::pubkey_string(&id.verifying_key());
        // The bound endpoint shows as the `owl-iroh://<key>` interface (the key
        // without its `ed25519:` prefix), the relay (none here) in the identity extension.
        assert_eq!(
            card["supportedInterfaces"][1]["url"],
            format!("owl-iroh://{}", pubkey.strip_prefix("ed25519:").unwrap())
        );
        let params = crate::server::extension_params(&card, crate::server::EXT_IDENTITY).unwrap();
        assert_eq!(params["pubkey"], pubkey);
        assert_eq!(params["relay"], serde_json::Value::Null);
        // A different seed is a different id: the equality above is not vacuous.
        assert_ne!(
            running.iroh().id().as_bytes(),
            Identity::from_seed([8u8; 32]).verifying_key().as_bytes()
        );
        running.shutdown();
        running.wait().await.unwrap();
    }

    #[tokio::test]
    async fn spawn_rejects_bad_listen_and_missing_key() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Config {
            listen: "127.0.0.1:0".into(),
            ..Default::default()
        };
        let err = spawn(home.path(), cfg.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("reading"), "no key: {err}");
        Identity::from_seed([6u8; 32]).save(home.path()).unwrap();
        let bad = Config {
            listen: "nonsense".into(),
            ..cfg.clone()
        };
        let err = spawn(home.path(), bad).await.unwrap_err().to_string();
        assert!(err.contains("not host:port"), "{err}");
        // Port already taken → bind error surfaces, no panic.
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let busy = Config {
            listen: taken.local_addr().unwrap().to_string(),
            ..cfg
        };
        let err = format!("{:#}", spawn(home.path(), busy).await.unwrap_err());
        assert!(err.contains("binding 127.0.0.1:"), "{err}");
    }
}
