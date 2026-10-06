//! Per-record event log (§8): `meta.events` on a spool record — what happened to
//! this request and when, in one chronological list, so `owl thread` can show the whole
//! conversation with one person without a sidecar file and without a new wire field.
//!
//! The `kind` is an **open string**, never an enum matched exhaustively: content requests and tool calls
//! each add kinds to the same timeline, and an unknown one must pass through a reader of this
//! version untouched.
//!
//! Records written before the event log existed carry no log; [`of`] derives one on read from
//! `received_at`, the record's directory and `meta.done_at`. Deriving never writes: a
//! read-only command must not migrate a legacy record.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::answer::meta_object;
use crate::envelope;
use crate::spool::{Dir, Record};

/// `meta` key holding the log.
pub const EVENTS_KEY: &str = "events";

/// Events kept per record. Beyond it [`push`] drops the oldest event *after* the first one,
/// so the anchor (`received` / `asked`) always stays.
pub const EVENTS_MAX: usize = 200;

/// The kind whose repeats are collapsed: a tight `owl status --wait` poll must not fill the
/// log with identical peer-state lines.
pub const STATE_SEEN: &str = "state-seen";

/// One entry of `meta.events`. `by` and `detail` are omitted from the JSON when absent, so a
/// bookkeeping event is exactly two keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub ts: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

/// An event to append, as the write points hand it over (`answer::finish` takes one).
#[derive(Debug, Clone, PartialEq)]
pub struct Ev<'a> {
    pub kind: &'a str,
    pub by: Option<&'a str>,
    pub detail: Option<Value>,
}

impl<'a> Ev<'a> {
    pub fn by(kind: &'a str, by: &'a str) -> Ev<'a> {
        Ev {
            kind,
            by: Some(by),
            detail: None,
        }
    }
}

/// Appends `{ts, kind}` (plus `by`/`detail` when given) to `rec.meta.events`, stamping the
/// current time. A non-object `meta` is repaired by [`meta_object`] exactly as elsewhere; a
/// `meta.events` that is not an array is replaced by a fresh one.
///
/// A [`STATE_SEEN`] event whose `detail.state` equals the previous `state-seen`'s is dropped
/// (the peer has not moved), so polling cannot fill the log.
pub fn push(rec: &mut Record, kind: &str, by: Option<&str>, detail: Option<Value>) {
    let ev = Event {
        ts: envelope::rfc3339_now(),
        kind: kind.to_string(),
        by: by.map(str::to_string),
        detail,
    };
    let meta = meta_object(rec);
    let list = match meta.get_mut(EVENTS_KEY) {
        Some(Value::Array(a)) => a,
        _ => {
            meta.insert(EVENTS_KEY.into(), json!([]));
            match meta.get_mut(EVENTS_KEY) {
                Some(Value::Array(a)) => a,
                _ => unreachable!("events was just set to an array"),
            }
        }
    };
    if ev.kind == STATE_SEEN && last_state_seen(list) == Some(state_of(&ev.detail)) {
        return;
    }
    let Ok(value) = serde_json::to_value(&ev) else {
        return;
    };
    list.push(value);
    // Over the cap: drop from index 1 up, never the anchor at index 0.
    while list.len() > EVENTS_MAX {
        list.remove(1);
    }
}

/// [`push`] for a prepared [`Ev`].
pub fn push_ev(rec: &mut Record, ev: Ev<'_>) {
    push(rec, ev.kind, ev.by, ev.detail);
}

/// `detail.state` of the newest `state-seen` entry already in `list`; the outer `None` means
/// the record has no `state-seen` yet, so nothing can collapse.
fn last_state_seen(list: &[Value]) -> Option<Option<String>> {
    list.iter()
        .rev()
        .find(|v| v.get("kind").and_then(Value::as_str) == Some(STATE_SEEN))
        .map(|v| {
            v.get("detail")
                .and_then(|d| d.get("state"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn state_of(detail: &Option<Value>) -> Option<String> {
    detail
        .as_ref()
        .and_then(|d| d.get("state"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The record's event log: the stored one when `meta.events` is a non-empty array, else the
/// derived log of a legacy record. Never writes and never mixes the two.
///
/// Derived: one event at `received_at` — `asked` for an `asks/` record, `received` everywhere
/// else — plus, when `meta.done_at` is set, one event at that timestamp whose kind comes from
/// the record's state.
pub fn of(dir: Dir, rec: &Record) -> Vec<Event> {
    if let Some(Value::Array(a)) = rec.meta.get(EVENTS_KEY)
        && !a.is_empty()
    {
        return a
            .iter()
            .filter_map(|v| serde_json::from_value::<Event>(v.clone()).ok())
            .collect();
    }
    derived(dir, rec)
}

fn derived(dir: Dir, rec: &Record) -> Vec<Event> {
    let birth = if dir == Dir::Asks {
        "asked"
    } else {
        "received"
    };
    let mut out = vec![Event {
        ts: rec.received_at.clone(),
        kind: birth.to_string(),
        by: None,
        detail: None,
    }];
    let Some(done_at) = rec.meta.get("done_at").and_then(Value::as_str) else {
        return out;
    };
    let (kind, detail) = match (dir, rec.state.as_str()) {
        (Dir::Asks, "answered") => ("answer-received", None),
        (_, "answered") => ("sent", None),
        (_, "rejected") => ("rejected", None),
        (_, "denied") => ("denied", None),
        (_, "declined") => ("denied", Some(json!({ "by": "peer" }))),
        (_, "acked" | "expired") => ("sent", None),
        _ => return out,
    };
    out.push(Event {
        ts: done_at.to_string(),
        kind: kind.to_string(),
        by: None,
        detail,
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(meta: Value) -> Record {
        Record {
            raw: String::new(),
            sig: String::new(),
            state: "pending".into(),
            seen: false,
            received_at: "2026-09-01T10:00:00Z".into(),
            draft: None,
            meta,
        }
    }

    fn kinds(r: &Record) -> Vec<String> {
        r.meta[EVENTS_KEY]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect()
    }

    /// The appended shape — two keys without `by`/`detail`, four with them — and a
    /// non-object `meta` repaired rather than indexed into.
    #[test]
    fn push_appends_the_documented_shape_and_repairs_meta() {
        for broken in [Value::Null, json!(7), json!("text"), json!([1, 2])] {
            let mut r = rec(broken.clone());
            push(&mut r, "received", None, None);
            assert!(r.meta.is_object(), "meta repaired for {broken}");
            let e = &r.meta[EVENTS_KEY][0];
            assert_eq!(e["kind"], "received");
            assert!(e["ts"].as_str().unwrap().ends_with('Z'), "{e}");
            let mut keys: Vec<&String> = e.as_object().unwrap().keys().collect();
            keys.sort();
            assert_eq!(keys, vec!["kind", "ts"], "absent optionals are omitted");
        }
        let mut r = rec(json!({ "peer": "owl:x" }));
        push(
            &mut r,
            "allowed",
            Some("human"),
            Some(json!({"scope":"once"})),
        );
        let e = &r.meta[EVENTS_KEY][0];
        assert_eq!(e["by"], "human");
        assert_eq!(e["detail"]["scope"], "once");
        assert_eq!(r.meta["peer"], "owl:x", "existing meta keys survive");
        // A `meta.events` of the wrong type is replaced, not indexed into.
        let mut r = rec(json!({ "events": 5 }));
        push(&mut r, "received", None, None);
        assert_eq!(kinds(&r), ["received"]);
    }

    /// At the cap nothing drops; past it the second event goes and the anchor stays.
    #[test]
    fn push_caps_the_log_and_keeps_the_first_event() {
        let mut r = rec(Value::Null);
        push(&mut r, "received", None, None);
        for i in 1..EVENTS_MAX {
            push(&mut r, &format!("k{i}"), None, None);
        }
        let at_cap = kinds(&r);
        assert_eq!(
            at_cap.len(),
            EVENTS_MAX,
            "exactly at the cap: nothing dropped"
        );
        assert_eq!(at_cap[0], "received");
        assert_eq!(at_cap[1], "k1");
        push(&mut r, "over", None, None);
        let over = kinds(&r);
        assert_eq!(over.len(), EVENTS_MAX);
        assert_eq!(over[0], "received", "the anchor is never dropped");
        assert_eq!(over[1], "k2", "the oldest after the anchor went");
        assert_eq!(over[EVENTS_MAX - 1], "over");
    }

    /// `state-seen` collapses only against the previous `state-seen`, so A A B A keeps
    /// three, and a different kind between two equal states does not un-collapse them.
    #[test]
    fn state_seen_dedupes_against_the_previous_state_seen_only() {
        let mut r = rec(Value::Null);
        let seen = |r: &mut Record, s: &str| {
            push(r, STATE_SEEN, Some("peer"), Some(json!({ "state": s })));
        };
        seen(&mut r, "WORKING");
        seen(&mut r, "WORKING");
        seen(&mut r, "INPUT_REQUIRED");
        seen(&mut r, "WORKING");
        let states: Vec<String> = r.meta[EVENTS_KEY]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["detail"]["state"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(states, ["WORKING", "INPUT_REQUIRED", "WORKING"]);
        // A different kind in between does not reset the comparison.
        push(&mut r, "drafted", None, None);
        seen(&mut r, "WORKING");
        assert_eq!(
            kinds(&r).len(),
            4,
            "the repeat after `drafted` still collapses"
        );
    }

    /// A stored log is returned as it stands, and an unknown kind survives the round trip.
    #[test]
    fn of_returns_the_stored_log_unchanged() {
        let r = rec(json!({ "events": [
            { "ts": "2026-09-01T10:00:00Z", "kind": "received" },
            { "ts": "2026-09-01T10:01:00Z", "kind": "content-requested", "by": "peer",
              "detail": { "n": 2 } },
        ]}));
        let got = of(Dir::Inbox, &r);
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].kind, "content-requested");
        assert_eq!(got[1].by.as_deref(), Some("peer"));
        assert_eq!(got[1].detail.as_ref().unwrap()["n"], 2);
        assert_eq!(got[0].by, None);
    }

    /// The whole derivation table, with and without `done_at`, per directory.
    #[test]
    fn of_derives_the_state_table_for_legacy_records() {
        // No stored log, no `done_at`: one birth event, named per directory.
        for (dir, kind) in [
            (Dir::Inbox, "received"),
            (Dir::Outbox, "received"),
            (Dir::Done, "received"),
            (Dir::Asks, "asked"),
        ] {
            let got = of(dir, &rec(json!({ "peer": "owl:x" })));
            assert_eq!(got.len(), 1, "{dir:?}");
            assert_eq!(got[0].kind, kind, "{dir:?}");
            assert_eq!(got[0].ts, "2026-09-01T10:00:00Z", "{dir:?}");
        }
        // `done_at` present: the second event's kind comes from the state.
        let done = "2026-09-01T11:00:00Z";
        for (dir, state, kind) in [
            (Dir::Done, "answered", Some("sent")),
            (Dir::Asks, "answered", Some("answer-received")),
            (Dir::Done, "rejected", Some("rejected")),
            (Dir::Done, "denied", Some("denied")),
            (Dir::Done, "declined", Some("denied")),
            (Dir::Done, "acked", Some("sent")),
            (Dir::Done, "expired", Some("sent")),
            (Dir::Done, "pending", None),
            (Dir::Done, "consent", None),
        ] {
            let mut r = rec(json!({ "done_at": done }));
            r.state = state.into();
            let got = of(dir, &r);
            match kind {
                None => assert_eq!(got.len(), 1, "{state}: no second event"),
                Some(k) => {
                    assert_eq!(got.len(), 2, "{state}");
                    assert_eq!(got[1].kind, k, "{state}");
                    assert_eq!(got[1].ts, done, "{state}");
                }
            }
        }
        // `declined` is the only derived kind carrying a detail.
        let mut r = rec(json!({ "done_at": done }));
        r.state = "declined".into();
        assert_eq!(of(Dir::Done, &r)[1].detail.as_ref().unwrap()["by"], "peer");
        let mut r = rec(json!({ "done_at": done }));
        r.state = "rejected".into();
        assert_eq!(of(Dir::Done, &r)[1].detail, None);
    }

    /// An empty or malformed `meta.events` falls back to the derived log rather than
    /// returning nothing.
    #[test]
    fn of_falls_back_when_the_stored_log_is_empty_or_not_an_array() {
        for meta in [
            json!({ "events": [] }),
            json!({ "events": {} }),
            json!({ "events": "x" }),
        ] {
            let got = of(Dir::Inbox, &rec(meta.clone()));
            assert_eq!(got.len(), 1, "{meta}");
            assert_eq!(got[0].kind, "received", "{meta}");
        }
    }
}
