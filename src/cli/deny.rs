//! `owl deny <peer>` (§5, §8, §9): policy `never` in the local overlay, then every held
//! `consent` record from that peer is finished into `done/` with state `denied` (done copy
//! first, inbox unlink after — the shared `finish`). From now on the daemon answers that peer
//! `403 unavailable`, indistinguishable from being offline.

use std::path::Path;

use owlpost::contacts::{Mode, Policy, Scope};
use owlpost::events::Ev;
use owlpost::spool::{Dir, Spool};
use serde_json::json;

use super::{contact_book, finish, payload_of, print_json, user_error};

/// Finishes every `consent` record from `fingerprint` as `denied`; returns their ids.
pub fn deny_held(spool: &Spool, fingerprint: &str) -> anyhow::Result<Vec<String>> {
    let mut denied = Vec::new();
    for (id, rec) in spool.list(Dir::Inbox, |r| r.state == "consent")? {
        if payload_of(&id, &rec)?.from != fingerprint {
            continue;
        }
        finish(
            spool,
            &id,
            rec,
            "denied",
            &[("previous_state", json!("consent"))],
            Ev::by("denied", "human"),
        )?;
        denied.push(id);
    }
    Ok(denied)
}

pub fn run(home: &Path, peer: &str, json: bool) -> anyhow::Result<()> {
    let mut book = contact_book(home)?;
    let contact = book.resolve(peer).map_err(|e| user_error(e.to_string()))?;
    let fingerprint = contact.fingerprint.clone();
    let name = contact.name.clone();
    book.set_policy(
        home,
        &fingerprint,
        Policy {
            mode: Mode::Never,
            scope: Scope::default(),
            rate_limit_per_hour: None,
        },
    )?;
    let spool = Spool::new(home)?;
    let denied = deny_held(&spool, &fingerprint)?;
    if json {
        print_json(&json!({
            "peer": name,
            "fingerprint": fingerprint,
            "policy": "never",
            "denied": denied,
        }))?;
    } else {
        // The name is whatever the peer's pasted file says: one line on the terminal.
        let name = owlpost::render::peer_name_line(&book, &fingerprint);
        println!(
            "denied {name} ({fingerprint}): policy never, {} held question{} moved to done",
            denied.len(),
            if denied.len() == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use owlpost::envelope::{self, Envelope, Payload};
    use owlpost::identity::{self, Identity};
    use owlpost::spool::Record;
    use std::os::unix::fs::PermissionsExt;

    fn fp(id: &Identity) -> String {
        identity::fingerprint(&id.verifying_key())
    }

    fn put(spool: &Spool, from: &Identity, to: &Identity, state: &str) -> String {
        let q = Payload::question(&fp(from), &fp(to), "p", Some("f"), "why?");
        let env = Envelope::sign(&q, from);
        spool
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
                    meta: json!({ "peer": fp(from) }),
                },
            )
            .unwrap();
        q.id
    }

    #[test]
    fn deny_held_moves_only_consent_records_of_that_peer() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (me, ana, bob) = (
            Identity::from_seed([2; 32]),
            Identity::from_seed([1; 32]),
            Identity::from_seed([3; 32]),
        );
        let held = put(&spool, &ana, &me, "consent");
        let pending = put(&spool, &ana, &me, "pending");
        let bobs = put(&spool, &bob, &me, "consent");
        let mut denied = deny_held(&spool, &fp(&ana)).unwrap();
        denied.sort();
        assert_eq!(denied, vec![held.clone()]);
        assert!(spool.get(Dir::Inbox, &held).unwrap().is_none());
        let d = spool.get(Dir::Done, &held).unwrap().unwrap();
        assert_eq!(d.state, "denied");
        assert_eq!(d.meta["previous_state"], "consent");
        assert_eq!(d.meta["peer"], fp(&ana));
        assert!(d.meta["done_at"].is_string());
        assert_eq!(
            spool.get(Dir::Inbox, &pending).unwrap().unwrap().state,
            "pending"
        );
        assert_eq!(
            spool.get(Dir::Inbox, &bobs).unwrap().unwrap().state,
            "consent"
        );
        // Nothing held: empty, no error.
        assert!(deny_held(&spool, &fp(&ana)).unwrap().is_empty());
        assert!(deny_held(&spool, "nobody").unwrap().is_empty());
    }

    /// `done/<id>.json` blocked by a non-empty directory: the error surfaces, the inbox record
    /// is byte-for-byte untouched, and the retry after unblocking succeeds.
    #[test]
    fn deny_held_blocked_done_leaves_inbox_untouched() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (me, ana) = (Identity::from_seed([2; 32]), Identity::from_seed([1; 32]));
        let held = put(&spool, &ana, &me, "consent");
        let before = std::fs::read(spool.path(Dir::Inbox, &held)).unwrap();
        let blocker = spool.path(Dir::Done, &held);
        std::fs::create_dir_all(blocker.join("child")).unwrap();
        let err = format!("{:#}", deny_held(&spool, &fp(&ana)).unwrap_err());
        assert!(err.contains(&format!("finishing record {held}")), "{err}");
        assert_eq!(
            std::fs::read(spool.path(Dir::Inbox, &held)).unwrap(),
            before
        );
        assert!(blocker.is_dir());
        std::fs::remove_dir_all(&blocker).unwrap();
        assert_eq!(deny_held(&spool, &fp(&ana)).unwrap(), vec![held.clone()]);
        assert_eq!(
            spool.get(Dir::Done, &held).unwrap().unwrap().state,
            "denied"
        );
        assert!(spool.get(Dir::Inbox, &held).unwrap().is_none());
    }

    /// `inbox/` read-only: the `done/` copy is written, the unlink fails, the original stays
    /// `consent`; the retry rewrites `done/` and removes the original.
    #[test]
    fn deny_held_readonly_inbox_leaves_original_and_retries() {
        let home = tempfile::tempdir().unwrap();
        let spool = Spool::new(home.path()).unwrap();
        let (me, ana) = (Identity::from_seed([2; 32]), Identity::from_seed([1; 32]));
        let held = put(&spool, &ana, &me, "consent");
        let inbox_dir = spool
            .path(Dir::Inbox, &held)
            .parent()
            .unwrap()
            .to_path_buf();
        std::fs::set_permissions(&inbox_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = deny_held(&spool, &fp(&ana));
        std::fs::set_permissions(&inbox_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("removing"), "{err}");
        assert_eq!(
            spool.get(Dir::Inbox, &held).unwrap().unwrap().state,
            "consent"
        );
        assert_eq!(
            spool.get(Dir::Done, &held).unwrap().unwrap().state,
            "denied"
        );
        assert_eq!(deny_held(&spool, &fp(&ana)).unwrap(), vec![held.clone()]);
        assert!(spool.get(Dir::Inbox, &held).unwrap().is_none());
        assert_eq!(
            spool.get(Dir::Done, &held).unwrap().unwrap().state,
            "denied"
        );
    }
}
