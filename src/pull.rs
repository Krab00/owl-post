//! Pull loop (architecture §3.5): every `pull_interval_secs` the daemon groups `asks/` by
//! responder, fetches each responder's outbox, verifies and ingests the answers to its open
//! asks, acks them, expires its own stale outbox entries and writes `daemon.status`.
//!
//! `owl ask --wait` runs the same ingestion (`ingest_envelope`) for a single ask.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{self, Iroh};
use crate::config::Config;
use crate::contacts::{Contact, ContactBook};
use crate::envelope::{self, Body, Envelope, Kind, Payload};
use crate::identity::Identity;
use crate::server::{AnswerIngested, AppState, DaemonEvent, Declined, on_pull_event};
use crate::spool::{Dir, Record, Spool};

/// `$OWLPOST_HOME/daemon.status`, rewritten after every loop.
pub const STATUS_FILE: &str = "daemon.status";
/// How long a responder whose last probe failed is left alone.
pub const PROBE_SKIP: Duration = Duration::from_secs(60);
/// At most this many presence-probe threads run at once: N contacts never mean N threads.
const PROBE_WORKERS: usize = 8;
/// No presence probe — and no endpoint attempt inside one — starts after this budget: the
/// existing per-request timeout.
const PROBE_BUDGET: Duration = client::REQUEST_TIMEOUT;
/// How often the trash GC runs: at daemon start and then at most once a day, not every loop.
const GC_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Written after each loop so `owl doctor` can report the last pull.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullStatus {
    /// RFC 3339 UTC time the loop finished.
    pub last_pull_at: String,
    /// Asks still waiting after this loop.
    pub open_asks: usize,
    /// Contacts probed this loop — every responder with an open ask plus every other contact
    /// of the book (reachable or not); skipped ones are not counted.
    pub peers_probed: usize,
    /// The last probe result per probed contact, kept across loops (what
    /// `owl presence` shows). Absent in older status files.
    #[serde(default)]
    pub peers: BTreeMap<String, PeerProbe>,
}

/// The last probe of one responder as `daemon.status` reports it: `probed_at` is
/// the RFC 3339 UTC wall clock of the last probe, `last_seen` of the last successful one —
/// kept across later failed probes, `None` while the peer was never reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerProbe {
    pub online: bool,
    pub probed_at: String,
    pub last_seen: Option<String>,
}

/// Outcome of the last probe of one responder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub last_probe: Instant,
    pub ok: bool,
}

/// In-memory liveness cache: a responder whose last probe failed is skipped for `skip`.
#[derive(Debug)]
pub struct Liveness {
    skip: Duration,
    map: HashMap<String, Probe>,
    peers: BTreeMap<String, PeerProbe>,
}

impl Liveness {
    pub fn new(skip: Duration) -> Liveness {
        Liveness {
            skip,
            map: HashMap::new(),
            peers: BTreeMap::new(),
        }
    }

    /// The daemon's skip window: 60 s, capped at one pull interval, minus one second so the
    /// re-probe never depends on tick jitter (two ticks are at least `interval` apart, and
    /// `now` is sampled a little after each one).
    // ponytail: `PROBE_SKIP` capped at `interval - 1 s` — with a 1 s interval an offline peer
    // is retried every loop, with the default 60 s interval on the next loop. Raise the cap
    // (or count loops instead of seconds) if short intervals should back off further.
    pub fn skip_for(interval: Duration) -> Duration {
        PROBE_SKIP
            .min(interval)
            .saturating_sub(Duration::from_secs(1))
    }

    /// True while the last probe failed less than `skip` ago; a peer never probed, or whose
    /// last probe succeeded, is never skipped.
    pub fn should_skip(&self, fingerprint: &str, now: Instant) -> bool {
        self.map
            .get(fingerprint)
            .is_some_and(|p| !p.ok && now.duration_since(p.last_probe) < self.skip)
    }

    pub fn record(&mut self, fingerprint: &str, ok: bool, at: Instant) {
        self.map
            .insert(fingerprint.to_string(), Probe { last_probe: at, ok });
        let now = envelope::rfc3339_now();
        let probe = self
            .peers
            .entry(fingerprint.to_string())
            .or_insert(PeerProbe {
                online: ok,
                probed_at: now.clone(),
                last_seen: None,
            });
        probe.online = ok;
        probe.probed_at = now.clone();
        if ok {
            probe.last_seen = Some(now);
        }
    }

    pub fn get(&self, fingerprint: &str) -> Option<Probe> {
        self.map.get(fingerprint).copied()
    }

    /// The last probe result per responder, for `PullStatus.peers`.
    pub fn peers(&self) -> BTreeMap<String, PeerProbe> {
        self.peers.clone()
    }
}

/// What the loop needs from an `asks/` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAsk {
    pub id: String,
    /// Responder fingerprint: `meta.peer`, else the question's `to`.
    pub peer: String,
    /// Asker-cache key: `meta.hash`, else recomputed from the question.
    pub hash: String,
    /// Path the question was about (for the notification); `-` for a repo-level question.
    pub path: String,
    /// The question carried a context snippet or continued a thread (`meta.threaded`):
    /// its answer is stored but never written to the asker cache.
    pub threaded: bool,
}

/// Parses one `asks/` record; `None` (warned) when `raw` is not a question payload.
pub fn open_ask(id: &str, rec: &Record) -> Option<OpenAsk> {
    let payload: Payload = match serde_json::from_str(&rec.raw) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(id, error = %e, "skipping ask: raw is not a payload");
            return None;
        }
    };
    // A content ask has no question hash at all — it is keyed by ref and by the
    // owner's consent — so it carries an empty one and is never cached in either direction.
    let question = match &payload.body {
        Body::Question {
            project,
            path,
            question,
            ..
        } => Some((project.clone(), path.clone(), question.clone())),
        Body::Content { .. } if payload.kind == crate::envelope::Kind::Content => None,
        // A tool-call ask has no question hash either, for the same reason.
        Body::ToolCall { .. } if payload.kind == crate::envelope::Kind::ToolCall => None,
        _ => {
            tracing::warn!(id, "skipping ask: payload is not a question");
            return None;
        }
    };
    let meta_str = |key: &str| {
        rec.meta
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Some(OpenAsk {
        id: id.to_string(),
        peer: meta_str("peer").unwrap_or_else(|| payload.to.clone()),
        hash: meta_str("hash").unwrap_or_else(|| match &question {
            Some((project, path, question)) => {
                envelope::question_hash(project, path.as_deref(), question)
            }
            None => String::new(),
        }),
        path: match (&question, &payload.body) {
            (Some((_, path, _)), _) => path.clone(),
            (None, Body::Content { path, .. }) => path.clone(),
            (None, _) => None,
        }
        .unwrap_or_else(|| "-".to_string()),
        threaded: rec
            .meta
            .get("threaded")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// Every parseable record in `asks/`, keyed by id; corrupt files are warned about and skipped.
pub fn open_asks(spool: &Spool) -> anyhow::Result<BTreeMap<String, OpenAsk>> {
    Ok(spool
        .list_lenient(Dir::Asks)?
        .iter()
        .filter_map(|(id, rec)| open_ask(id, rec))
        .map(|ask| (ask.id.clone(), ask))
        .collect())
}

/// Verified answer → `inbox/<answer id>` (state `pending`, unseen, `meta = {peer, hash,
/// in_reply_to}`) and, when `cache` (the question was not threaded), the asker
/// cache under `hash`.
pub fn store_answer(
    spool: &Spool,
    env: &Envelope,
    answer: &Payload,
    peer: &str,
    hash: &str,
    question_id: &str,
    cache: bool,
) -> anyhow::Result<()> {
    let mut rec = Record {
        raw: env.raw.clone(),
        sig: env.sig.clone(),
        state: "pending".into(),
        seen: false,
        received_at: envelope::rfc3339_now(),
        draft: None,
        meta: json!({ "peer": peer, "hash": hash, "in_reply_to": question_id }),
    };
    // Record birth: the peer's answer landing here. A content reply
    // names its own kind, so `owl thread` shows the content leg of the exchange.
    let kind = match answer.kind {
        Kind::ContentReply => "content-received",
        Kind::ToolReply => "tool-received",
        _ => "answer-received",
    };
    crate::events::push(&mut rec, kind, None, None);
    // The id is the peer's; a writer racing this one for it loses instead of
    // replacing the record.
    spool.put_new(Dir::Inbox, &answer.id, &rec)?;
    if cache {
        spool.cache_put(hash, &rec)?;
    }
    Ok(())
}

/// What a peer's answer id meets in this home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerId {
    New,
    Stored,
    Refused,
}

/// `Refused` when the id is not a plain record id (`spool::is_record_id`), equals the answer's own `in_reply_to`, or
/// names a record in inbox/, outbox/, asks/ or done/ that is not this very answer — a
/// peer-chosen id must not replace or shadow a record, and a case variant names the same file
/// on a case-insensitive filesystem. `Stored` when every record under the id IS this answer
/// (the same signed raw): a re-offer of an answer already written, e.g. by a crash between
/// the inbox write and the ask move. `New` otherwise. An unreadable or unparseable file
/// counts as NOT the same answer (→ `Refused`).
pub fn answer_id(spool: &Spool, env: &Envelope, answer: &Payload) -> anyhow::Result<AnswerId> {
    // The id becomes a file name: only the record-id shape a question id must have
    // (`server::validate_request`).
    if !crate::spool::is_record_id(&answer.id)
        || answer.in_reply_to.as_deref() == Some(answer.id.as_str())
    {
        return Ok(AnswerId::Refused);
    }
    let found = spool.same_id(&answer.id)?;
    if found.is_empty() {
        return Ok(AnswerId::New);
    }
    let same_answer = |path: &Path| {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
            .is_some_and(|rec| rec.raw == env.raw)
    };
    if found.iter().all(|p| same_answer(p)) {
        Ok(AnswerId::Stored)
    } else {
        Ok(AnswerId::Refused)
    }
}

/// An answer that went through `ingest_envelope`.
#[derive(Debug)]
pub struct Ingested {
    pub ask: OpenAsk,
    pub answer: Payload,
    /// `Some` when the ack did not reach the responder; the answer is stored either way.
    pub ack_error: Option<anyhow::Error>,
}

/// Per-envelope verdict of `ingest_envelope`.
#[derive(Debug)]
pub enum Verdict {
    Ingested(Box<Ingested>),
    /// Signature (or shape) did not verify against the responder's key: warned, not acked.
    Forged,
    /// Verified, but `in_reply_to` is none of `open`: left on the responder, not acked.
    Unrelated,
    /// Its id starts with `-`, equals its `in_reply_to` or names another record here:
    /// dropped unwritten, not acked.
    Refused,
}

/// One outbox envelope from `contact` against the open asks: verify with the pinned key,
/// match `in_reply_to` against the asks addressed to *this* contact (an answer B signs to a
/// question asked of C is `Unrelated`), store in `inbox/` + cache, move the ask to `done/`
/// (`answered`), ack.
///
/// The inbox write comes before the ask move so a crash in between leaves the ask open; the
/// next pull then finds the same answer already stored and writes nothing before
/// retrying the move.
// ponytail: an answer whose ack failed is re-served by the responder until its TTL, and every
// later pull sees it as `Unrelated` (the ask is closed by then) — re-ack from `done/` if that
// churn ever matters.
pub fn ingest_envelope(
    identity: &Identity,
    contact: &Contact,
    iroh: &Iroh,
    spool: &Spool,
    open: &BTreeMap<String, OpenAsk>,
    env: &Envelope,
) -> anyhow::Result<Verdict> {
    let answer = match client::verify_answer(contact, env, None) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                peer = %contact.fingerprint,
                claimed_id = %claimed_id(env),
                error = %format!("{e:#}"),
                "dropping outbox entry: signature does not verify"
            );
            return Ok(Verdict::Forged);
        }
    };
    let ask = answer
        .in_reply_to
        .as_deref()
        .and_then(|q| open.get(q))
        .filter(|ask| ask.peer == contact.fingerprint);
    let Some(ask) = ask else {
        tracing::debug!(
            peer = %contact.fingerprint,
            id = %answer.id,
            in_reply_to = ?answer.in_reply_to,
            "outbox entry replies to no open ask to this peer; left unacked"
        );
        return Ok(Verdict::Unrelated);
    };
    // The peer picked the answer's id; it must not replace or shadow a record here.
    match answer_id(spool, env, &answer)? {
        AnswerId::Refused => {
            tracing::warn!(peer = %contact.fingerprint, id = %answer.id, "dropping outbox entry: its id collides with a record here");
            return Ok(Verdict::Refused);
        }
        // A re-offer of the answer already written (a crash took the ask move): nothing to
        // write, carry on as if stored.
        AnswerId::Stored => {}
        AnswerId::New => {
            // A content reply never enters the asker's cache — the cache answers
            // questions by their text, and content is keyed by ref and by the owner's consent.
            let cache = !ask.threaded && answer.kind == Kind::Answer && !ask.hash.is_empty();
            store_answer(
                spool,
                env,
                &answer,
                &contact.fingerprint,
                &ask.hash,
                &ask.id,
                cache,
            )?;
        }
    }
    spool.move_to(Dir::Asks, &ask.id, Dir::Done)?;
    spool.set_state_with_event(
        Dir::Done,
        &ask.id,
        "answered",
        "answer-received",
        None,
        None,
    )?;
    let ack_error = client::ack(identity, contact, iroh, &answer.id).err();
    Ok(Verdict::Ingested(Box::new(Ingested {
        ask: ask.clone(),
        answer,
        ack_error,
    })))
}

/// The `id` an unverified envelope claims, for the log line only.
fn claimed_id(env: &Envelope) -> String {
    serde_json::from_str::<Value>(&env.raw)
        .ok()
        .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "?".to_string())
}

/// Records the peer's Task state on the still-open ask `id`: one `state-seen`
/// event, collapsed by `events::push` when the state has not moved since the last one.
/// Best effort — the ask may have been closed between the fetch and this write.
pub fn note_task_state(spool: &Spool, id: &str, task: &client::Task) {
    if let Err(e) = spool.push_event(
        Dir::Asks,
        id,
        crate::events::STATE_SEEN,
        Some("peer"),
        Some(json!({ "state": task.state, "text": task.text })),
    ) {
        tracing::debug!(id, error = %format!("{e:#}"), "recording the peer's task state failed");
    }
}

/// Closes ask `id` as declined: the `asks/` record moves to `done/` with state
/// `declined`. Move first, like the ack handler, so a failed move leaves the ask open.
pub fn close_declined(spool: &Spool, id: &str) -> anyhow::Result<()> {
    spool.move_to(Dir::Asks, id, Dir::Done)?;
    spool.set_state_with_event(Dir::Done, id, "declined", "declined", Some("peer"), None)
}

/// Oldest-probe-first: contacts never probed come first (`None` sorts before any `Some`),
/// then by `last_probe` ascending — the contacts a probe budget skips this loop are first
/// next loop.
fn oldest_first(contacts: &mut Vec<&Contact>, liveness: &Liveness) {
    contacts.sort_by_key(|c| liveness.get(&c.fingerprint).map(|p| p.last_probe));
}

/// Probes `contacts` with the same `fetch_outbox` call a responder pull uses, on at most
/// `workers` std threads that pull the next contact off a shared index. No new probe, and no
/// new endpoint attempt inside a running one, is STARTED once `budget` has elapsed, so the
/// call lasts at most the budget plus one in-flight attempt; the contacts that did not run
/// simply have no entry in the result (one `(fingerprint, online)` per probe that ran) and
/// keep their last liveness result. A panicking worker is contained: it does not propagate,
/// its unfinished contacts just have no result.
fn probe_contacts(
    contacts: &[&Contact],
    identity: &Identity,
    iroh: &Iroh,
    workers: usize,
    budget: Duration,
) -> Vec<(String, bool)> {
    let started = Instant::now();
    let deadline = started + budget;
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    // `client::via_iroh` with `Iroh::Direct` re-enters the async runtime through
    // `Handle::current()`, which panics on a plain std thread: carry the caller's handle (when
    // there is one) into each worker and enter it there.
    let rt = tokio::runtime::Handle::try_current().ok();
    std::thread::scope(|s| {
        let (next, results) = (&next, &results);
        let workers: Vec<_> = (0..workers.min(contacts.len()))
            .map(|_| {
                let rt = rt.clone();
                s.spawn(move || {
                    let _guard = rt.as_ref().map(|h| h.enter());
                    while started.elapsed() < budget {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(contact) = contacts.get(i) else {
                            break;
                        };
                        let ok =
                            client::fetch_outbox_until(identity, contact, iroh, Some(deadline))
                                .is_ok();
                        results
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push((contact.fingerprint.clone(), ok));
                    }
                })
            })
            .collect();
        for w in workers {
            let _ = w.join();
        }
    });
    results.into_inner().unwrap_or_else(|e| e.into_inner())
}

/// One pull over every responder with an open ask (iroh first, then `endpoints`, see
/// `client`). Contacts are reloaded on each call so an endpoint edit is picked up without
/// a restart. For every ask still open to a responder that was reached, the peer's Task
/// is fetched too: a `REJECTED` one closes the ask as `done/declined` and
/// reports `DaemonEvent::Declined`. Returns the status to write.
///
/// Every OTHER contact of the book is probed too, with the same `fetch_outbox` call, so
/// `daemon.status` covers the whole book: presence is about the book, not about open asks,
/// and a contact with policy `never` is probed like any contact. Only ok/err is recorded
/// into the liveness cache; the fetched items are discarded (there is no ask to match them
/// against). The probes run on at most `PROBE_WORKERS` probe threads plus one coordinating
/// thread, concurrently with the responder loop, and no new probe or endpoint attempt is
/// started once `PROBE_BUDGET` has elapsed — contacts the budget skips keep their last result,
/// are not counted in `peers_probed` and, the probes being ordered oldest-first, are first
/// next loop. Worst case the probe phase adds the budget (30 s) plus one in-flight attempt
/// (iroh dial 10 s, or one endpoint's connect 2 s / request 30 s) to a loop, whatever N is
/// and however many endpoints a contact lists.
#[allow(clippy::too_many_arguments)]
pub fn pull_once(
    home: &Path,
    cwd: &Path,
    identity: &Identity,
    iroh: &Iroh,
    spool: &Spool,
    liveness: &mut Liveness,
    now: Instant,
    mut on_event: impl FnMut(DaemonEvent),
) -> anyhow::Result<PullStatus> {
    let mut open = open_asks(spool)?;
    let book = ContactBook::load(home, cwd)?;
    let mut by_peer: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for ask in open.values() {
        by_peer
            .entry(ask.peer.clone())
            .or_default()
            .push(ask.id.clone());
    }
    let mut peers_probed = 0;
    // The presence probes: every contact of the book whose fingerprint has no open ask,
    // deduplicated by fingerprint, skip-windowed like a responder. Contacts with policy
    // `never` are probed like any contact — presence is about the book.
    let mut seen = HashSet::new();
    let mut extra: Vec<&Contact> = book
        .contacts
        .iter()
        .filter(|c| !by_peer.contains_key(&c.fingerprint))
        .filter(|c| seen.insert(&c.fingerprint))
        .filter(|c| !liveness.should_skip(&c.fingerprint, now))
        .collect();
    oldest_first(&mut extra, liveness);
    // The handle must be captured HERE, on the caller's thread: the probe phase below runs on
    // a plain std thread, where `Handle::try_current()` finds nothing.
    let rt = tokio::runtime::Handle::try_current().ok();
    std::thread::scope(|s| {
        // On its own thread so the bounded probe phase runs concurrently with the ask loop;
        // entering the runtime there lets `probe_contacts` hand the handle to its workers.
        let probes = s.spawn(move || {
            let _guard = rt.as_ref().map(|h| h.enter());
            probe_contacts(&extra, identity, iroh, PROBE_WORKERS, PROBE_BUDGET)
        });
        for peer in by_peer.keys() {
            let Some(contact) = book.contacts.iter().find(|c| &c.fingerprint == peer) else {
                tracing::warn!(peer, "open ask to an unknown contact; not pulled");
                continue;
            };
            if liveness.should_skip(peer, now) {
                tracing::debug!(peer, "responder offline recently; skipped");
                continue;
            }
            peers_probed += 1;
            let items = match client::fetch_outbox(identity, contact, iroh) {
                Ok(items) => {
                    liveness.record(peer, true, now);
                    tracing::debug!(peer, count = items.len(), "outbox fetched");
                    items
                }
                Err(e) => {
                    liveness.record(peer, false, now);
                    tracing::warn!(peer, error = %format!("{e:#}"), "outbox fetch failed");
                    continue;
                }
            };
            for env in &items {
                match ingest_envelope(identity, contact, iroh, spool, &open, env) {
                    Ok(Verdict::Ingested(ing)) => {
                        if let Some(e) = &ing.ack_error {
                            tracing::warn!(peer, id = %ing.answer.id, error = %format!("{e:#}"), "ack failed");
                        }
                        // The pulled answer is a new inbox record: wake one session.
                        match Config::load(home)
                            .and_then(|cfg| crate::route::route(home, &cfg, spool, &ing.answer.id))
                        {
                            Ok(Some(sid)) => {
                                tracing::info!(id = %ing.answer.id, session = %sid, "routed to session")
                            }
                            Ok(None) => {
                                tracing::debug!(id = %ing.answer.id, "no live session to wake")
                            }
                            Err(e) => {
                                tracing::warn!(id = %ing.answer.id, error = %format!("{e:#}"), "routing failed")
                            }
                        }
                        open.remove(&ing.ask.id);
                        on_event(DaemonEvent::Answer(AnswerIngested {
                            id: ing.ask.id.clone(),
                            peer: peer.clone(),
                            path: ing.ask.path.clone(),
                        }));
                    }
                    Ok(Verdict::Forged | Verdict::Unrelated | Verdict::Refused) => {}
                    Err(e) => {
                        tracing::warn!(peer, error = %format!("{e:#}"), "ingesting answer failed");
                    }
                }
            }
            // The responder is online: ask it where every still-open ask stands. A
            // fetch error is logged and the ask stays open; only `REJECTED` closes it.
            let still_open: Vec<String> = open
                .values()
                .filter(|a| &a.peer == peer)
                .map(|a| a.id.clone())
                .collect();
            for id in still_open {
                match client::fetch_task(identity, contact, iroh, &id) {
                    Ok(Some(task)) if task.rejected() => match close_declined(spool, &id) {
                        Ok(()) => {
                            tracing::info!(peer, id, text = %task.text, "question declined");
                            open.remove(&id);
                            on_event(DaemonEvent::Declined(Declined {
                                id,
                                peer: peer.clone(),
                            }));
                        }
                        Err(e) => {
                            tracing::warn!(peer, id, error = %format!("{e:#}"), "closing declined ask failed")
                        }
                    },
                    Ok(Some(task)) => {
                        note_task_state(spool, &id, &task);
                        tracing::debug!(peer, id, state = %task.state, "task state");
                    }
                    Ok(None) => tracing::debug!(peer, id, "peer holds no task for the ask"),
                    Err(e) => {
                        tracing::warn!(peer, id, error = %format!("{e:#}"), "task fetch failed")
                    }
                }
            }
        }
        // A panicking probe phase cannot take pull_once down: its unfinished contacts simply
        // have no result this loop and keep their last liveness entry.
        for (fp, ok) in probes.join().unwrap_or_default() {
            liveness.record(&fp, ok, now);
            peers_probed += 1;
        }
    });
    Ok(PullStatus {
        last_pull_at: envelope::rfc3339_now(),
        open_asks: open.len(),
        peers_probed,
        // Peers skipped this loop keep their last result: the liveness cache holds them all.
        peers: liveness.peers(),
    })
}

/// Moves every `outbox/` record older than `ttl_days` (strictly: age > TTL) to `done/` with
/// state `expired`. Returns how many moved. A record whose `received_at` does not parse is
/// kept and warned about.
pub fn expire_outbox(spool: &Spool, ttl_days: u64, now_unix: u64) -> anyhow::Result<usize> {
    let ttl_secs = ttl_days.saturating_mul(86_400);
    let mut expired = 0;
    for (id, rec) in spool.list_lenient(Dir::Outbox)? {
        let Some(received) = envelope::parse_rfc3339_to_unix(&rec.received_at) else {
            tracing::warn!(id, received_at = %rec.received_at, "outbox record has no usable received_at; kept");
            continue;
        };
        if now_unix.saturating_sub(received) <= ttl_secs {
            continue;
        }
        // Move first, like the ack handler: a failed move must not leave `expired` in outbox/.
        match spool.move_to(Dir::Outbox, &id, Dir::Done).and_then(|_| {
            spool.set_state_with_event(Dir::Done, &id, "expired", "expired", None, None)
        }) {
            Ok(()) => {
                tracing::info!(id, "outbox entry expired");
                expired += 1;
            }
            Err(e) => tracing::warn!(id, error = %format!("{e:#}"), "expiring outbox entry failed"),
        }
    }
    Ok(expired)
}

/// Atomically writes `<home>/daemon.status` (temp file + rename).
pub fn write_status(home: &Path, status: &PullStatus) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(status).context("serialising daemon.status")?;
    bytes.push(b'\n');
    crate::daemon::write_atomic(home, STATUS_FILE, &bytes)
}

/// Parses `<home>/daemon.status`; `Ok(None)` when the file does not exist.
pub fn read_status(home: &Path) -> anyhow::Result<Option<PullStatus>> {
    let path = home.join(STATUS_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The loop period: `pull_interval_secs`, floored at 1 s (a zero interval would spin).
pub fn loop_interval(config: &Config) -> Duration {
    Duration::from_secs(config.pull_interval_secs.max(1))
}

/// One loop iteration: pull, expire, write the status file. Never panics on I/O.
pub fn tick(state: &AppState, liveness: &mut Liveness) {
    match expire_outbox(
        &state.spool,
        state.config.outbox_ttl_days,
        envelope::now_unix(),
    ) {
        Ok(0) => {}
        Ok(n) => tracing::info!(count = n, "expired outbox entries"),
        Err(e) => tracing::warn!(error = %format!("{e:#}"), "outbox expiry failed"),
    }
    let status = match pull_once(
        &state.home,
        &state.cwd,
        &state.identity,
        &Iroh::for_daemon(state),
        &state.spool,
        liveness,
        Instant::now(),
        |ev| on_pull_event(state, ev),
    ) {
        Ok(status) => status,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "pull failed");
            return;
        }
    };
    tracing::debug!(
        open_asks = status.open_asks,
        peers_probed = status.peers_probed,
        "pulled"
    );
    if let Err(e) = write_status(&state.home, &status) {
        tracing::warn!(error = %format!("{e:#}"), "writing daemon.status failed");
    }
}

/// The trash GC is due at daemon start (`last` is `None`) and then once `GC_EVERY` has
/// passed since the last run.
fn gc_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= GC_EVERY)
}

/// Runs `tick` immediately and then every `loop_interval`, off the async runtime (the HTTP
/// client is blocking). The trash GC (`crate::daemon::gc_trash`) runs before `tick` at start
/// and then at most once a day. Ends only when the task is aborted.
pub async fn run_loop(state: Arc<AppState>) {
    let interval = loop_interval(&state.config);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut liveness = Liveness::new(Liveness::skip_for(interval));
    let mut last_gc: Option<Instant> = None;
    loop {
        ticker.tick().await;
        let st = state.clone();
        let mut lv = liveness;
        let mut gc = last_gc;
        liveness = match tokio::task::spawn_blocking(move || {
            if gc_due(gc, Instant::now()) {
                let removed = crate::daemon::gc_trash(&st.home, SystemTime::now());
                if removed > 0 {
                    tracing::info!(count = removed, "removed old trash batches");
                }
                gc = Some(Instant::now());
            }
            tick(&st, &mut lv);
            (lv, gc)
        })
        .await
        {
            Ok((lv, gc)) => {
                last_gc = gc;
                lv
            }
            Err(e) => {
                tracing::error!(error = %e, "pull tick panicked; liveness cache reset");
                Liveness::new(Liveness::skip_for(interval))
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity;

    fn rec(env: &Envelope, state: &str, received_at: &str, meta: Value) -> Record {
        Record {
            raw: env.raw.clone(),
            sig: env.sig.clone(),
            state: state.into(),
            seen: false,
            received_at: received_at.into(),
            draft: None,
            meta,
        }
    }

    fn question(a: &Identity, b: &Identity) -> Payload {
        Payload::question(
            &identity::fingerprint(&a.verifying_key()),
            &identity::fingerprint(&b.verifying_key()),
            "proj",
            Some("src/x.rs"),
            "why?",
        )
    }

    fn write_contact(
        home: &Path,
        id: &Identity,
        name: &str,
        endpoints: &[&str],
        policy: Option<&str>,
    ) {
        let dir = home.join("contacts");
        std::fs::create_dir_all(&dir).unwrap();
        let mut v = json!({
            "name": name,
            "emails": [],
            "pubkey": identity::pubkey_string(&id.verifying_key()),
            "endpoints": endpoints,
            "source": "global",
        });
        if let Some(mode) = policy {
            v["policy"] = json!({ "mode": mode });
        }
        std::fs::write(
            dir.join(format!(
                "{}.json",
                identity::fingerprint(&id.verifying_key())
            )),
            serde_json::to_vec(&v).unwrap(),
        )
        .unwrap();
    }

    fn closed_port() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    }

    /// A listener whose connections are accepted, held ~1 s and dropped (a thread per
    /// connection), tracking the max number of simultaneously open ones: a probe of it takes
    /// ~1 s, and `max` shows how many ran in parallel.
    fn gated_endpoint() -> (String, Arc<AtomicUsize>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let max = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(AtomicUsize::new(0));
        let (mx, cur) = (Arc::clone(&max), Arc::clone(&current));
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let (mx, cur) = (Arc::clone(&mx), Arc::clone(&cur));
                std::thread::spawn(move || {
                    let open = cur.fetch_add(1, Ordering::SeqCst) + 1;
                    mx.fetch_max(open, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_secs(1));
                    cur.fetch_sub(1, Ordering::SeqCst);
                    drop(stream);
                });
            }
        });
        (addr, max)
    }

    /// A listener that holds every accepted connection until `target` connections are open
    /// at once — the barrier that releases them all — or `cap` has passed since its own
    /// accept (so a test that never reaches the target fails instead of hanging), tracking
    /// the max simultaneously open. A probe of it hangs until the barrier releases, so `max`
    /// reaching `target` PROVES that many probes were open at the same time. `cap` must stay
    /// under the client's connect timeout (CONNECT_TIMEOUT, 2 s): a client whose held TLS
    /// handshake times out drops its end and retries, and the retry would count as an extra
    /// simultaneously open connection the barrier could then wrongly credit.
    fn barrier_endpoint(target: usize, cap: Duration) -> (String, Arc<AtomicUsize>) {
        assert!(
            cap < client::CONNECT_TIMEOUT,
            "the cap must beat the client's connect timeout: {cap:?}"
        );
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let max = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(AtomicUsize::new(0));
        let (mx, cur) = (Arc::clone(&max), Arc::clone(&current));
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let (mx, cur) = (Arc::clone(&mx), Arc::clone(&cur));
                std::thread::spawn(move || {
                    let open = cur.fetch_add(1, Ordering::SeqCst) + 1;
                    mx.fetch_max(open, Ordering::SeqCst);
                    let accepted = Instant::now();
                    while cur.load(Ordering::SeqCst) < target && accepted.elapsed() < cap {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    if cur.load(Ordering::SeqCst) >= target {
                        // The barrier is met: hold a beat so every waiter (5 ms poll)
                        // observes it before the count starts dropping again.
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    cur.fetch_sub(1, Ordering::SeqCst);
                    drop(stream);
                });
            }
        });
        (addr, max)
    }

    /// A listener that accepts, counts, holds each connection `hold` and drops it (a thread
    /// per connection): a probe of it takes `hold`, and the counter says how many connections
    /// the endpoint saw.
    fn held_endpoint(hold: Duration) -> (String, Arc<AtomicUsize>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let accepted = Arc::new(AtomicUsize::new(0));
        let acc = Arc::clone(&accepted);
        std::thread::spawn(move || {
            for stream in l.incoming() {
                acc.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || {
                    std::thread::sleep(hold);
                    drop(stream);
                });
            }
        });
        (addr, accepted)
    }

    /// A contact for `id` pointing at `endpoint`, without going through a contact book.
    fn contact_at(id: &Identity, endpoint: &str) -> Contact {
        Contact {
            name: "Cy".into(),
            emails: vec![],
            pubkey: identity::pubkey_string(&id.verifying_key()),
            endpoints: vec![endpoint.to_string()],
            source: "global".into(),
            policy: None,
            added_at: None,
            fingerprint: identity::fingerprint(&id.verifying_key()),
        }
    }

    fn no_iroh() -> Iroh {
        Iroh::Unavailable("no endpoint".into())
    }

    #[test]
    fn liveness_skips_only_recently_failed_peers() {
        let t0 = Instant::now();
        let mut lv = Liveness::new(Duration::from_secs(60));
        assert!(!lv.should_skip("owl:b", t0), "never probed");
        lv.record("owl:b", false, t0);
        assert!(lv.should_skip("owl:b", t0));
        assert!(lv.should_skip("owl:b", t0 + Duration::from_secs(59)));
        assert!(
            !lv.should_skip("owl:b", t0 + Duration::from_secs(60)),
            "exactly the window: probe again"
        );
        assert!(!lv.should_skip("owl:c", t0), "another peer is unaffected");
        lv.record("owl:b", true, t0 + Duration::from_secs(60));
        assert!(
            !lv.should_skip("owl:b", t0 + Duration::from_secs(61)),
            "a successful probe is never skipped"
        );
        assert_eq!(
            lv.get("owl:b"),
            Some(Probe {
                last_probe: t0 + Duration::from_secs(60),
                ok: true
            })
        );
        assert_eq!(lv.get("owl:zzz"), None);
        // A shorter window shortens the skip.
        let mut short = Liveness::new(Duration::from_secs(1));
        short.record("owl:b", false, t0);
        assert!(short.should_skip("owl:b", t0 + Duration::from_millis(999)));
        assert!(!short.should_skip("owl:b", t0 + Duration::from_secs(1)));
    }

    /// `last_seen` is the last successful probe's wall clock and survives a later
    /// failed probe; `probed_at` moves to the failure.
    #[test]
    fn last_seen_survives_a_failed_probe() {
        let mut lv = Liveness::new(Duration::ZERO);
        let t0 = Instant::now();
        lv.record("owl:b", true, t0);
        let seen = lv.peers()["owl:b"].last_seen.clone().expect("seen once");
        assert!(envelope::parse_rfc3339_to_unix(&seen).is_some());
        lv.record("owl:b", false, t0 + Duration::from_secs(1));
        let peers = lv.peers();
        let p = &peers["owl:b"];
        assert!(!p.online);
        assert_eq!(
            p.last_seen.as_deref(),
            Some(seen.as_str()),
            "kept across the failure"
        );
        assert!(envelope::parse_rfc3339_to_unix(&p.probed_at).is_some());
    }

    #[test]
    fn skip_window_is_one_second_short_of_the_interval() {
        assert_eq!(
            Liveness::skip_for(Duration::from_secs(1)),
            Duration::ZERO,
            "1 s interval: re-probed on every loop"
        );
        assert_eq!(
            Liveness::skip_for(Duration::from_secs(5)),
            Duration::from_secs(4)
        );
        assert_eq!(
            Liveness::skip_for(Duration::from_secs(60)),
            Duration::from_secs(59)
        );
        assert_eq!(
            Liveness::skip_for(Duration::from_secs(600)),
            Duration::from_secs(59),
            "never longer than PROBE_SKIP - 1 s"
        );
        assert_eq!(PROBE_SKIP, Duration::from_secs(60));
        // A zero window never skips, even right after a failure.
        let mut lv = Liveness::new(Duration::ZERO);
        let t0 = Instant::now();
        lv.record("owl:b", false, t0);
        assert!(!lv.should_skip("owl:b", t0));
    }

    #[test]
    fn loop_interval_floors_at_one_second() {
        let cfg = Config {
            pull_interval_secs: 0,
            ..Default::default()
        };
        assert_eq!(loop_interval(&cfg), Duration::from_secs(1));
        let cfg = Config {
            pull_interval_secs: 7,
            ..Default::default()
        };
        assert_eq!(loop_interval(&cfg), Duration::from_secs(7));
        assert_eq!(loop_interval(&Config::default()), Duration::from_secs(60));
    }

    #[test]
    fn open_ask_reads_meta_and_falls_back_to_the_payload() {
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let q = question(&a, &b);
        let env = Envelope::sign(&q, &a);
        let full = rec(
            &env,
            "waiting",
            "2026-09-01T00:00:00Z",
            json!({ "peer": "owl:override", "hash": "h1" }),
        );
        let ask = open_ask("q1", &full).unwrap();
        assert_eq!(
            ask,
            OpenAsk {
                id: "q1".into(),
                peer: "owl:override".into(),
                hash: "h1".into(),
                path: "src/x.rs".into(),
                threaded: false,
            }
        );
        // `meta.threaded` is read as a bool; anything else means not threaded.
        let threaded = rec(
            &env,
            "waiting",
            "2026-09-01T00:00:00Z",
            json!({ "peer": "owl:override", "hash": "h1", "threaded": true }),
        );
        assert!(open_ask("q1", &threaded).unwrap().threaded);
        let odd = rec(&env, "waiting", "x", json!({ "threaded": "yes" }));
        assert!(!open_ask("q1", &odd).unwrap().threaded);
        // No meta at all: peer from `to`, hash recomputed.
        let bare = rec(&env, "waiting", "2026-09-01T00:00:00Z", Value::Null);
        let ask = open_ask("q1", &bare).unwrap();
        assert_eq!(ask.peer, identity::fingerprint(&b.verifying_key()));
        assert_eq!(
            ask.hash,
            envelope::question_hash("proj", Some("src/x.rs"), "why?")
        );
        // Wrong-shape meta (array, empty strings, numbers): same fallbacks, no panic.
        for meta in [
            json!([1, 2]),
            json!({ "peer": "", "hash": 7 }),
            json!("str"),
        ] {
            let ask = open_ask("q1", &rec(&env, "waiting", "x", meta)).unwrap();
            assert_eq!(ask.peer, identity::fingerprint(&b.verifying_key()));
            assert_eq!(
                ask.hash,
                envelope::question_hash("proj", Some("src/x.rs"), "why?")
            );
        }
        // A repo-level question hashes over the empty path and shows `-`.
        let mut no_path = q.clone();
        no_path.body = Body::Question {
            project: "proj".into(),
            path: None,
            question: "why?".into(),
            context: None,
        };
        let env_np = Envelope::sign(&no_path, &a);
        let ask = open_ask("q2", &rec(&env_np, "waiting", "x", Value::Null)).unwrap();
        assert_eq!(ask.path, "-");
        assert_eq!(ask.hash, envelope::question_hash("proj", None, "why?"));
        assert_ne!(
            ask.hash,
            envelope::question_hash("proj", Some("src/x.rs"), "why?")
        );
        // Not a payload, or an answer payload: skipped.
        let garbage = Record {
            raw: "{not json".into(),
            ..bare.clone()
        };
        assert!(open_ask("q1", &garbage).is_none());
        let ans = Envelope::sign(&Payload::answer(&q, "yes", "fake", 0, false), &b);
        assert!(open_ask("q1", &rec(&ans, "waiting", "x", Value::Null)).is_none());
    }

    #[test]
    fn open_asks_skips_corrupt_files_and_keeps_the_rest() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let q = question(&a, &b);
        let env = Envelope::sign(&q, &a);
        spool
            .put(
                Dir::Asks,
                &q.id,
                &rec(&env, "waiting", "2026-09-01T00:00:00Z", Value::Null),
            )
            .unwrap();
        std::fs::write(spool.path(Dir::Asks, "broken"), "{").unwrap();
        std::fs::write(spool.path(Dir::Asks, "notes").with_extension("txt"), "x").unwrap();
        let open = open_asks(&spool).unwrap();
        assert_eq!(open.keys().collect::<Vec<_>>(), [&q.id]);
        assert!(spool.path(Dir::Asks, "broken").exists(), "left in place");
        // Missing asks/ dir: an error, not a panic.
        std::fs::remove_dir_all(home.path().join("spool/asks")).unwrap();
        assert!(open_asks(&spool).is_err());
    }

    #[test]
    fn expire_outbox_boundary() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let q = question(&a, &b);
        let mk = |suffix: &str, received_at: &str| {
            let ans = Payload::answer(&q, suffix, "fake", 0, false);
            let env = Envelope::sign(&ans, &b);
            spool
                .put(
                    Dir::Outbox,
                    &ans.id,
                    &rec(&env, "unacked", received_at, Value::Null),
                )
                .unwrap();
            ans.id
        };
        // now = 2026-09-02T00:00:00Z; TTL 1 day.
        let now = envelope::parse_rfc3339_to_unix("2026-09-02T00:00:00Z").unwrap();
        let exactly = mk("exactly", "2026-09-01T00:00:00Z");
        let older = mk("older", "2026-08-31T23:59:59Z");
        let fresh = mk("fresh", "2026-09-01T12:00:00Z");
        let future = mk("future", "2026-09-03T00:00:00Z");
        let unparseable = mk("bad", "yesterday");
        assert_eq!(expire_outbox(&spool, 1, now).unwrap(), 1);
        assert!(spool.get(Dir::Outbox, &older).unwrap().is_none());
        assert_eq!(
            spool.get(Dir::Done, &older).unwrap().unwrap().state,
            "expired"
        );
        for kept in [&exactly, &fresh, &future, &unparseable] {
            let r = spool
                .get(Dir::Outbox, kept)
                .unwrap()
                .expect("still in outbox");
            assert_eq!(r.state, "unacked");
            assert!(spool.get(Dir::Done, kept).unwrap().is_none());
        }
        // TTL 0: anything older than "now" goes, a record dated now stays.
        let at_now = mk("now", "2026-09-02T00:00:00Z");
        assert_eq!(
            expire_outbox(&spool, 0, now).unwrap(),
            2,
            "`exactly` and `fresh`; `older` already went in the first pass"
        );
        assert!(spool.get(Dir::Outbox, &at_now).unwrap().is_some());
        assert!(spool.get(Dir::Outbox, &future).unwrap().is_some());
        assert!(spool.get(Dir::Outbox, &unparseable).unwrap().is_some());
        for gone in [&exactly, &fresh] {
            assert_eq!(
                spool.get(Dir::Done, gone).unwrap().unwrap().state,
                "expired"
            );
        }
        // Nothing left to expire: 0, and the non-default 14-day TTL keeps a 13-day record.
        assert_eq!(expire_outbox(&spool, 0, now).unwrap(), 0);
        let thirteen = mk("13d", "2026-08-20T00:00:00Z");
        let fifteen = mk("15d", "2026-08-18T00:00:00Z");
        assert_eq!(expire_outbox(&spool, 14, now).unwrap(), 1);
        assert!(spool.get(Dir::Outbox, &thirteen).unwrap().is_some());
        assert!(spool.get(Dir::Outbox, &fifteen).unwrap().is_none());
    }

    #[test]
    fn expire_outbox_failed_move_leaves_the_record_unacked() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let q = question(&a, &b);
        let ans = Payload::answer(&q, "old", "fake", 0, false);
        let env = Envelope::sign(&ans, &b);
        spool
            .put(
                Dir::Outbox,
                &ans.id,
                &rec(&env, "unacked", "2020-01-01T00:00:00Z", Value::Null),
            )
            .unwrap();
        // Block the destination with a non-empty directory.
        std::fs::create_dir_all(spool.path(Dir::Done, &ans.id).join("child")).unwrap();
        assert_eq!(expire_outbox(&spool, 0, envelope::now_unix()).unwrap(), 0);
        assert_eq!(
            spool.get(Dir::Outbox, &ans.id).unwrap().unwrap().state,
            "unacked"
        );
        // Corrupt outbox file: skipped, others still processed.
        std::fs::write(spool.path(Dir::Outbox, "junk"), "nope").unwrap();
        assert_eq!(expire_outbox(&spool, 0, envelope::now_unix()).unwrap(), 0);
        assert!(spool.path(Dir::Outbox, "junk").exists());
    }

    #[test]
    fn status_file_roundtrip_and_failure_paths() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(read_status(home.path()).unwrap(), None, "no file yet");
        let status = PullStatus {
            last_pull_at: "2026-09-02T10:00:00Z".into(),
            open_asks: 2,
            peers_probed: 1,
            peers: BTreeMap::from([(
                "owl:b".to_string(),
                PeerProbe {
                    online: true,
                    probed_at: "2026-09-02T10:00:00Z".into(),
                    last_seen: Some("2026-09-02T09:59:00Z".into()),
                },
            )]),
        };
        write_status(home.path(), &status).unwrap();
        let raw: Value =
            serde_json::from_slice(&std::fs::read(home.path().join(STATUS_FILE)).unwrap()).unwrap();
        assert_eq!(raw["last_pull_at"], "2026-09-02T10:00:00Z");
        assert_eq!(raw["open_asks"], 2);
        assert_eq!(raw["peers_probed"], 1);
        assert_eq!(raw["peers"]["owl:b"]["online"], true);
        assert_eq!(raw["peers"]["owl:b"]["last_seen"], "2026-09-02T09:59:00Z");
        assert_eq!(read_status(home.path()).unwrap(), Some(status.clone()));
        // A status file written before the `peers` key existed still parses.
        std::fs::write(
            home.path().join(STATUS_FILE),
            r#"{"last_pull_at":"2026-09-02T10:00:00Z","open_asks":1,"peers_probed":0}"#,
        )
        .unwrap();
        let old = read_status(home.path()).unwrap().unwrap();
        assert_eq!(old.peers, BTreeMap::new());
        std::fs::remove_file(home.path().join(STATUS_FILE)).unwrap();
        assert!(!home.path().join("daemon.status.tmp").exists());
        // Unparseable file: an error naming the file.
        std::fs::write(home.path().join(STATUS_FILE), "{").unwrap();
        let err = read_status(home.path()).unwrap_err().to_string();
        assert!(err.contains("parsing"), "{err}");
        // Destination blocked by a non-empty directory: rename fails, no tmp left behind.
        std::fs::remove_file(home.path().join(STATUS_FILE)).unwrap();
        std::fs::create_dir_all(home.path().join(STATUS_FILE).join("child")).unwrap();
        let err = write_status(home.path(), &status).unwrap_err().to_string();
        assert!(err.contains("renaming to"), "{err}");
        assert!(!home.path().join("daemon.status.tmp").exists());
        let err = read_status(home.path()).unwrap_err().to_string();
        assert!(err.contains("reading"), "{err}");
    }

    /// `pull_once` against closed ports: every contact of the book is probed — B through its
    /// open ask, C and D (policy `never`) for presence alone — recorded offline, then skipped
    /// inside the window. An open ask to a fingerprint NOT in the book is still not probed; a
    /// malformed ask is skipped without touching the rest.
    #[test]
    fn pull_once_records_offline_and_skips_bad_asks() {
        let home = tempfile::tempdir().unwrap();
        let (a, b, c, d, e) = (
            Identity::from_seed([1; 32]),
            Identity::from_seed([2; 32]),
            Identity::from_seed([3; 32]),
            Identity::from_seed([4; 32]),
            Identity::from_seed([5; 32]),
        );
        let spool = Spool::new(home.path()).unwrap();
        write_contact(home.path(), &b, "Bea", &[&closed_port()], None);
        write_contact(home.path(), &c, "Cy", &[&closed_port()], None);
        write_contact(home.path(), &d, "Dee", &[&closed_port()], Some("never"));
        let (fp_b, fp_c, fp_d, fp_e) = (
            identity::fingerprint(&b.verifying_key()),
            identity::fingerprint(&c.verifying_key()),
            identity::fingerprint(&d.verifying_key()),
            identity::fingerprint(&e.verifying_key()),
        );
        let q = question(&a, &b);
        spool
            .put(
                Dir::Asks,
                &q.id,
                &rec(
                    &Envelope::sign(&q, &a),
                    "waiting",
                    "2026-09-01T00:00:00Z",
                    json!({ "peer": fp_b, "hash": "h" }),
                ),
            )
            .unwrap();
        // An open ask to E, whose fingerprint is not in the book.
        let to_e = question(&a, &e);
        spool
            .put(
                Dir::Asks,
                &to_e.id,
                &rec(
                    &Envelope::sign(&to_e, &a),
                    "waiting",
                    "2026-09-01T00:00:00Z",
                    Value::Null,
                ),
            )
            .unwrap();
        std::fs::write(spool.path(Dir::Asks, "junk"), "{\"raw\": 1}").unwrap();
        let mut lv = Liveness::new(Duration::from_secs(60));
        let t0 = Instant::now();
        let mut events = Vec::new();
        let status = pull_once(
            home.path(),
            home.path(),
            &a,
            &no_iroh(),
            &spool,
            &mut lv,
            t0,
            |ev| events.push(ev),
        )
        .unwrap();
        assert_eq!(status.open_asks, 2, "both parseable asks stay open");
        assert_eq!(
            status.peers_probed, 3,
            "B through its ask, C and D for presence; E is not a contact"
        );
        assert!(events.is_empty());
        for fp in [&fp_b, &fp_c, &fp_d] {
            assert_eq!(lv.get(fp).map(|p| p.ok), Some(false), "{fp}");
            // The probe is in the status too — offline, never seen, wall-clock
            // probed_at.
            let probe = &status.peers[fp];
            assert!(!probe.online, "{fp}");
            assert_eq!(probe.last_seen, None, "{fp}");
            assert!(envelope::parse_rfc3339_to_unix(&probe.probed_at).is_some());
        }
        assert_eq!(lv.get(&fp_e), None, "unknown contact: never probed");
        assert!(!status.peers.contains_key(&fp_e), "never probed");
        assert_eq!(status.peers.len(), 3);
        assert!(envelope::parse_rfc3339_to_unix(&status.last_pull_at).is_some());
        // Inside the window the offline contacts are skipped — the ask responder and the
        // presence probes alike: nothing probed at all.
        let status = pull_once(
            home.path(),
            home.path(),
            &a,
            &no_iroh(),
            &spool,
            &mut lv,
            t0 + Duration::from_secs(59),
            |_| {},
        )
        .unwrap();
        assert_eq!(status.peers_probed, 0);
        assert_eq!(
            status.peers.len(),
            3,
            "skipped this loop: the last results are kept"
        );
        assert!(status.peers.values().all(|p| !p.online));
        // Past the window they are all probed again.
        let status = pull_once(
            home.path(),
            home.path(),
            &a,
            &no_iroh(),
            &spool,
            &mut lv,
            t0 + Duration::from_secs(60),
            |_| {},
        )
        .unwrap();
        assert_eq!(status.peers_probed, 3);
        assert_eq!(
            lv.get(&fp_b).unwrap().last_probe,
            t0 + Duration::from_secs(60)
        );
        assert_eq!(
            lv.get(&fp_d).unwrap().last_probe,
            t0 + Duration::from_secs(60),
            "a presence probe is refreshed too"
        );
        // Nothing to do at all: no probes, no error.
        let empty = tempfile::tempdir().unwrap();
        let spool = Spool::new(empty.path()).unwrap();
        let status = pull_once(
            empty.path(),
            empty.path(),
            &a,
            &no_iroh(),
            &spool,
            &mut Liveness::new(Duration::ZERO),
            t0,
            |_| {},
        )
        .unwrap();
        assert_eq!((status.open_asks, status.peers_probed), (0, 0));
    }

    /// The presence probes run concurrently with each other and with the ask loop, proven by
    /// COUNTING, not wall clock: all 6 contacts (B with its open ask plus 5 presence contacts)
    /// share ONE barrier endpoint that releases a connection only once 6 are open at the same
    /// time — that max is 6 only if the presence probes overlap the ask loop (joining the
    /// probes BEFORE the ask loop would peak at 5 and fall to the barrier's 1 s fallback).
    #[test]
    fn presence_probes_run_concurrently() {
        let home = tempfile::tempdir().unwrap();
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let spool = Spool::new(home.path()).unwrap();
        let (addr, max) = barrier_endpoint(6, Duration::from_secs(1));
        write_contact(home.path(), &b, "Bea", &[&addr], None);
        let fp_b = identity::fingerprint(&b.verifying_key());
        let q = question(&a, &b);
        spool
            .put(
                Dir::Asks,
                &q.id,
                &rec(
                    &Envelope::sign(&q, &a),
                    "waiting",
                    "2026-09-01T00:00:00Z",
                    json!({ "peer": fp_b, "hash": "h" }),
                ),
            )
            .unwrap();
        let mut fps = vec![fp_b];
        for seed in 3..8u8 {
            let id = Identity::from_seed([seed; 32]);
            write_contact(home.path(), &id, &format!("C{seed}"), &[&addr], None);
            fps.push(identity::fingerprint(&id.verifying_key()));
        }
        let mut lv = Liveness::new(Duration::from_secs(60));
        let started = Instant::now();
        let status = pull_once(
            home.path(),
            home.path(),
            &a,
            &no_iroh(),
            &spool,
            &mut lv,
            started,
            |_| {},
        )
        .unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(10),
            "sanity: one ~1 s wave: {elapsed:?}"
        );
        assert_eq!(
            max.load(Ordering::SeqCst),
            6,
            "all six probes open at once: the presence probes overlap the ask loop"
        );
        assert_eq!(status.peers_probed, 6);
        assert_eq!(status.open_asks, 1, "B is offline: its ask stays open");
        assert!(spool.get(Dir::Asks, &q.id).unwrap().is_some());
        assert!(spool.get(Dir::Done, &q.id).unwrap().is_none());
        for fp in &fps {
            assert_eq!(lv.get(fp).map(|p| p.ok), Some(false), "{fp}");
            let probe = &status.peers[fp];
            assert!(!probe.online, "{fp}");
            assert_eq!(probe.last_seen, None, "{fp}");
            assert!(envelope::parse_rfc3339_to_unix(&probe.probed_at).is_some());
        }
        assert_eq!(status.peers.len(), 6);
    }

    /// 20 contacts sharing ONE endpoint that holds every connection ~1 s: at most
    /// `PROBE_WORKERS` probes run at once (but truly in parallel), all 20 land offline, and
    /// the whole phase costs ceil(20/8) waves of ~1 s, not 20 s.
    #[test]
    fn presence_probes_are_capped_and_parallel() {
        let home = tempfile::tempdir().unwrap();
        let a = Identity::from_seed([1; 32]);
        let spool = Spool::new(home.path()).unwrap();
        let (addr, max) = gated_endpoint();
        let mut fps = Vec::new();
        for seed in 2..22u8 {
            let id = Identity::from_seed([seed; 32]);
            write_contact(home.path(), &id, &format!("C{seed}"), &[&addr], None);
            fps.push(identity::fingerprint(&id.verifying_key()));
        }
        let mut lv = Liveness::new(Duration::ZERO);
        let started = Instant::now();
        let status = pull_once(
            home.path(),
            home.path(),
            &a,
            &no_iroh(),
            &spool,
            &mut lv,
            started,
            |_| {},
        )
        .unwrap();
        let elapsed = started.elapsed();
        let max = max.load(Ordering::SeqCst);
        assert!(max <= PROBE_WORKERS, "at most 8 probes at once: {max}");
        assert!(max > 1, "truly parallel: {max}");
        assert!(
            elapsed >= Duration::from_millis(2500),
            "the cap really queued them: ceil(20/8) waves of ~1 s: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(15),
            "sanity: capped, not 20 sequential probes: {elapsed:?}"
        );
        assert_eq!(status.peers_probed, 20);
        for fp in &fps {
            assert_eq!(lv.get(fp).map(|p| p.ok), Some(false), "{fp}");
            assert!(!status.peers[fp].online, "{fp}");
        }
    }

    /// The probe phase runs EXACTLY 8 wide, pinned by counting alone: 20 contacts on one
    /// barrier endpoint that releases a connection only once 8 are open at the same time
    /// (with a 1 s fallback, under the client's 2 s connect timeout, so a smaller cap fails
    /// instead of hanging). One worker fewer and the barrier is never reached — the peak
    /// is 7, not 8.
    #[test]
    fn presence_probes_run_eight_wide() {
        let home = tempfile::tempdir().unwrap();
        let a = Identity::from_seed([1; 32]);
        let spool = Spool::new(home.path()).unwrap();
        let (addr, max) = barrier_endpoint(8, Duration::from_secs(1));
        let mut fps = Vec::new();
        for seed in 2..22u8 {
            let id = Identity::from_seed([seed; 32]);
            write_contact(home.path(), &id, &format!("C{seed}"), &[&addr], None);
            fps.push(identity::fingerprint(&id.verifying_key()));
        }
        let mut lv = Liveness::new(Duration::ZERO);
        let status = pull_once(
            home.path(),
            home.path(),
            &a,
            &no_iroh(),
            &spool,
            &mut lv,
            Instant::now(),
            |_| {},
        )
        .unwrap();
        assert_eq!(
            max.load(Ordering::SeqCst),
            8,
            "the barrier released: 8 probes were open at once, no fewer"
        );
        assert_eq!(status.peers_probed, 20);
        for fp in &fps {
            assert_eq!(lv.get(fp).map(|p| p.ok), Some(false), "{fp}");
            assert!(!status.peers[fp].online, "{fp}");
        }
    }

    /// A listener that records the Instant it accepts a connection, holds it ~1 s and drops
    /// it: a probe of it takes ~1 s, and the recorded accept says which wave it ran in.
    fn timed_endpoint() -> (String, Arc<Mutex<Option<Instant>>>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let accepted = Arc::new(Mutex::new(None));
        let acc = Arc::clone(&accepted);
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let acc = Arc::clone(&acc);
                std::thread::spawn(move || {
                    *acc.lock().unwrap() = Some(Instant::now());
                    std::thread::sleep(Duration::from_secs(1));
                    drop(stream);
                });
            }
        });
        (addr, accepted)
    }

    /// pull_once itself orders the probes oldest-first: 9 contacts on 9 timed endpoints, the
    /// FIRST in book order probed long ago (past the skip window), the other 8 never probed.
    /// With 8 workers the first wave is 8 contacts — the never-probed ones — so the old
    /// contact's listener is accepted only in the second wave, ~1 s after the first accepts.
    #[test]
    fn pull_once_probes_never_probed_contacts_first() {
        let home = tempfile::tempdir().unwrap();
        let a = Identity::from_seed([1; 32]);
        let spool = Spool::new(home.path()).unwrap();
        let mut accepts: HashMap<String, Arc<Mutex<Option<Instant>>>> = HashMap::new();
        for seed in 2..11u8 {
            let id = Identity::from_seed([seed; 32]);
            let (addr, accepted) = timed_endpoint();
            write_contact(home.path(), &id, &format!("C{seed}"), &[&addr], None);
            accepts.insert(identity::fingerprint(&id.verifying_key()), accepted);
        }
        let book = ContactBook::load(home.path(), home.path()).unwrap();
        assert_eq!(book.contacts.len(), 9);
        let fp_old = book.contacts[0].fingerprint.clone();
        let now = Instant::now();
        let mut lv = Liveness::new(Duration::from_secs(60));
        // Probed an hour ago (a freshly booted machine has no Instant that far back: fall
        // back to no skip window at all, where the age is moot).
        let old = match now.checked_sub(Duration::from_secs(3600)) {
            Some(old) => old,
            None => {
                lv = Liveness::new(Duration::ZERO);
                now
            }
        };
        lv.record(&fp_old, false, old);
        assert!(
            !lv.should_skip(&fp_old, now),
            "precondition: the old probe is past the skip window"
        );
        let status = pull_once(
            home.path(),
            home.path(),
            &a,
            &no_iroh(),
            &spool,
            &mut lv,
            now,
            |_| {},
        )
        .unwrap();
        assert_eq!(status.peers_probed, 9);
        let at = |fp: &str| {
            accepts[fp]
                .lock()
                .unwrap()
                .unwrap_or_else(|| panic!("{fp} was never probed"))
        };
        let earliest = book
            .contacts
            .iter()
            .filter(|c| c.fingerprint != fp_old)
            .map(|c| at(&c.fingerprint))
            .min()
            .unwrap();
        let old_at = at(&fp_old);
        assert!(
            old_at.saturating_duration_since(earliest) >= Duration::from_millis(800),
            "the never-probed contacts went first; the old one ran in the second wave: \
             {old_at:?} vs earliest {earliest:?}"
        );
        for c in &book.contacts {
            assert_eq!(lv.get(&c.fingerprint).map(|p| p.ok), Some(false));
        }
    }

    /// The budget: only probes STARTED inside it run, proven by counting. 20 contacts on an
    /// endpoint that holds each connection 2 s, 8 workers, a 1 s budget: the first wave —
    /// the first 8 contacts of the given order — starts well inside the budget and ends well
    /// after it, the endpoint accepts exactly 8 connections (not 20), and the call does not
    /// wait out the 12 that never started.
    #[test]
    fn probe_budget_stops_starting_new_probes() {
        let (addr, accepted) = held_endpoint(Duration::from_secs(2));
        let a = Identity::from_seed([1; 32]);
        let contacts: Vec<Contact> = (2..22u8)
            .map(|seed| contact_at(&Identity::from_seed([seed; 32]), &addr))
            .collect();
        let refs: Vec<&Contact> = contacts.iter().collect();
        let started = Instant::now();
        let results = probe_contacts(&refs, &a, &no_iroh(), 8, Duration::from_secs(1));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(10),
            "sanity: one 2 s wave, not all 20: {elapsed:?}"
        );
        assert_eq!(results.len(), 8, "only the first wave started");
        assert!(results.iter().all(|(_, ok)| !*ok));
        // Completion order inside the wave is a race: compare as sets.
        let mut probed: Vec<&str> = results.iter().map(|(fp, _)| fp.as_str()).collect();
        probed.sort_unstable();
        let mut first_wave: Vec<&str> = refs[..8].iter().map(|c| c.fingerprint.as_str()).collect();
        first_wave.sort_unstable();
        assert_eq!(probed, first_wave, "the first 8 of the given order");
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            8,
            "the endpoint saw the first wave only, not all 20"
        );
    }

    /// The budget also bounds ONE contact: 20 dead endpoints that hold every connection far
    /// longer than the client's 2 s connect timeout, so each endpoint attempt takes ~2 s, and
    /// a 3 s budget. Attempts start at ~0 s and ~2 s (inside the budget); the third would
    /// start at ~4 s, after the deadline, so it and the 17 after it never run: exactly two
    /// connections are accepted (20 without the deadline, ~40 s; 3 with a deadline of twice
    /// the budget), and the contact is recorded offline.
    #[test]
    fn probe_budget_stops_endpoint_attempts_inside_one_probe() {
        let (addr, accepted) = held_endpoint(Duration::from_secs(10));
        let a = Identity::from_seed([1; 32]);
        let contact = Contact {
            endpoints: vec![addr; 20],
            ..contact_at(&Identity::from_seed([2; 32]), "x")
        };
        let results = probe_contacts(&[&contact], &a, &no_iroh(), 8, Duration::from_secs(3));
        assert_eq!(results, [(contact.fingerprint.clone(), false)]);
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            2,
            "only the endpoint attempts started inside the budget ran"
        );
    }

    /// The probe budget is the existing per-request timeout, 30 s.
    #[test]
    fn probe_budget_is_the_request_timeout() {
        assert_eq!(PROBE_BUDGET, client::REQUEST_TIMEOUT);
        assert_eq!(PROBE_BUDGET, Duration::from_secs(30));
    }

    /// The trash GC runs at start, then not again until exactly `GC_EVERY` (24 h) later.
    #[test]
    fn gc_is_due_at_start_and_every_24_hours() {
        let t0 = Instant::now();
        let day = Duration::from_secs(24 * 60 * 60);
        assert_eq!(GC_EVERY, day);
        assert!(gc_due(None, t0), "at daemon start");
        assert!(!gc_due(Some(t0), t0), "just ran");
        assert!(!gc_due(Some(t0), t0 + day - Duration::from_secs(1)));
        assert!(gc_due(Some(t0), t0 + day), "24 h later");
        assert!(gc_due(Some(t0), t0 + day + Duration::from_secs(1)));
    }

    /// Never-probed contacts sort first, then by last probe ascending: the contacts a small
    /// budget skips this loop are first next loop.
    #[test]
    fn probes_go_oldest_first() {
        let t0 = Instant::now();
        let mut lv = Liveness::new(Duration::ZERO);
        lv.record("owl:old", false, t0);
        lv.record("owl:recent", true, t0 + Duration::from_secs(10));
        let mk = |fp: &str| Contact {
            fingerprint: fp.to_string(),
            ..contact_at(&Identity::from_seed([9; 32]), "x")
        };
        let contacts = [
            mk("owl:recent"),
            mk("owl:new2"),
            mk("owl:old"),
            mk("owl:new1"),
        ];
        let mut refs: Vec<&Contact> = contacts.iter().collect();
        oldest_first(&mut refs, &lv);
        let order: Vec<&str> = refs.iter().map(|c| c.fingerprint.as_str()).collect();
        // Never probed first (in book order — the sort is stable), then oldest probe first.
        assert_eq!(order, ["owl:new2", "owl:new1", "owl:old", "owl:recent"]);
    }

    /// Two `.agents/peers/` files for the same key (the repo provider does not fold them, only
    /// global files fold): the presence probes deduplicate by fingerprint — one probe, one
    /// status entry.
    #[test]
    fn duplicate_repo_contacts_are_probed_once() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir(cwd.path().join(".git")).unwrap();
        let peers = cwd.path().join(".agents").join("peers");
        std::fs::create_dir_all(&peers).unwrap();
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let port = closed_port();
        for (file, name) in [("01-bea.json", "Bea"), ("02-bea-again.json", "Bea Again")] {
            let v = json!({
                "name": name,
                "emails": [],
                "pubkey": identity::pubkey_string(&b.verifying_key()),
                "endpoints": [&port],
                "source": "local",
            });
            std::fs::write(peers.join(file), serde_json::to_vec(&v).unwrap()).unwrap();
        }
        let fp_b = identity::fingerprint(&b.verifying_key());
        // Precondition: the book really holds the key twice, or this test proves nothing
        // about the dedupe.
        let book = ContactBook::load(home.path(), cwd.path()).unwrap();
        assert_eq!(
            book.contacts
                .iter()
                .filter(|c| c.fingerprint == fp_b)
                .count(),
            2,
            "the repo provider does not fold same-key files: {book:?}"
        );
        let spool = Spool::new(home.path()).unwrap();
        let mut lv = Liveness::new(Duration::from_secs(60));
        let t0 = Instant::now();
        let status = pull_once(
            home.path(),
            cwd.path(),
            &a,
            &no_iroh(),
            &spool,
            &mut lv,
            t0,
            |_| {},
        )
        .unwrap();
        assert_eq!(status.peers_probed, 1, "deduplicated by fingerprint");
        assert_eq!(status.open_asks, 0);
        assert_eq!(status.peers.len(), 1);
        let probe = &status.peers[&fp_b];
        assert!(!probe.online);
        assert_eq!(probe.last_seen, None);
        assert!(envelope::parse_rfc3339_to_unix(&probe.probed_at).is_some());
        assert_eq!(lv.get(&fp_b).map(|p| p.ok), Some(false));
        assert_eq!(lv.get(&fp_b).unwrap().last_probe, t0);
    }

    #[test]
    fn ingest_envelope_verdicts() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (a, b, other) = (
            Identity::from_seed([1; 32]),
            Identity::from_seed([2; 32]),
            Identity::from_seed([9; 32]),
        );
        let port = closed_port();
        write_contact(home.path(), &b, "Bea", &[&port], None);
        let book = ContactBook::load(home.path(), home.path()).unwrap();
        let contact = book.contacts[0].clone();
        let q = question(&a, &b);
        spool
            .put(
                Dir::Asks,
                &q.id,
                &rec(
                    &Envelope::sign(&q, &a),
                    "waiting",
                    "2026-09-01T00:00:00Z",
                    json!({ "peer": contact.fingerprint, "hash": "h" }),
                ),
            )
            .unwrap();
        let open = open_asks(&spool).unwrap();
        let ans = Payload::answer(&q, "yes", "fake", 0, false);
        // Forged: signed by somebody else.
        let forged = Envelope::sign(&ans, &other);
        assert!(matches!(
            ingest_envelope(&a, &contact, &no_iroh(), &spool, &open, &forged).unwrap(),
            Verdict::Forged
        ));
        // Unrelated: replies to an unknown question.
        let mut unrelated = ans.clone();
        unrelated.in_reply_to = Some("someone-elses".into());
        let unrelated = Envelope::sign(&unrelated, &b);
        assert!(matches!(
            ingest_envelope(&a, &contact, &no_iroh(), &spool, &open, &unrelated).unwrap(),
            Verdict::Unrelated
        ));
        // B-signed answer to A's open ask to C: verified, but not B's to answer.
        let to_c = question(&a, &other);
        spool
            .put(
                Dir::Asks,
                &to_c.id,
                &rec(
                    &Envelope::sign(&to_c, &a),
                    "waiting",
                    "2026-09-01T00:00:00Z",
                    json!({ "peer": identity::fingerprint(&other.verifying_key()), "hash": "hc" }),
                ),
            )
            .unwrap();
        let open = open_asks(&spool).unwrap();
        assert_eq!(open.len(), 2);
        let hijack = Envelope::sign(&Payload::answer(&to_c, "mine now", "fake", 0, false), &b);
        assert!(matches!(
            ingest_envelope(&a, &contact, &no_iroh(), &spool, &open, &hijack).unwrap(),
            Verdict::Unrelated
        ));
        assert!(
            spool.get(Dir::Asks, &to_c.id).unwrap().is_some(),
            "ask to C stays open"
        );
        assert!(spool.cache_get("hc").unwrap().is_none());
        let mut no_reply = ans.clone();
        no_reply.in_reply_to = None;
        assert!(matches!(
            ingest_envelope(
                &a,
                &contact,
                &no_iroh(),
                &spool,
                &open,
                &Envelope::sign(&no_reply, &b)
            )
            .unwrap(),
            Verdict::Unrelated
        ));
        assert!(spool.get(Dir::Inbox, &ans.id).unwrap().is_none());
        assert!(spool.get(Dir::Asks, &q.id).unwrap().is_some());
        // Good: stored, ask moved, ack failed (closed port) but reported, not fatal.
        let good = Envelope::sign(&ans, &b);
        let Verdict::Ingested(ing) =
            ingest_envelope(&a, &contact, &no_iroh(), &spool, &open, &good).unwrap()
        else {
            panic!("expected Ingested");
        };
        assert_eq!(ing.ask.id, q.id);
        assert_eq!(ing.answer, ans);
        assert!(
            ing.ack_error
                .as_ref()
                .is_some_and(|e| e.to_string().starts_with("offline")),
            "{:?}",
            ing.ack_error
        );
        let inbox = spool.get(Dir::Inbox, &ans.id).unwrap().unwrap();
        assert_eq!(inbox.state, "pending");
        assert!(!inbox.seen);
        assert_eq!(inbox.meta["peer"], contact.fingerprint);
        assert_eq!(inbox.meta["hash"], "h");
        assert_eq!(inbox.meta["in_reply_to"], q.id);
        assert_eq!(spool.cache_get("h").unwrap().unwrap().raw, good.raw);
        assert!(spool.get(Dir::Asks, &q.id).unwrap().is_none());
        assert_eq!(
            spool.get(Dir::Done, &q.id).unwrap().unwrap().state,
            "answered"
        );
    }

    /// The ask move is blocked: the inbox record (written first) exists, the ask stays open
    /// and the error surfaces — the next pull repeats the idempotent write and retries.
    #[test]
    fn ingest_writes_inbox_before_moving_the_ask() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let port = closed_port();
        write_contact(home.path(), &b, "Bea", &[&port], None);
        let contact = ContactBook::load(home.path(), home.path())
            .unwrap()
            .contacts[0]
            .clone();
        let q = question(&a, &b);
        spool
            .put(
                Dir::Asks,
                &q.id,
                &rec(
                    &Envelope::sign(&q, &a),
                    "waiting",
                    "2026-09-01T00:00:00Z",
                    Value::Null,
                ),
            )
            .unwrap();
        let open = open_asks(&spool).unwrap();
        let ans = Payload::answer(&q, "yes", "fake", 0, false);
        let good = Envelope::sign(&ans, &b);
        std::fs::create_dir_all(spool.path(Dir::Done, &q.id).join("child")).unwrap();
        let err = ingest_envelope(&a, &contact, &no_iroh(), &spool, &open, &good)
            .unwrap_err()
            .to_string();
        assert!(err.contains("moving"), "{err}");
        assert!(spool.get(Dir::Inbox, &ans.id).unwrap().is_some());
        assert!(spool.get(Dir::Asks, &q.id).unwrap().is_some(), "still open");
        // Inbox write blocked instead: nothing moved, error names the inbox write.
        // The block is a read-only inbox/ — a directory at `<id>.json` would now be an id
        // collision (Refused) before the write is even tried, and the temp name is unique
        // per write (`Spool::put_new`).
        use std::os::unix::fs::PermissionsExt;
        std::fs::remove_dir_all(spool.path(Dir::Done, &q.id)).unwrap();
        std::fs::remove_file(spool.path(Dir::Inbox, &ans.id)).unwrap();
        let inbox = home.path().join("spool/inbox");
        std::fs::set_permissions(&inbox, std::fs::Permissions::from_mode(0o555)).unwrap();
        let err = ingest_envelope(&a, &contact, &no_iroh(), &spool, &open, &good)
            .unwrap_err()
            .to_string();
        std::fs::set_permissions(&inbox, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tmp = inbox
            .join(format!("{}.json.", ans.id))
            .display()
            .to_string();
        assert!(
            err.starts_with(&format!("writing {tmp}")) && err.ends_with(".tmp"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_dir(&inbox).unwrap().count(),
            0,
            "nothing written"
        );
        assert!(spool.get(Dir::Asks, &q.id).unwrap().is_some(), "still open");
        assert!(spool.cache_get("h").unwrap().is_none());
    }

    /// `store_answer` never writes over another record under the answer's id — in
    /// inbox/ or another record dir, also as a case variant (what a writer racing the
    /// `answer_id` check would meet): the error is `is_taken`, and nothing changes, the cache
    /// included.
    #[test]
    fn store_answer_never_replaces_a_record() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let q = question(&a, &b);
        let ans = Payload::answer(&q, "yes", "fake", 0, false);
        let env = Envelope::sign(&ans, &b);
        let other = rec(
            &Envelope::sign(&q, &a),
            "answered",
            "2026-09-01T00:00:00Z",
            Value::Null,
        );
        for (dir, id) in [
            (Dir::Inbox, ans.id.clone()),
            (Dir::Inbox, ans.id.to_ascii_uppercase()),
            (Dir::Done, ans.id.clone()),
        ] {
            spool.put(dir, &id, &other).unwrap();
            let before = std::fs::read(spool.path(dir, &id)).unwrap();
            let err = store_answer(&spool, &env, &ans, "owl:b", "h", &q.id, true).unwrap_err();
            assert!(crate::spool::is_taken(&err), "{dir:?} {id}: {err:#}");
            assert_eq!(std::fs::read(spool.path(dir, &id)).unwrap(), before);
            assert!(spool.cache_get("h").unwrap().is_none(), "{dir:?} {id}");
            std::fs::remove_file(spool.path(dir, &id)).unwrap();
        }
        store_answer(&spool, &env, &ans, "owl:b", "h", &q.id, true).unwrap();
        assert_eq!(
            spool.get(Dir::Inbox, &ans.id).unwrap().unwrap().raw,
            env.raw
        );
    }

    /// `answer_id` is `Stored` only when EVERY record under the id is this answer;
    /// one identical copy next to another record, or a copy that does not parse, is `Refused`.
    #[test]
    fn answer_id_is_stored_only_when_every_record_is_this_answer() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (a, b) = (Identity::from_seed([1; 32]), Identity::from_seed([2; 32]));
        let q = question(&a, &b);
        let ans = Payload::answer(&q, "yes", "fake", 0, false);
        let env = Envelope::sign(&ans, &b);
        let same = rec(&env, "pending", "2026-09-01T00:00:00Z", Value::Null);
        assert_eq!(answer_id(&spool, &env, &ans).unwrap(), AnswerId::New);
        spool.put(Dir::Inbox, &ans.id, &same).unwrap();
        assert_eq!(answer_id(&spool, &env, &ans).unwrap(), AnswerId::Stored);
        // The identical copy plus another record under the same id in done/.
        let other = rec(
            &Envelope::sign(&q, &a),
            "answered",
            "2026-09-01T00:00:00Z",
            Value::Null,
        );
        spool.put(Dir::Done, &ans.id, &other).unwrap();
        assert_eq!(answer_id(&spool, &env, &ans).unwrap(), AnswerId::Refused);
        // The identical copy plus a file under the id that does not parse.
        std::fs::remove_file(spool.path(Dir::Done, &ans.id)).unwrap();
        std::fs::write(spool.path(Dir::Outbox, &ans.id), "{").unwrap();
        assert_eq!(answer_id(&spool, &env, &ans).unwrap(), AnswerId::Refused);
        std::fs::remove_file(spool.path(Dir::Outbox, &ans.id)).unwrap();
        assert_eq!(answer_id(&spool, &env, &ans).unwrap(), AnswerId::Stored);
        // Not a record id: refused before any lookup, so no path is built from it.
        for bad in ["", "-x", "../x", "a/b", "a.b", "..", "a\\b", "a\0b", "a b"] {
            let mut odd = ans.clone();
            odd.id = bad.to_string();
            let env = Envelope::sign(&odd, &b);
            assert_eq!(
                answer_id(&spool, &env, &odd).unwrap(),
                AnswerId::Refused,
                "{bad:?}"
            );
        }
        let mut plain = ans.clone();
        plain.id = "Ab-09".to_string();
        let env = Envelope::sign(&plain, &b);
        assert_eq!(answer_id(&spool, &env, &plain).unwrap(), AnswerId::New);
    }
}
