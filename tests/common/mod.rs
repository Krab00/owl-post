//! Shared integration helpers: a daemon with a temp home on 127.0.0.1:0, peer contacts with
//! optional policy overlays, pinned / unpinned reqwest clients, signed questions, and a
//! plain-HTTP iroh relay on 127.0.0.1:0 for the iroh tests.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use owlpost::config::Config;
use owlpost::contacts::{Mode, Policy, Scope};
use owlpost::daemon::{self, Running};
use owlpost::envelope::{Envelope, Payload};
use owlpost::identity::{Identity, fingerprint, pubkey_string};
use owlpost::spool::{Record, Spool};
use owlpost::tls::client_config;
use tempfile::TempDir;

pub const PROJECT: &str = "github.com/company/monorepo";
pub const PATH: &str = "src/auth/session.rs";

/// §1: the `🔑` standing wording a `consent` record carries for `name`, whose key is
/// a hand-added (global) contact — the shape every integration fixture here produces
/// (`write_contact` writes `source: "global"`). One definition, so the three surfaces
/// (`owl show --format claude`, `owl inbox --format claude`, the `owl route` wake file) are
/// checked against the same bytes.
pub fn key_standing(name: &str) -> String {
    format!(
        "known key: contact \"{name}\" — added by hand (global book; fingerprint not verified through a PR)"
    )
}

/// The session-start previews of every unseen inbox record of `home` (all from `name`,
/// all questions), in the order the hook lists them, closed by the open sentence:
/// `- <name> question [<state>] · <id>` — metadata only, never the path or the text.
pub fn previews(home: &Path, name: &str) -> String {
    let spool = Spool::new(home).unwrap();
    let mut lines: Vec<String> = spool
        .list(owlpost::spool::Dir::Inbox, |r| !r.seen)
        .unwrap()
        .into_iter()
        .map(|(id, r)| format!("- {name} question [{}] · {id}", r.state))
        .collect();
    lines.push("owlpost: tell the user in one line and offer /owlpost:inbox; run it only when the user asks.".to_string());
    lines.join("\n")
}

/// The `🔑` row itself, as it appears in the message table of a `consent` record: the first
/// body row, directly after the `|---|` rule. The three wordings contain no
/// `|`, so nothing is escaped here.
pub fn key_line(name: &str) -> String {
    format!("| 🔑 {} |", key_standing(name))
}

/// The process-wide `OWLPOST_CLAUDE_HOME`: a temp dir with an empty `sessions/`,
/// set once so no in-process daemon or spawned `owl` ever reads the real `~/.claude` and a
/// live Claude session on this machine can never influence a test. Every environment read
/// in the test binaries goes through `std::env`, whose lock serialises this single write.
pub fn claude_home() -> &'static Path {
    static DIR: OnceLock<TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
        // SAFETY: see above — nothing here reads the environment outside `std::env`.
        unsafe { std::env::set_var(owlpost::route::CLAUDE_HOME_ENV, dir.path()) };
        dir
    })
    .path()
}

pub fn id(seed: u8) -> Identity {
    Identity::from_seed([seed; 32])
}

/// Every file under `root` as `relative path → bytes` (A refused message must leave
/// the home byte-identical, a success must change exactly its target). `except` excludes
/// top-level files by name — a fixture daemon's own pull loop rewrites `daemon.status` on its
/// own schedule (through `daemon.status.tmp`), so a refusal test with a live daemon excludes
/// exactly those two.
pub fn snapshot(
    root: &Path,
    except: &[&str],
) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if except.iter().any(|x| rel == Path::new(x)) {
                continue;
            }
            out.insert(rel, std::fs::read(&path).unwrap());
        }
    }
    out
}

pub fn fp(id: &Identity) -> String {
    fingerprint(&id.verifying_key())
}

pub fn key(id: &Identity) -> [u8; 32] {
    *id.verifying_key().as_bytes()
}

pub fn policy(mode: Mode, rate_limit_per_hour: Option<u32>) -> Policy {
    Policy {
        mode,
        scope: Scope::default(),
        rate_limit_per_hour,
    }
}

/// A contact entry in the daemon's local provider: full contact plus optional policy overlay.
pub struct Peer<'a> {
    pub id: &'a Identity,
    pub name: &'a str,
    pub policy: Option<Policy>,
}

impl<'a> Peer<'a> {
    pub fn new(id: &'a Identity, name: &'a str, policy: Option<Policy>) -> Peer<'a> {
        Peer { id, name, policy }
    }
}

/// Writes `$home/contacts/<fingerprint>.json` in the `contacts::local` file shape
/// (email `<name lowercased>@example.org`, no endpoints).
pub fn write_contact(home: &Path, peer: &Peer<'_>) {
    let email = format!("{}@example.org", peer.name.to_lowercase());
    write_contact_full(home, peer, &[], &[&email]);
}

/// `write_contact` with explicit endpoints and emails (for the asker side of `owl ask`).
pub fn write_contact_full(home: &Path, peer: &Peer<'_>, endpoints: &[&str], emails: &[&str]) {
    let dir = home.join("contacts");
    std::fs::create_dir_all(&dir).unwrap();
    let mut v = serde_json::json!({
        "name": peer.name,
        "emails": emails,
        "pubkey": pubkey_string(&peer.id.verifying_key()),
        "endpoints": endpoints,
        "source": "global",
        "added_at": "2026-09-01T10:00:00Z",
    });
    if let Some(p) = &peer.policy {
        v["policy"] = serde_json::to_value(p).unwrap();
    }
    std::fs::write(
        dir.join(format!("{}.json", fp(peer.id))),
        serde_json::to_vec_pretty(&v).unwrap(),
    )
    .unwrap();
}

/// Prepares a home for `id`: key, config (`listen = 127.0.0.1:0`), contacts.
pub fn prepare_home(home: &Path, id: &Identity, responder_enabled: bool, peers: &[Peer<'_>]) {
    prepare_home_with(home, id, peers, |cfg| {
        cfg.responder.enabled = responder_enabled
    });
}

/// Like `prepare_home`, with a hook to tweak any config value before it is saved.
pub fn prepare_home_with(
    home: &Path,
    id: &Identity,
    peers: &[Peer<'_>],
    tweak: impl FnOnce(&mut Config),
) {
    std::fs::create_dir_all(home).unwrap();
    if !home.join("key").exists() {
        id.save(home).unwrap();
    }
    // `notify` is off in every fixture daemon so the suite never fires real desktop
    // notifications; `tests/notify.rs` opts in explicitly through `tweak`. `relay_urls` is
    // the empty list so no fixture daemon ever touches n0's public relays or DNS discovery
    // (the default `None` would): the iroh endpoint is bound but has no relay; the iroh
    // tests point it at `relay()` through `tweak`.
    let mut cfg = Config {
        name: "Bea".into(),
        listen: "127.0.0.1:0".into(),
        notify: false,
        relay_urls: Some(vec![]),
        ..Default::default()
    };
    // The daemon refuses a question about a project it does not map (§7), so every
    // fixture knows [`PROJECT`]; the checkout does not exist unless a test maps a real one.
    cfg.projects.insert(
        PROJECT.into(),
        home.join("no-checkout").to_string_lossy().into_owned(),
    );
    tweak(&mut cfg);
    cfg.save(home).unwrap();
    for p in peers {
        write_contact(home, p);
    }
}

pub struct TestDaemon {
    pub dir: TempDir,
    pub id: Identity,
    pub addr: SocketAddr,
    pub running: Running,
}

impl TestDaemon {
    pub fn home(&self) -> &Path {
        self.dir.path()
    }

    pub fn spool(&self) -> Spool {
        Spool::new(self.home()).unwrap()
    }

    pub fn fp(&self) -> String {
        fp(&self.id)
    }

    pub fn url(&self, path: &str) -> String {
        format!("https://{}{}", self.addr, path)
    }
}

/// Spawns an in-process daemon for `seed` with the given contacts.
pub async fn spawn_daemon(seed: u8, responder_enabled: bool, peers: &[Peer<'_>]) -> TestDaemon {
    spawn_daemon_with(seed, peers, |cfg| cfg.responder.enabled = responder_enabled).await
}

/// Spawns a daemon whose config was adjusted by `tweak` (non-default values under test).
pub async fn spawn_daemon_with(
    seed: u8,
    peers: &[Peer<'_>],
    tweak: impl FnOnce(&mut Config),
) -> TestDaemon {
    let dir = tempfile::tempdir().unwrap();
    let id = id(seed);
    prepare_home_with(dir.path(), &id, peers, tweak);
    respawn(dir, id).await
}

/// (Re)starts a daemon on an already prepared home, e.g. after `running.shutdown()`. The
/// daemon's session liveness lookup is pointed at [`claude_home`], never the real one.
pub async fn respawn(dir: TempDir, id: Identity) -> TestDaemon {
    claude_home();
    let cfg = Config::load(dir.path()).unwrap();
    let running = daemon::spawn(dir.path(), cfg).await.unwrap();
    TestDaemon {
        addr: running.addr,
        dir,
        id,
        running,
    }
}

/// A plain-HTTP iroh relay on 127.0.0.1:0 (no TLS, no QUIC, no n0 infrastructure). The
/// returned URL goes into `config.relay_urls`; the server stops when dropped.
pub async fn relay() -> (String, iroh_relay::server::Server) {
    use iroh_relay::server::{RelayConfig, Server, ServerConfig};
    let mut config = ServerConfig::default();
    config.relay = Some(RelayConfig::new((std::net::Ipv4Addr::LOCALHOST, 0)));
    config.quic = None;
    let server = Server::spawn(config).await.expect("test relay");
    let addr = server.http_addr().expect("http relay bound");
    (format!("http://{addr}/"), server)
}

/// Bounded wait (≤ 10 s) until the daemon's iroh endpoint is connected to its home relay.
pub async fn wait_online(d: &TestDaemon) {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        d.running.iroh().online(),
    )
    .await
    .expect("iroh endpoint never reached its relay");
}

/// reqwest client pinned to `server`'s key; `client = Some(id)` presents a certificate.
pub fn client(client: Option<&Identity>, server: &Identity) -> reqwest::Client {
    let cfg = client_config(client, Some(key(server))).unwrap();
    reqwest::Client::builder()
        .use_preconfigured_tls(Arc::unwrap_or_clone(cfg))
        .build()
        .unwrap()
}

/// Spool record for an envelope in `state`, no meta.
pub fn record(env: &Envelope, state: &str) -> Record {
    Record {
        raw: env.raw.clone(),
        sig: env.sig.clone(),
        state: state.into(),
        seen: false,
        received_at: owlpost::envelope::rfc3339_now(),
        draft: None,
        meta: serde_json::Value::Null,
    }
}

pub fn question(from: &Identity, to: &Identity, text: &str) -> Payload {
    Payload::question(&fp(from), &fp(to), PROJECT, Some(PATH), text)
}

pub fn signed(from: &Identity, to: &Identity, text: &str) -> Envelope {
    Envelope::sign(&question(from, to, text), from)
}

/// `POST /v1/questions` with the envelope's raw body and signature header.
pub async fn post_envelope(
    client: &reqwest::Client,
    daemon: &TestDaemon,
    env: &Envelope,
) -> reqwest::Response {
    post_raw(client, daemon, env.raw.clone().into_bytes(), Some(&env.sig)).await
}

pub async fn post_raw(
    client: &reqwest::Client,
    daemon: &TestDaemon,
    body: Vec<u8>,
    sig: Option<&str>,
) -> reqwest::Response {
    let mut req = client
        .post(daemon.url("/v1/questions"))
        .header("content-type", "application/json")
        .body(body);
    if let Some(s) = sig {
        req = req.header("X-Owl-Signature", s);
    }
    req.send().await.unwrap()
}

/// Asserts a JSON `{"error": ...}` body with the given status and exact error string.
pub async fn assert_error(
    resp: reqwest::Response,
    status: u16,
    error: &str,
) -> reqwest::header::HeaderMap {
    assert_eq!(resp.status().as_u16(), status, "status for {error:?}");
    let headers = resp.headers().clone();
    let body: serde_json::Value = resp.json().await.expect("error body must be JSON");
    assert_eq!(body.get("error").and_then(|e| e.as_str()), Some(error));
    headers
}

/// A fake `claude` (and `cargo`) on a temp `PATH` for the `owl setup` / `owl update` /
/// `owl doctor` MCP registration tests: every call appends `<name> <argv>` to `calls.log`
/// and exits 0, except `claude mcp get owl`, which exits `get_exit` (0 = registered). A call
/// carrying `--root <dir>` (`cargo install`) leaves `fake owl` at `<dir>/bin/owl`.
/// Returns the bin directory (to be the whole `PATH`) and the log path.
pub fn fake_claude(dir: &Path, get_exit: i32) -> (std::path::PathBuf, std::path::PathBuf) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.join("calls.log");
    for name in ["claude", "cargo"] {
        let script = bin.join(name);
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"{name} $*\" >> '{}'\n\
                 [ \"$1 $2 $3\" = \"mcp get owl\" ] && exit {get_exit}\n\
                 root=''; while [ $# -gt 0 ]; do [ \"$1\" = --root ] && root=\"$2\"; shift; done\n\
                 [ -z \"$root\" ] || {{ /bin/mkdir -p \"$root/bin\" && printf 'fake owl\\n' > \"$root/bin/owl\"; }}\n\
                 exit 0\n",
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    (bin, log)
}

/// The lines of a `fake_claude` log (empty when nothing ran).
pub fn fake_calls(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}
