//! Auto-accept scheduler (§3.2 step 3 `auto`, §3.4 "Auto: send immediately"): for a `pending`
//! question from a peer whose policy is `auto`, draft → redact → sign → send through the very
//! same `crate::answer` functions `owl draft` / `owl send` use, log the answer to
//! `log/outgoing.jsonl` with `mode: "auto"` and notify.
//!
//! Two triggers feed [`Scheduler`]: the daemon's `Question { auto: true }` event (an immediate
//! attempt) and a periodic scan of the spool (records released by `owl allow --always`, or
//! stranded by a daemon restart). A failed attempt — timeout, unknown project,
//! `extract_failed`, a send error — leaves the record `pending` with `meta.auto_error` so the
//! human sees it in `owl inbox`; such records are not retried by the scan (the human re-drafts
//! or rejects). One exception: when `send` failed *after* the signed envelope reached
//! `outbox/` (log or `done/` not writable), the record stays `drafted` with `auto_error`, so
//! the human's `owl send` reuses that envelope instead of signing a second one (the same
//! `existing_answer` rule `owl edit` honours). The policy is re-read at attempt time: a peer flipped to `manual` or `never`
//! between arrival and attempt is never auto-answered.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::answer::{self, SendMode, Sent};
use crate::config::Config;
use crate::contacts::{ContactBook, Mode};
use crate::notify::{self, Kind};
use crate::runner::DraftStatus;
use crate::server::AppState;
use crate::spool::{Dir, Record, Spool};

pub use crate::answer::AUTO_ERROR;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The answer left the machine.
    Sent {
        answer: Sent,
        peer: String,
        path: String,
    },
    /// Nothing to do (record gone, not `pending`, policy no longer `auto`, responder off).
    Skipped(String),
    /// The record stays `pending` with `meta.auto_error` set to this text.
    Failed(String),
}

/// Why a `pending` record is not an auto-accept candidate right now, if it is not.
fn not_a_candidate(config: &Config, book: &ContactBook, id: &str, rec: &Record) -> Option<String> {
    if rec.state != "pending" {
        return Some(format!("record {id} is {}, not pending", rec.state));
    }
    let payload = match answer::payload_of(id, rec) {
        Ok(p) => p,
        Err(e) => return Some(format!("{e:#}")),
    };
    // Content never runs unattended, whatever the peer's policy says. An explicit
    // guard before the body check, so the reason a test reads is this one and a later kind
    // inherits it.
    if payload.kind == crate::envelope::Kind::Content {
        return Some(format!("record {id} is a content request — consent only"));
    }
    // The same guard for a tool call, and for a stronger reason — the daemon never
    // spawns a tool at all, whatever the peer's policy says.
    if payload.kind == crate::envelope::Kind::ToolCall {
        return Some(format!("record {id} is a tool-call request — consent only"));
    }
    if answer::question_body(id, &payload).is_err() {
        return Some(format!("record {id} is an answer"));
    }
    if !config.responder.enabled {
        return Some("responder is disabled".into());
    }
    match book.policy_for(&payload.from).map(|p| p.mode) {
        Some(Mode::Auto) => None,
        Some(m) => Some(format!("policy for {} is {}", payload.from, m.as_str())),
        None => Some(format!("no policy for {}", payload.from)),
    }
}

/// Puts `rec` back as `pending` without a draft and with `meta.auto_error = why`.
fn fail(spool: &Spool, id: &str, mut rec: Record, why: String) -> anyhow::Result<Outcome> {
    rec.state = "pending".into();
    rec.draft = None;
    fail_as_is(spool, id, rec, why)
}

/// Writes `rec` back unchanged except for `meta.auto_error = why`.
fn fail_as_is(spool: &Spool, id: &str, mut rec: Record, why: String) -> anyhow::Result<Outcome> {
    answer::meta_object(&mut rec).insert(AUTO_ERROR.into(), json!(why));
    spool.put(Dir::Inbox, id, &rec)?;
    Ok(Outcome::Failed(why))
}

/// One attempt at question `id`. `Err` only for spool I/O failures the caller should log; every
/// runner / policy / send outcome is an [`Outcome`].
pub fn attempt(
    home: &Path,
    cwd: &Path,
    config: &Config,
    spool: &Spool,
    id: &str,
) -> anyhow::Result<Outcome> {
    let Some(rec) = spool.get(Dir::Inbox, id)? else {
        return Ok(Outcome::Skipped(format!(
            "record {id} is no longer in the inbox"
        )));
    };
    let book = ContactBook::load(home, cwd)?;
    if let Some(why) = not_a_candidate(config, &book, id, &rec) {
        return Ok(Outcome::Skipped(why));
    }
    let payload = answer::payload_of(id, &rec)?;
    let (_, path, _) = answer::question_body(id, &payload)?;
    let (peer, path) = (payload.from.clone(), path.unwrap_or("-").to_string());
    let original = rec.clone();
    // `answer::draft` drops any stale `auto_error` from an earlier failed attempt.
    let drafted = match answer::draft(config, home, id, rec, None) {
        Ok((drafted, d)) if d.status == DraftStatus::Ok => drafted,
        Ok((_, d)) => {
            return fail(
                spool,
                id,
                original,
                format!("draft status {}", d.status.as_str()),
            );
        }
        Err(e) => return fail(spool, id, original, format!("{e:#}")),
    };
    spool.put(Dir::Inbox, id, &drafted)?;
    match answer::send(home, spool, id, drafted, SendMode::Auto) {
        Ok(answer) => Ok(Outcome::Sent { answer, peer, path }),
        Err(e) => {
            let why = format!("{e:#}");
            // Nothing left in the inbox: `finish` wrote `done/` and only the unlink failed.
            let Some(left) = spool.get(Dir::Inbox, id)? else {
                return Ok(Outcome::Failed(why));
            };
            if answer::existing_answer(spool, id)?.is_some() {
                // The signed envelope is already in `outbox/`: keep the record `drafted` so
                // `owl send` reuses it rather than signing a second answer.
                return fail_as_is(spool, id, left, why);
            }
            fail(spool, id, left, why)
        }
    }
}

/// Ids of every record the scan should attempt: `pending` questions from `auto` peers that
/// carry no `auto_error` (those wait for the human). Corrupt records are skipped.
pub fn candidates(
    config: &Config,
    book: &ContactBook,
    spool: &Spool,
) -> anyhow::Result<Vec<String>> {
    Ok(spool
        .list_lenient(Dir::Inbox)?
        .into_iter()
        .filter(|(id, rec)| {
            answer::auto_error(rec).is_none() && not_a_candidate(config, book, id, rec).is_none()
        })
        .map(|(id, _)| id)
        .collect())
}

/// Serialises attempts per record: the event trigger and the scan must not both draft the same
/// question (a draft can run for `timeout_secs`, longer than the scan period).
#[derive(Debug, Clone, Default)]
pub struct Scheduler {
    in_flight: Arc<Mutex<HashSet<String>>>,
}

impl Scheduler {
    pub fn new() -> Scheduler {
        Scheduler::default()
    }

    /// True when `id` was not in flight and is now claimed.
    pub fn claim(&self, id: &str) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string())
    }

    pub fn release(&self, id: &str) {
        self.in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Attempts `id` on a blocking thread (the runner blocks), unless it is already in flight.
    /// The outcome is logged and, on success, notified.
    pub fn spawn(&self, state: Arc<AppState>, id: String) {
        if !self.claim(&id) {
            tracing::debug!(%id, "auto-accept already in flight");
            return;
        }
        let sched = self.clone();
        tokio::spawn(async move {
            let st = state.clone();
            let rid = id.clone();
            let result = tokio::task::spawn_blocking(move || {
                attempt(&st.home, &st.cwd, &st.config, &st.spool, &rid)
            })
            .await;
            sched.release(&id);
            match result {
                Ok(outcome) => report(&state, &id, outcome),
                Err(e) => tracing::error!(%id, error = %e, "auto-accept task panicked"),
            }
        });
    }

    /// Scans the spool now and then every pull interval, spawning an attempt per candidate.
    /// Ends only when the task is aborted.
    pub async fn run_scan(self, state: Arc<AppState>) {
        let interval = crate::pull::loop_interval(&state.config);
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let st = state.clone();
            let ids = tokio::task::spawn_blocking(move || {
                let book = ContactBook::load(&st.home, &st.cwd)?;
                candidates(&st.config, &book, &st.spool)
            })
            .await;
            match ids {
                Ok(Ok(ids)) => {
                    for id in ids {
                        self.spawn(state.clone(), id);
                    }
                }
                Ok(Err(e)) => tracing::warn!(error = %format!("{e:#}"), "auto-accept scan failed"),
                Err(e) => tracing::error!(error = %e, "auto-accept scan panicked"),
            }
        }
    }
}

/// Logs one attempt's result and fires the "auto-answered" notification for a sent answer.
fn report(state: &AppState, id: &str, outcome: anyhow::Result<Outcome>) {
    match outcome {
        Ok(Outcome::Sent { answer, peer, path }) => {
            tracing::info!(%id, answer = %answer.answer_id, to = %answer.to, "auto-answered");
            notify::notify(
                &state.config,
                Kind::AutoAnswered,
                &crate::daemon::peer_name(state, &peer),
                &path,
            );
        }
        Ok(Outcome::Skipped(why)) => tracing::debug!(%id, %why, "auto-accept skipped"),
        Ok(Outcome::Failed(why)) => tracing::warn!(%id, %why, "auto-accept failed; left pending"),
        Err(e) => tracing::warn!(%id, error = %format!("{e:#}"), "auto-accept attempt errored"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Harness;
    use crate::contacts::{Policy, Scope};
    use crate::envelope::{self, Envelope, Payload};
    use crate::identity::{self, Identity};
    use serde_json::Value;

    const FAKE_HARNESS: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fake-harness.sh"
    );
    const PROJECT: &str = "github.com/company/monorepo";

    struct Fixture {
        home: tempfile::TempDir,
        _checkout: tempfile::TempDir,
        config: Config,
        me: Identity,
        peer: Identity,
    }

    impl Fixture {
        fn new(mode: Option<Mode>, map_project: bool) -> Fixture {
            let home = tempfile::tempdir().unwrap();
            let checkout = tempfile::tempdir().unwrap();
            let me = Identity::from_seed([2u8; 32]);
            me.save(home.path()).unwrap();
            let peer = Identity::from_seed([1u8; 32]);
            let mut config = Config {
                name: "Bea".into(),
                notify: false,
                ..Default::default()
            };
            config.harnesses.insert(
                "fake".into(),
                Harness {
                    cmd: vec![FAKE_HARNESS.into(), "{prompt}".into()],
                    answer_path: "raw".into(),
                    enabled: true,
                    disabled_reason: None,
                    env: Default::default(),
                    model: None,
                },
            );
            config.responder.harness = "fake".into();
            if map_project {
                config.projects.insert(
                    PROJECT.into(),
                    checkout.path().to_string_lossy().into_owned(),
                );
            }
            let f = Fixture {
                home,
                _checkout: checkout,
                config,
                me,
                peer,
            };
            f.write_contact(mode);
            f
        }

        fn fp(&self, id: &Identity) -> String {
            identity::fingerprint(&id.verifying_key())
        }

        fn write_contact(&self, mode: Option<Mode>) {
            let dir = self.home.path().join("contacts");
            std::fs::create_dir_all(&dir).unwrap();
            let mut v = json!({
                "name": "Ana",
                "pubkey": identity::pubkey_string(&self.peer.verifying_key()),
                "source": "global",
            });
            if let Some(mode) = mode {
                let p = Policy {
                    mode,
                    scope: Scope::default(),
                    rate_limit_per_hour: None,
                };
                v.as_object_mut()
                    .unwrap()
                    .insert("policy".into(), serde_json::to_value(p).unwrap());
            }
            std::fs::write(
                dir.join(format!("{}.json", self.fp(&self.peer))),
                serde_json::to_vec_pretty(&v).unwrap(),
            )
            .unwrap();
        }

        fn spool(&self) -> Spool {
            Spool::new(self.home.path()).unwrap()
        }

        fn put(&self, state: &str, meta: Value) -> String {
            self.put_with_path(Some("src/auth/session.rs"), state, meta)
        }

        fn put_with_path(&self, path: Option<&str>, state: &str, meta: Value) -> String {
            let q = Payload::question(
                &self.fp(&self.peer),
                &self.fp(&self.me),
                PROJECT,
                path,
                "Where is the retry policy?",
            );
            let env = Envelope::sign(&q, &self.peer);
            self.spool()
                .put(
                    Dir::Inbox,
                    &q.id,
                    &Record {
                        raw: env.raw,
                        sig: env.sig,
                        state: state.into(),
                        seen: false,
                        received_at: envelope::rfc3339_now(),
                        draft: None,
                        meta,
                    },
                )
                .unwrap();
            q.id
        }

        fn attempt(&self, id: &str) -> Outcome {
            attempt(
                self.home.path(),
                self.home.path(),
                &self.config,
                &self.spool(),
                id,
            )
            .unwrap()
        }

        fn inbox(&self, id: &str) -> Option<Record> {
            self.spool().get(Dir::Inbox, id).unwrap()
        }

        fn outbox_len(&self) -> usize {
            self.spool().list(Dir::Outbox, |_| true).unwrap().len()
        }

        fn log_lines(&self) -> Vec<Value> {
            std::fs::read_to_string(self.home.path().join(answer::OUTGOING_LOG))
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }
    }

    fn peer_meta(f: &Fixture) -> Value {
        json!({ "peer": f.fp(&f.peer), "hash": "h" })
    }

    /// A repo-level question is auto-answered like any other; the outcome (and so
    /// the "auto-answered … about" notification) carries `-` for the path.
    #[test]
    fn auto_policy_answers_a_question_without_path() {
        let f = Fixture::new(Some(Mode::Auto), true);
        let id = f.put_with_path(None, "pending", peer_meta(&f));
        let out = f.attempt(&id);
        let Outcome::Sent { answer, peer, path } = out else {
            panic!("{out:?}");
        };
        assert_eq!(peer, f.fp(&f.peer));
        assert_eq!(path, "-");
        assert_eq!(answer.to, f.fp(&f.peer));
        assert!(f.inbox(&id).is_none());
        assert_eq!(f.outbox_len(), 1);
    }

    #[test]
    fn auto_policy_sends_and_logs() {
        let f = Fixture::new(Some(Mode::Auto), true);
        let id = f.put("pending", peer_meta(&f));
        let out = f.attempt(&id);
        let Outcome::Sent { answer, peer, path } = out else {
            panic!("{out:?}");
        };
        assert_eq!(peer, f.fp(&f.peer));
        assert_eq!(path, "src/auth/session.rs");
        assert_eq!(answer.to, f.fp(&f.peer));
        assert_eq!(answer.redactions, 1);
        assert_eq!(answer.harness, "fake");
        assert!(f.inbox(&id).is_none());
        let done = f.spool().get(Dir::Done, &id).unwrap().unwrap();
        assert_eq!(done.state, "answered");
        assert_eq!(done.meta["answer_id"], answer.answer_id);
        assert!(done.meta.get(AUTO_ERROR).is_none());
        assert_eq!(f.outbox_len(), 1);
        let lines = f.log_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["mode"], "auto");
        assert_eq!(lines[0]["question_id"], id);
        assert_eq!(lines[0]["redactions"], 1);
        // A second attempt on the finished record is a no-op.
        assert!(matches!(f.attempt(&id), Outcome::Skipped(_)));
        assert_eq!(f.log_lines().len(), 1);
    }

    /// The policy is read at attempt time: manual, never and absent all skip, nothing is run,
    /// nothing is written, no `auto_error`.
    #[test]
    fn policy_is_rechecked_at_attempt_time() {
        for mode in [Some(Mode::Manual), Some(Mode::Never), None] {
            let f = Fixture::new(Some(Mode::Auto), true);
            let id = f.put("pending", peer_meta(&f));
            // Flipped after the question was spooled (as the daemon's event would have seen it).
            f.write_contact(mode);
            let out = f.attempt(&id);
            assert!(matches!(out, Outcome::Skipped(_)), "{mode:?}: {out:?}");
            let rec = f.inbox(&id).unwrap();
            assert_eq!(rec.state, "pending", "{mode:?}");
            assert_eq!(answer::auto_error(&rec), None, "{mode:?}");
            assert!(rec.draft.is_none());
            assert_eq!(f.outbox_len(), 0, "{mode:?}");
            assert!(f.log_lines().is_empty(), "{mode:?}");
            assert!(
                !f.home.path().join("tmp").exists(),
                "{mode:?}: runner must not have run"
            );
        }
    }

    /// `responder.enabled = false` (non-default) skips even an `auto` peer.
    #[test]
    fn disabled_responder_skips() {
        let mut f = Fixture::new(Some(Mode::Auto), true);
        f.config.responder.enabled = false;
        let id = f.put("pending", peer_meta(&f));
        let out = f.attempt(&id);
        assert!(
            matches!(&out, Outcome::Skipped(why) if why.contains("responder is disabled")),
            "{out:?}"
        );
        assert_eq!(f.inbox(&id).unwrap().state, "pending");
        assert_eq!(f.outbox_len(), 0);
    }

    /// Only `pending` questions are attempted: consent, drafted, denied and answer records skip.
    #[test]
    fn non_pending_states_skip() {
        let f = Fixture::new(Some(Mode::Auto), true);
        for state in ["consent", "drafted", "denied", "seen"] {
            let id = f.put(state, peer_meta(&f));
            let out = f.attempt(&id);
            assert!(matches!(out, Outcome::Skipped(_)), "{state}: {out:?}");
            assert_eq!(f.inbox(&id).unwrap().state, state);
        }
        assert_eq!(f.outbox_len(), 0);
        assert!(matches!(f.attempt("nope"), Outcome::Skipped(_)));
    }

    /// Unknown project: the record stays `pending`, `auto_error` names it, no outbox, no log.
    #[test]
    fn unknown_project_is_recorded() {
        let f = Fixture::new(Some(Mode::Auto), false);
        let id = f.put("pending", peer_meta(&f));
        let out = f.attempt(&id);
        let Outcome::Failed(why) = out else {
            panic!("{out:?}");
        };
        assert!(why.contains("unknown project"), "{why}");
        let rec = f.inbox(&id).unwrap();
        assert_eq!(rec.state, "pending");
        assert!(rec.draft.is_none());
        let stored = answer::auto_error(&rec).unwrap();
        assert!(
            stored.contains(&format!("unknown project {PROJECT}")),
            "{stored}"
        );
        assert_eq!(rec.meta["peer"], f.fp(&f.peer), "existing meta keys kept");
        assert_eq!(f.outbox_len(), 0);
        assert!(f.log_lines().is_empty());
        // Failed records are not scan candidates any more; a fresh one is.
        let book = ContactBook::load(f.home.path(), f.home.path()).unwrap();
        assert_eq!(
            candidates(&f.config, &book, &f.spool()).unwrap(),
            Vec::<String>::new()
        );
        let fresh = f.put("pending", peer_meta(&f));
        assert_eq!(
            candidates(&f.config, &book, &f.spool()).unwrap(),
            vec![fresh]
        );
    }

    /// Timeout and `extract_failed` drafts are never sent automatically.
    #[test]
    fn bad_draft_status_is_recorded() {
        let mut f = Fixture::new(Some(Mode::Auto), true);
        f.config.responder.timeout_secs = 1;
        f.config
            .harnesses
            .get_mut("fake")
            .unwrap()
            .env
            .insert("FAKE_SLEEP".into(), "3".into());
        let id = f.put("pending", peer_meta(&f));
        let out = f.attempt(&id);
        assert!(
            matches!(&out, Outcome::Failed(why) if why.contains("timeout")),
            "{out:?}"
        );
        let rec = f.inbox(&id).unwrap();
        assert_eq!(rec.state, "pending");
        assert!(rec.draft.is_none());
        assert!(answer::auto_error(&rec).unwrap().contains("timeout"));

        let mut f = Fixture::new(Some(Mode::Auto), true);
        f.config.harnesses.get_mut("fake").unwrap().answer_path = "result".into();
        let id = f.put("pending", peer_meta(&f));
        let out = f.attempt(&id);
        assert!(
            matches!(&out, Outcome::Failed(why) if why.contains("extract_failed")),
            "{out:?}"
        );
        assert_eq!(f.inbox(&id).unwrap().state, "pending");
        assert_eq!(f.outbox_len(), 0);
        assert!(f.log_lines().is_empty());
    }

    /// A non-object `meta` (scalar, array, null) must not panic: it is replaced by an object
    /// carrying `auto_error`.
    #[test]
    fn wrong_shape_meta_does_not_panic() {
        let f = Fixture::new(Some(Mode::Auto), false);
        for bad in [json!("oops"), json!([1]), Value::Null, json!(7)] {
            let id = f.put("pending", bad.clone());
            assert!(matches!(f.attempt(&id), Outcome::Failed(_)), "{bad}");
            let rec = f.inbox(&id).unwrap();
            assert!(rec.meta.is_object(), "{bad}: {}", rec.meta);
            assert!(
                answer::auto_error(&rec)
                    .unwrap()
                    .contains("unknown project")
            );
        }
        // And on the success path too.
        let f = Fixture::new(Some(Mode::Auto), true);
        let id = f.put("pending", json!("oops"));
        assert!(matches!(f.attempt(&id), Outcome::Sent { .. }));
        let done = f.spool().get(Dir::Done, &id).unwrap().unwrap();
        assert!(done.meta["answer_id"].is_string());
    }

    /// Candidates exclude records from non-auto peers and non-pending records.
    #[test]
    fn candidates_filter_by_policy_and_state() {
        let f = Fixture::new(Some(Mode::Manual), true);
        let pending = f.put("pending", peer_meta(&f));
        f.put("consent", peer_meta(&f));
        let book = ContactBook::load(f.home.path(), f.home.path()).unwrap();
        assert!(candidates(&f.config, &book, &f.spool()).unwrap().is_empty());
        f.write_contact(Some(Mode::Auto));
        let book = ContactBook::load(f.home.path(), f.home.path()).unwrap();
        assert_eq!(
            candidates(&f.config, &book, &f.spool()).unwrap(),
            vec![pending]
        );
        let mut off = f.config.clone();
        off.responder.enabled = false;
        assert!(candidates(&off, &book, &f.spool()).unwrap().is_empty());
    }

    /// A stale `auto_error` (from an earlier failure) is dropped when the attempt succeeds.
    #[test]
    fn success_clears_stale_auto_error() {
        let f = Fixture::new(Some(Mode::Auto), true);
        let id = f.put(
            "pending",
            json!({ "peer": f.fp(&f.peer), AUTO_ERROR: "unknown project x" }),
        );
        assert!(matches!(f.attempt(&id), Outcome::Sent { .. }));
        let done = f.spool().get(Dir::Done, &id).unwrap().unwrap();
        assert!(done.meta.get(AUTO_ERROR).is_none(), "{}", done.meta);
        assert_eq!(done.meta["peer"], f.fp(&f.peer));
    }

    /// `done/` blocked: the envelope is already in `outbox/` and logged, so the record stays
    /// `drafted` (not `pending`) with `auto_error`; the human's `owl send` then reuses the
    /// envelope and the log keeps exactly one line.
    #[test]
    fn send_failure_after_envelope_keeps_record_drafted() {
        let f = Fixture::new(Some(Mode::Auto), true);
        let id = f.put("pending", peer_meta(&f));
        let blocker = f.spool().path(Dir::Done, &id);
        std::fs::create_dir_all(blocker.join("child")).unwrap();
        let out = f.attempt(&id);
        let Outcome::Failed(why) = out else {
            panic!("{out:?}");
        };
        assert!(why.contains("finishing record"), "{why}");
        let rec = f.inbox(&id).unwrap();
        assert_eq!(rec.state, "drafted");
        assert!(rec.draft.is_some(), "draft kept for the reuse path");
        assert_eq!(answer::auto_error(&rec), Some(why.as_str()));
        assert_eq!(f.outbox_len(), 1);
        assert_eq!(f.log_lines().len(), 1, "logged before finish");
        // Still not a scan candidate; a second attempt skips (state is drafted).
        assert!(matches!(f.attempt(&id), Outcome::Skipped(_)));
        assert_eq!(f.outbox_len(), 1);

        std::fs::remove_dir_all(&blocker).unwrap();
        let sent = answer::send(
            f.home.path(),
            &f.spool(),
            &id,
            f.inbox(&id).unwrap(),
            SendMode::Manual,
        )
        .unwrap();
        assert!(f.inbox(&id).is_none());
        let done = f.spool().get(Dir::Done, &id).unwrap().unwrap();
        assert_eq!(done.state, "answered");
        assert_eq!(done.meta["answer_id"], sent.answer_id);
        assert_eq!(f.outbox_len(), 1, "envelope reused, not re-signed");
        let lines = f.log_lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0]["mode"], "auto", "the first (auto) line stands");
    }

    /// The log blocked (a directory where the file should be): the envelope is spooled, the
    /// log append fails before `finish`, the record stays `drafted` with `auto_error`, nothing
    /// is in `done/`.
    #[test]
    fn send_failure_on_log_keeps_record_drafted_and_unfinished() {
        let f = Fixture::new(Some(Mode::Auto), true);
        let id = f.put("pending", peer_meta(&f));
        let log = f.home.path().join(answer::OUTGOING_LOG);
        std::fs::create_dir_all(&log).unwrap();
        let out = f.attempt(&id);
        let Outcome::Failed(why) = out else {
            panic!("{out:?}");
        };
        assert!(why.contains("outgoing.jsonl"), "{why}");
        let rec = f.inbox(&id).unwrap();
        assert_eq!(rec.state, "drafted");
        assert!(answer::auto_error(&rec).is_some());
        assert_eq!(f.outbox_len(), 1);
        assert!(f.spool().get(Dir::Done, &id).unwrap().is_none());
        std::fs::remove_dir(&log).unwrap();
        answer::send(
            f.home.path(),
            &f.spool(),
            &id,
            f.inbox(&id).unwrap(),
            SendMode::Manual,
        )
        .unwrap();
        assert_eq!(f.log_lines().len(), 1);
        assert_eq!(f.log_lines()[0]["mode"], "manual");
        assert_eq!(f.outbox_len(), 1);
        assert_eq!(
            f.spool().get(Dir::Done, &id).unwrap().unwrap().state,
            "answered"
        );
    }

    /// `outbox/` unwritable: `send` fails before any envelope exists, so the record goes back
    /// to `pending` with the draft dropped and `auto_error` set — nothing spooled, logged or
    /// finished. (The sibling arm, envelope present → stay `drafted`, is tested above.)
    #[test]
    fn send_failure_before_envelope_resets_to_pending() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new(Some(Mode::Auto), true);
        let id = f.put("pending", peer_meta(&f));
        let outbox = f.home.path().join("spool").join(Dir::Outbox.name());
        let chmod = |mode: u32| {
            std::fs::set_permissions(&outbox, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        chmod(0o555);
        let out = f.attempt(&id);
        chmod(0o755);
        let Outcome::Failed(why) = out else {
            panic!("{out:?}");
        };
        assert!(why.contains("outbox"), "{why}");
        let rec = f.inbox(&id).unwrap();
        assert_eq!(rec.state, "pending");
        assert!(rec.draft.is_none(), "draft dropped: {:?}", rec.draft);
        assert_eq!(answer::auto_error(&rec), Some(why.as_str()));
        assert!(
            answer::auto_error(&rec).unwrap().contains("denied"),
            "{why}"
        );
        assert_eq!(rec.meta["peer"], f.fp(&f.peer), "existing meta keys kept");
        assert_eq!(f.outbox_len(), 0);
        assert!(f.log_lines().is_empty());
        assert!(f.spool().get(Dir::Done, &id).unwrap().is_none());
        assert!(f.spool().list(Dir::Done, |_| true).unwrap().is_empty());
        // Not a scan candidate until the human acts.
        let book = ContactBook::load(f.home.path(), f.home.path()).unwrap();
        assert!(candidates(&f.config, &book, &f.spool()).unwrap().is_empty());
    }

    #[test]
    fn scheduler_claims_once() {
        let s = Scheduler::new();
        assert!(s.claim("a"));
        assert!(!s.claim("a"));
        assert!(s.claim("b"));
        assert_eq!(s.in_flight(), 2);
        s.release("a");
        assert_eq!(s.in_flight(), 1);
        assert!(s.claim("a"));
        let t = s.clone();
        assert!(!t.claim("a"), "clones share the set");
    }
}
