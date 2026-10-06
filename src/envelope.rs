//! Signed payload envelope (§6): `Payload` JSON, `Envelope { raw, sig }`, question hash,
//! replay window (`is_fresh`, `SeenIds`).

use std::path::Path;

use anyhow::Context;
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::identity::{self, Identity};

pub const REPLAY_WINDOW_SECS: u64 = 300;
pub const SEEN_IDS_TTL_SECS: u64 = 600;
const SEEN_IDS_FILE: &str = "seen-ids.txt";
/// Largest `body.context` (the asker's snippet) after trimming, in bytes.
pub const MAX_CONTEXT_BYTES: usize = 8192;
/// The longest `body.project` or `body.path` a question may carry, in bytes.
pub const MAX_PATH_BYTES: usize = 1024;
/// Largest `body.content` a content reply carries, in bytes after redaction.
/// Beyond it the content is cut at the last complete character and `truncated` goes out
/// `true`; `sha256` stays the digest of the whole redacted content, so the asker can tell it
/// holds a prefix.
pub const MAX_CONTENT_BYTES: usize = 262_144;
/// Largest `body.output` a tool reply carries, in bytes after redaction. Same cap
/// and same character-boundary cut as [`MAX_CONTENT_BYTES`], with `truncated` going out
/// `true` when it bites. The output carries no digest: it is the record of one run, not a
/// file the asker can re-derive.
pub const MAX_OUTPUT_BYTES: usize = 262_144;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Payload {
    pub v: u32,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    pub from: String,
    pub to: String,
    pub ts: String,
    pub in_reply_to: Option<String>,
    /// Thread id (A2A `contextId`): set by `owl ask`, copied by `--reply-to` and by
    /// the answer. Omitted on the wire when absent; `null` reads as absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    pub body: Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Question,
    Answer,
    /// A peer asks for one file at a ref of a named project, or one memory entry.
    #[serde(rename = "content")]
    Content,
    /// The reply carrying that content. `rename_all = "lowercase"` alone would
    /// spell it `contentreply`, hence the explicit rename.
    #[serde(rename = "content-reply")]
    ContentReply,
    /// A peer names a tool of the owner's registry and an input object. Nothing
    /// runs until the owner allows the record and types `owl draft`.
    #[serde(rename = "tool-call")]
    ToolCall,
    /// The reply carrying that run's output and exit code.
    #[serde(rename = "tool-reply")]
    ToolReply,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Body {
    Question {
        project: String,
        /// Omitted on the wire when the question is about the repository as a whole.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        question: String,
        /// The asker's snippet (a diff, an error, an excerpt), ≤ `MAX_CONTEXT_BYTES` after
        /// trimming; omitted on the wire when absent, `null` reads as absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },
    Answer {
        answer: String,
        harness: String,
        redactions: u32,
        cached: bool,
    },
    /// Content reply body. Declared **before** [`Body::Content`] on purpose: every field of
    /// `Content` is optional, so an untagged `Content` would swallow a reply body and leave
    /// every field `None`. `ContentReply` requires five keys a request never carries, so the
    /// order is safe in both directions.
    ContentReply {
        content: String,
        sha256: String,
        /// The commit the ref resolved to; absent for a `memory` request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ref_resolved: Option<String>,
        truncated: bool,
        redactions: u32,
        /// Always `"human"`: no harness and no model run on this path.
        harness: String,
    },
    /// Tool-call request body. Declared before [`Body::Content`] for the same reason
    /// [`Body::ContentReply`] is: `Content` accepts any object, so it would swallow this one.
    /// `tool` and `input` are both required, so nothing else is swallowed by it either.
    ToolCall {
        tool: String,
        input: serde_json::Map<String, serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
    },
    /// Tool reply body: the one run's output, its exit code and how long it took.
    /// `exit_code` is `-1` when the child was killed by the timeout or by a signal, and
    /// `duration_ms` is then the timeout itself.
    ToolReply {
        output: String,
        exit_code: i32,
        duration_ms: u64,
        truncated: bool,
        redactions: u32,
        /// Always `"human"`: no harness and no model run on this path.
        harness: String,
    },
    /// Content request body: exactly one of `path` (with `project`) and `memory`. Every
    /// field is optional on the wire so the server can name the missing one itself
    /// (`validate_request`) instead of returning a serde schema error.
    Content {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
        git_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        memory: Option<String>,
    },
}

impl Kind {
    /// A kind a peer sends *to* us: it earns a spool record and an A2A `Task`.
    pub fn is_request(self) -> bool {
        matches!(self, Kind::Question | Kind::Content | Kind::ToolCall)
    }

    /// A kind we send *back*: what `GET /v1/outbox` serves and a pull ingests.
    pub fn is_reply(self) -> bool {
        matches!(self, Kind::Answer | Kind::ContentReply | Kind::ToolReply)
    }
}

impl Payload {
    /// New question payload with a fresh UUIDv7 id and the current UTC timestamp.
    pub fn question(
        from: &str,
        to: &str,
        project: &str,
        path: Option<&str>,
        question: &str,
    ) -> Payload {
        Payload {
            v: 1,
            id: uuid::Uuid::now_v7().to_string(),
            kind: Kind::Question,
            from: from.into(),
            to: to.into(),
            ts: rfc3339_now(),
            in_reply_to: None,
            context_id: None,
            body: Body::Question {
                project: project.into(),
                path: path.map(str::to_string),
                question: question.into(),
                context: None,
            },
        }
    }

    /// New answer payload replying to `question`.
    pub fn answer(
        question: &Payload,
        answer: &str,
        harness: &str,
        redactions: u32,
        cached: bool,
    ) -> Payload {
        Payload {
            v: 1,
            id: uuid::Uuid::now_v7().to_string(),
            kind: Kind::Answer,
            from: question.to.clone(),
            to: question.from.clone(),
            ts: rfc3339_now(),
            in_reply_to: Some(question.id.clone()),
            context_id: question.context_id.clone(),
            body: Body::Answer {
                answer: answer.into(),
                harness: harness.into(),
                redactions,
                cached,
            },
        }
    }

    /// New content request payload. Exactly one of `path` (with `project`) and
    /// `memory` is meant to be `Some`; the responder's `validate_request` is what enforces
    /// it, so this constructor stays a plain carrier.
    pub fn content(
        from: &str,
        to: &str,
        project: Option<&str>,
        git_ref: Option<&str>,
        path: Option<&str>,
        memory: Option<&str>,
    ) -> Payload {
        Payload {
            v: 1,
            id: uuid::Uuid::now_v7().to_string(),
            kind: Kind::Content,
            from: from.into(),
            to: to.into(),
            ts: rfc3339_now(),
            in_reply_to: None,
            context_id: None,
            body: Body::Content {
                project: project.map(str::to_string),
                git_ref: git_ref.map(str::to_string),
                path: path.map(str::to_string),
                memory: memory.map(str::to_string),
            },
        }
    }

    /// New content reply payload replying to `request`, mirroring [`Payload::answer`]:
    /// the thread id is copied from the request, `harness` is always `"human"`.
    pub fn content_reply(
        request: &Payload,
        content: &str,
        sha256: &str,
        ref_resolved: Option<&str>,
        truncated: bool,
        redactions: u32,
    ) -> Payload {
        Payload {
            v: 1,
            id: uuid::Uuid::now_v7().to_string(),
            kind: Kind::ContentReply,
            from: request.to.clone(),
            to: request.from.clone(),
            ts: rfc3339_now(),
            in_reply_to: Some(request.id.clone()),
            context_id: request.context_id.clone(),
            body: Body::ContentReply {
                content: content.into(),
                sha256: sha256.into(),
                ref_resolved: ref_resolved.map(str::to_string),
                truncated,
                redactions,
                harness: "human".into(),
            },
        }
    }

    /// New tool-call request payload. The tool name is checked against the
    /// owner's registry by `validate_request`; this constructor stays a plain carrier.
    pub fn tool_call(
        from: &str,
        to: &str,
        tool: &str,
        input: serde_json::Map<String, serde_json::Value>,
        project: Option<&str>,
    ) -> Payload {
        Payload {
            v: 1,
            id: uuid::Uuid::now_v7().to_string(),
            kind: Kind::ToolCall,
            from: from.into(),
            to: to.into(),
            ts: rfc3339_now(),
            in_reply_to: None,
            context_id: None,
            body: Body::ToolCall {
                tool: tool.into(),
                input,
                project: project.map(str::to_string),
            },
        }
    }

    /// New tool reply payload replying to `request`, mirroring
    /// [`Payload::content_reply`]: the thread id is copied from the request, `harness` is
    /// always `"human"`.
    pub fn tool_reply(
        request: &Payload,
        output: &str,
        exit_code: i32,
        duration_ms: u64,
        truncated: bool,
        redactions: u32,
    ) -> Payload {
        Payload {
            v: 1,
            id: uuid::Uuid::now_v7().to_string(),
            kind: Kind::ToolReply,
            from: request.to.clone(),
            to: request.from.clone(),
            ts: rfc3339_now(),
            in_reply_to: Some(request.id.clone()),
            context_id: request.context_id.clone(),
            body: Body::ToolReply {
                output: output.into(),
                exit_code,
                duration_ms,
                truncated,
                redactions,
                harness: "human".into(),
            },
        }
    }

    /// Exact bytes that get signed: compact JSON in §6 field order.
    pub fn to_signed_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("Payload is always serialisable")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub raw: String,
    pub sig: String,
}

impl Envelope {
    pub fn sign(payload: &Payload, identity: &Identity) -> Envelope {
        let bytes = payload.to_signed_bytes();
        let sig = identity.sign(&bytes);
        Envelope {
            raw: String::from_utf8(bytes).expect("serde_json emits UTF-8"),
            sig: identity::sig_string(&sig),
        }
    }

    /// Verifies the signature over the raw bytes BEFORE parsing them.
    pub fn verify(&self, pubkey: &VerifyingKey) -> anyhow::Result<Payload> {
        let sig = identity::parse_sig(&self.sig)?;
        if !identity::verify(pubkey, self.raw.as_bytes(), &sig) {
            anyhow::bail!("signature does not verify");
        }
        serde_json::from_str(&self.raw).context("parsing payload")
    }
}

/// SHA-256 hex of `project\npath\nnorm(question)`; norm = trim, collapse whitespace, lowercase.
/// A missing path hashes as the empty string, so a repo-level question never collides with the
/// same question about a file.
pub fn question_hash(project: &str, path: Option<&str>, question: &str) -> String {
    let path = path.unwrap_or_default();
    let norm = question
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let digest = Sha256::digest(format!("{project}\n{path}\n{norm}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A2A 1.0 `TaskState` names the responder exposes. `INPUT_REQUIRED`,
/// `AUTH_REQUIRED`, `CANCELED` and `FAILED` are never produced.
pub const TASK_STATE_SUBMITTED: &str = "TASK_STATE_SUBMITTED";
pub const TASK_STATE_WORKING: &str = "TASK_STATE_WORKING";
pub const TASK_STATE_COMPLETED: &str = "TASK_STATE_COMPLETED";
pub const TASK_STATE_REJECTED: &str = "TASK_STATE_REJECTED";

/// `(A2A state, status.message text)` for a spooled question:
/// `consent` → SUBMITTED "waiting for the owner's consent"; `pending` → WORKING "the
/// owner's agent is answering" (or, with `auto_error` — a draft timeout or runner error in
/// auto mode — "the owner's agent could not answer; waiting for the owner"); `drafted` →
/// WORKING "the owner is reviewing the answer"; `unacked` (outbox), `acked`, `expired`,
/// `answered` → COMPLETED "answered"; `denied`, `rejected` → REJECTED "the owner declined";
/// anything else (no record, `never` policy, responder disabled) → REJECTED "unavailable".
pub fn a2a_state(spool_state: &str, auto_error: bool) -> (&'static str, &'static str) {
    match spool_state {
        "consent" => (TASK_STATE_SUBMITTED, "waiting for the owner's consent"),
        "pending" if auto_error => (
            TASK_STATE_WORKING,
            "the owner's agent could not answer; waiting for the owner",
        ),
        "pending" => (TASK_STATE_WORKING, "the owner's agent is answering"),
        "drafted" => (TASK_STATE_WORKING, "the owner is reviewing the answer"),
        "unacked" | "acked" | "expired" | "answered" => (TASK_STATE_COMPLETED, "answered"),
        "denied" | "rejected" => (TASK_STATE_REJECTED, "the owner declined"),
        _ => (TASK_STATE_REJECTED, "unavailable"),
    }
}

/// The words `owl ask` prints for a `202` body's `state` (the responder's own text is only
/// on the Task): `None` for a state name this version does not know.
pub fn state_text(state: &str) -> Option<&'static str> {
    match state {
        TASK_STATE_SUBMITTED => Some("waiting for the owner's consent"),
        TASK_STATE_WORKING => Some("the owner's agent is answering"),
        TASK_STATE_COMPLETED => Some("answered"),
        TASK_STATE_REJECTED => Some("the owner declined"),
        _ => None,
    }
}

/// `context` trimmed, or an error naming its byte length when it exceeds
/// [`MAX_CONTEXT_BYTES`]. Bytes, not characters: a Polish snippet counts its `ł` twice.
pub fn check_context(context: &str) -> Result<&str, usize> {
    let trimmed = context.trim();
    if trimmed.len() > MAX_CONTEXT_BYTES {
        Err(trimmed.len())
    } else {
        Ok(trimmed)
    }
}

/// `|now − ts| ≤ window`. Unparsable `ts` is never fresh.
pub fn is_fresh(ts: &str, now_unix: u64, window_secs: u64) -> bool {
    match parse_rfc3339_to_unix(ts) {
        Some(t) => now_unix.abs_diff(t) <= window_secs,
        None => false,
    }
}

/// Recently seen payload ids with their unix timestamps; persisted one `id ts` per line.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SeenIds {
    pub ids: Vec<(String, u64)>,
}

impl SeenIds {
    /// Missing file → empty. Entries older than `SEEN_IDS_TTL_SECS` relative to `now` are pruned.
    pub fn load(home: &Path, now_unix: u64) -> anyhow::Result<SeenIds> {
        let path = home.join(SEEN_IDS_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let ids = text
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                Some((it.next()?.to_string(), it.next()?.parse().ok()?))
            })
            .filter(|(_, ts)| now_unix.saturating_sub(*ts) <= SEEN_IDS_TTL_SECS)
            .collect();
        Ok(SeenIds { ids })
    }

    /// Returns false (and does nothing) if `id` is already present.
    pub fn insert(&mut self, id: &str, now_unix: u64) -> bool {
        if self.ids.iter().any(|(i, _)| i == id) {
            return false;
        }
        self.ids.push((id.to_string(), now_unix));
        true
    }

    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
        let path = home.join(SEEN_IDS_FILE);
        let text: String = self
            .ids
            .iter()
            .map(|(id, ts)| format!("{id} {ts}\n"))
            .collect();
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for the current time.
pub fn rfc3339_now() -> String {
    unix_to_rfc3339(now_unix())
}

// ponytail: UTC 'Z' only — add offsets if a peer ever sends them.
pub fn parse_rfc3339_to_unix(s: &str) -> Option<u64> {
    // Exact shape `YYYY-MM-DDTHH:MM:SSZ`: 20 ASCII bytes, digits only in the numeric fields.
    let b = s.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let field = |from: usize, to: usize| -> Option<i64> {
        let f = &s[from..to];
        if !f.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        f.parse().ok()
    };
    let (y, m, day) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (h, mi, sec) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let days = days_from_civil(y, m, day);
    u64::try_from(days * 86_400 + h * 3_600 + mi * 60 + sec).ok()
}

pub fn unix_to_rfc3339(unix: u64) -> String {
    let unix = unix as i64;
    let days = unix.div_euclid(86_400);
    let rem = unix.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

// Howard Hinnant's days_from_civil / civil_from_days (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0191c7a0-0000-7000-8000-000000000000";
    const TS: &str = "2026-09-01T10:00:00Z";
    const TS_UNIX: u64 = 1_788_256_800;

    fn fixed_question() -> Payload {
        Payload {
            v: 1,
            id: ID.into(),
            kind: Kind::Question,
            from: "owl:aaaaaaaaaaaaaaaa".into(),
            to: "owl:bbbbbbbbbbbbbbbb".into(),
            ts: TS.into(),
            in_reply_to: None,
            context_id: None,
            body: Body::Question {
                project: "github.com/company/monorepo".into(),
                path: Some("src/auth/session.rs".into()),
                question: "Why is the refresh token rotated on every read?".into(),
                context: None,
            },
        }
    }

    #[test]
    fn golden_question_json_matches_design_section_6() {
        let json = String::from_utf8(fixed_question().to_signed_bytes()).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"id":"0191c7a0-0000-7000-8000-000000000000","type":"question","from":"owl:aaaaaaaaaaaaaaaa","to":"owl:bbbbbbbbbbbbbbbb","ts":"2026-09-01T10:00:00Z","in_reply_to":null,"body":{"project":"github.com/company/monorepo","path":"src/auth/session.rs","question":"Why is the refresh token rotated on every read?"}}"#
        );
        let back: Payload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, fixed_question());
    }

    #[test]
    fn golden_answer_json_matches_design_section_6() {
        let q = fixed_question();
        let mut a = Payload::answer(&q, "Because.", "claude", 2, true);
        a.id = ID.into();
        a.ts = TS.into();
        let json = String::from_utf8(a.to_signed_bytes()).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"id":"0191c7a0-0000-7000-8000-000000000000","type":"answer","from":"owl:bbbbbbbbbbbbbbbb","to":"owl:aaaaaaaaaaaaaaaa","ts":"2026-09-01T10:00:00Z","in_reply_to":"0191c7a0-0000-7000-8000-000000000000","body":{"answer":"Because.","harness":"claude","redactions":2,"cached":true}}"#
        );
        let back: Payload = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, Kind::Answer);
        assert_eq!(back, a);
    }

    #[test]
    fn question_constructor_fills_uuid_v7_and_timestamp() {
        let p = Payload::question("owl:a", "owl:b", "proj", Some("p.rs"), "why?");
        let u = uuid::Uuid::parse_str(&p.id).unwrap();
        assert_eq!(u.get_version_num(), 7);
        assert!(is_fresh(&p.ts, now_unix(), 5), "ts={}", p.ts);
        assert_eq!(p.v, 1);
        assert_eq!(p.kind, Kind::Question);
        assert_eq!(p.in_reply_to, None);
    }

    #[test]
    fn sign_then_verify() {
        let me = Identity::from_seed([1u8; 32]);
        let other = Identity::from_seed([2u8; 32]);
        let payload = fixed_question();
        let env = Envelope::sign(&payload, &me);
        assert_eq!(env.raw.as_bytes(), payload.to_signed_bytes().as_slice());
        assert!(env.sig.starts_with("ed25519:"));
        assert_eq!(env.verify(&me.verifying_key()).unwrap(), payload);
        assert!(
            env.verify(&other.verifying_key()).is_err(),
            "other key must reject"
        );
        let mut tampered = env.clone();
        tampered.raw.replace_range(2..3, "V"); // "v" -> "V": one character
        assert_ne!(tampered.raw, env.raw);
        assert!(
            tampered.verify(&me.verifying_key()).is_err(),
            "tampered raw must reject"
        );
        let mut bad_sig = env.clone();
        bad_sig.sig = "foo:AAAA".into();
        assert!(
            bad_sig.verify(&me.verifying_key()).is_err(),
            "bad sig prefix must reject"
        );
        // Valid signature over non-JSON bytes: verify passes, parse fails.
        let junk = Envelope {
            raw: "not json".into(),
            sig: identity::sig_string(&me.sign(b"not json")),
        };
        let err = junk.verify(&me.verifying_key()).err().unwrap().to_string();
        assert!(err.contains("parsing"), "{err}");
        // Verify BEFORE parse: non-JSON raw with a foreign signature fails on the signature,
        // never reaching the parser.
        let foreign = Envelope {
            raw: "not json".into(),
            sig: identity::sig_string(&other.sign(b"not json")),
        };
        let err = foreign
            .verify(&me.verifying_key())
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("signature"), "{err}");
        assert!(!err.contains("parsing"), "{err}");
        let bad = Envelope {
            raw: "not json".into(),
            sig: "ed25519:AAAA".into(),
        };
        let err = bad.verify(&me.verifying_key()).err().unwrap().to_string();
        assert!(err.contains("signature"), "{err}");
        assert!(!err.contains("parsing"), "{err}");
    }

    #[test]
    fn question_hash_normalises() {
        let a = question_hash("p", Some("f"), "  Why  X? ");
        let b = question_hash("p", Some("f"), "why x?");
        let c = question_hash("p", Some("f"), "why y?");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64);
        assert!(
            a.chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
        );
        assert_ne!(
            question_hash("p2", Some("f"), "why x?"),
            a,
            "project is part of the hash"
        );
        assert_ne!(
            question_hash("p", Some("f2"), "why x?"),
            a,
            "path is part of the hash"
        );
        assert_eq!(
            question_hash("p", Some("f"), "why\tx?\n"),
            a,
            "all whitespace collapses"
        );
        // Pinned: SHA-256("p\nf\nwhy x?")
        assert_eq!(
            a,
            "001767e04e6a17cd5059b7e8132c2b100099becb5f5e85cacef6cb69e73c5975"
        );
    }

    /// A repo-level question hashes as the empty path — stable, normalised the
    /// same way, and never equal to the same question about a file.
    #[test]
    fn question_hash_without_path_is_stable_and_distinct() {
        let none = question_hash("p", None, "why x?");
        assert_eq!(none, question_hash("p", None, "  Why \tX? \n"));
        assert_ne!(none, question_hash("p", Some("f"), "why x?"));
        assert_ne!(none, question_hash("p2", None, "why x?"));
        assert_eq!(
            none,
            question_hash("p", Some(""), "why x?"),
            "None and the empty path are the same key"
        );
        // Pinned: SHA-256("p\n\nwhy x?")
        assert_eq!(
            none,
            "a4ebf08f97de160963f99fec4925a88637f5bc246e2836b4d5754a86bbce1c08"
        );
    }

    /// A question without a path omits the key on the wire, a payload without the
    /// key parses as `None`, and an explicit `"path": null` parses the same way.
    #[test]
    fn question_without_path_omits_the_key_on_the_wire() {
        let mut q = fixed_question();
        q.body = Body::Question {
            project: "github.com/company/monorepo".into(),
            path: None,
            question: "How long is your README?".into(),
            context: None,
        };
        let json = String::from_utf8(q.to_signed_bytes()).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"id":"0191c7a0-0000-7000-8000-000000000000","type":"question","from":"owl:aaaaaaaaaaaaaaaa","to":"owl:bbbbbbbbbbbbbbbb","ts":"2026-09-01T10:00:00Z","in_reply_to":null,"body":{"project":"github.com/company/monorepo","question":"How long is your README?"}}"#
        );
        let back: Payload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, q);
        let with_null = json.replace(
            r#""project":"github.com/company/monorepo","#,
            r#""project":"github.com/company/monorepo","path":null,"#,
        );
        assert_ne!(with_null, json);
        let back: Payload = serde_json::from_str(&with_null).unwrap();
        assert_eq!(back, q);
        // The constructor threads `None` through unchanged.
        let p = Payload::question("owl:a", "owl:b", "proj", None, "why?");
        assert!(matches!(p.body, Body::Question { path: None, .. }), "{p:?}");
    }

    #[test]
    fn replay_window() {
        let now = TS_UNIX;
        assert!(!is_fresh(
            &unix_to_rfc3339(now - 301),
            now,
            REPLAY_WINDOW_SECS
        ));
        assert!(is_fresh(
            &unix_to_rfc3339(now - 299),
            now,
            REPLAY_WINDOW_SECS
        ));
        assert!(is_fresh(
            &unix_to_rfc3339(now - 300),
            now,
            REPLAY_WINDOW_SECS
        ));
        assert!(
            !is_fresh(&unix_to_rfc3339(now + 301), now, REPLAY_WINDOW_SECS),
            "future skew"
        );
        assert!(is_fresh(
            &unix_to_rfc3339(now + 299),
            now,
            REPLAY_WINDOW_SECS
        ));
        assert!(!is_fresh("garbage", now, REPLAY_WINDOW_SECS));
        assert!(
            !is_fresh("2026-09-01T10:00:00", now, REPLAY_WINDOW_SECS),
            "missing Z"
        );

        let home = tempfile::tempdir().unwrap();
        let mut seen = SeenIds::load(home.path(), now).unwrap();
        assert!(seen.ids.is_empty());
        assert!(seen.insert(ID, now));
        assert!(!seen.insert(ID, now), "second insert must be false");
        assert!(
            seen.insert("other", now - 601),
            "old entry inserted for pruning check"
        );
        assert!(seen.insert("edge", now - 600), "boundary entry");
        seen.save(home.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.path().join("seen-ids.txt")).unwrap(),
            format!("{ID} {now}\nother {}\nedge {}\n", now - 601, now - 600)
        );
        let mut loaded = SeenIds::load(home.path(), now).unwrap();
        assert_eq!(
            loaded.ids.len(),
            2,
            "now-601 pruned, now-600 kept: {:?}",
            loaded.ids
        );
        assert!(!loaded.insert(ID, now), "must survive save/load");
        assert!(
            !loaded.insert("edge", now),
            "entry at exactly 600 s is kept"
        );
        assert!(
            loaded.insert("other", now),
            "entries older than 600 s are pruned on load"
        );
        assert_eq!(loaded.ids.len(), 3);
    }

    #[test]
    fn rfc3339_roundtrip_and_rejects() {
        assert_eq!(parse_rfc3339_to_unix(TS), Some(TS_UNIX));
        assert_eq!(unix_to_rfc3339(TS_UNIX), TS);
        assert_eq!(parse_rfc3339_to_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_to_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            parse_rfc3339_to_unix("2000-03-01T00:00:00Z"),
            Some(951_868_800)
        );
        assert_eq!(unix_to_rfc3339(951_868_799), "2000-02-29T23:59:59Z");
        for bad in [
            "",
            "2026-09-01T10:00:00",
            "2026-09-01 10:00:00Z",
            "2026-13-01T10:00:00Z",
            "2026-09-01T24:00:00Z",
            "2026-09-01T10:00:00+02:00",
            "2026-09-01T10:00:00.000Z",
            "1969-12-31T23:59:59Z",
            "2026-09-01T-1:00:00Z",
            "+2026-09-01T10:00:00Z",
            "2026-9-01T10:00:00Z",
            "2026-09-01T10:00:0Z",
            "2026-09-01-05T10:00:00Z",
            "2026-09-01T10:00:00:00Z",
            "2026-09-01T10:00:00Zz",
        ] {
            assert_eq!(parse_rfc3339_to_unix(bad), None, "{bad:?}");
        }
        assert_eq!(rfc3339_now().len(), 20);
        assert!(rfc3339_now().ends_with('Z'));
    }
}
