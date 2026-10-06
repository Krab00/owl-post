//! `owl history [--peer <name|fp>] [--path <glob>] [--since <RFC3339|Nd|Nh|Nm>]`: finished
//! exchanges, i.e. everything under `done/` (§8, §9).
//!
//! `peer` is the other party of each record (whichever of `from`/`to` is not this identity);
//! `--peer` matches its fingerprint exactly or its contact name case-insensitively. `--since`
//! compares the record's `received_at`. The glob knows `*` and `?` only.

use std::path::Path;

use anyhow::{Context, bail};
use owlpost::envelope::{self, Body};
use owlpost::identity::{self, Identity};
use owlpost::spool::{Dir, Spool};
use serde_json::{Value, json};

use super::{format_age, kind_str, payload_of, peer_name, print_json, print_table};

pub struct Filters {
    pub peer: Option<String>,
    pub path: Option<String>,
    pub since: Option<String>,
}

/// `*` matches any run (including empty), `?` exactly one character; everything else literal.
// ponytail: two metacharacters only — swap in the `glob` crate if character classes are needed.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, ti));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// `2026-09-01T10:00:00Z` → that instant; `3d` / `12h` / `30m` → `now` minus that much.
pub fn parse_since(s: &str, now: u64) -> anyhow::Result<u64> {
    if let Some(t) = envelope::parse_rfc3339_to_unix(s) {
        return Ok(t);
    }
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let mult = match unit {
        "d" => 86_400,
        "h" => 3_600,
        "m" => 60,
        _ => bail!("bad --since {s:?}: expected RFC3339 (YYYY-MM-DDTHH:MM:SSZ) or <N>d|<N>h|<N>m"),
    };
    let n: u64 = num
        .parse()
        .with_context(|| format!("bad --since {s:?}: expected RFC3339 or <N>d|<N>h|<N>m"))?;
    Ok(now.saturating_sub(n.saturating_mul(mult)))
}

pub fn run(home: &Path, filters: Filters, json: bool) -> anyhow::Result<()> {
    let spool = Spool::new(home)?;
    let book = super::contact_book(home)?;
    let now = envelope::now_unix();
    let since = filters
        .since
        .as_deref()
        .map(|s| parse_since(s, now))
        .transpose()?;
    let own = match Identity::load(home) {
        Ok(id) => Some(identity::fingerprint(&id.verifying_key())),
        Err(_) => None,
    };
    let mut rows: Vec<Value> = Vec::new();
    for (id, rec) in spool.list(Dir::Done, |_| true)? {
        let payload = payload_of(&id, &rec)?;
        let peer_fp = if own.as_deref() == Some(payload.from.as_str()) {
            payload.to.clone()
        } else {
            payload.from.clone()
        };
        let name = peer_name(&book, &peer_fp);
        if let Some(q) = &filters.peer
            && peer_fp != *q
            && !name.eq_ignore_ascii_case(q)
        {
            continue;
        }
        let (project, path, text) = match &payload.body {
            Body::Question {
                project,
                path,
                question,
                ..
            } => (
                Some(project.as_str()),
                path.as_deref().unwrap_or("-"),
                question.as_str(),
            ),
            Body::Answer { answer, .. } => (None, "-", answer.as_str()),
            // A content request lists its path, a reply the content it carried.
            Body::Content { project, path, .. } => {
                (project.as_deref(), path.as_deref().unwrap_or("-"), "")
            }
            Body::ContentReply { content, .. } => (None, "-", content.as_str()),
            // A tool call lists no path of ours, a reply the output it carried.
            Body::ToolCall { project, .. } => (project.as_deref(), "-", ""),
            Body::ToolReply { output, .. } => (None, "-", output.as_str()),
        };
        if let Some(g) = &filters.path
            && !glob_match(g, path)
        {
            continue;
        }
        let received = envelope::parse_rfc3339_to_unix(&rec.received_at);
        if let Some(s) = since
            && !received.is_some_and(|r| r >= s)
        {
            continue;
        }
        let age = received.map_or(0, |r| now.saturating_sub(r));
        rows.push(json!({
            "id": id,
            "peer": peer_fp,
            "peer_name": name,
            "from": payload.from,
            "to": payload.to,
            "type": kind_str(payload.kind),
            "state": rec.state,
            "project": project,
            "path": path,
            // The payload's thread id, `null` when the record is not threaded.
            "context_id": payload.context_id,
            "text": text,
            "received_at": rec.received_at,
            "age": format_age(age),
            "done_at": rec.meta.get("done_at").cloned().unwrap_or(Value::Null),
        }));
    }
    if json {
        print_json(&Value::Array(rows))?;
    } else {
        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|r| {
                ["id", "peer_name", "type", "state", "path", "received_at"]
                    .iter()
                    .map(|k| r[k].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .collect();
        print_table(&["ID", "PEER", "TYPE", "STATE", "PATH", "RECEIVED"], &cells);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_star_and_question_mark() {
        assert!(glob_match("src/auth/*.rs", "src/auth/session.rs"));
        assert!(glob_match("src/*", "src/auth/session.rs"));
        assert!(glob_match("*", ""));
        assert!(glob_match("*.rs", "a.rs"));
        assert!(glob_match("src/auth/session.r?", "src/auth/session.rs"));
        assert!(glob_match("src/**/session.rs", "src/auth/session.rs"));
        assert!(!glob_match("src/auth/session.r?", "src/auth/session.r"));
        assert!(!glob_match("src/auth/*.rs", "src/auth/session.ts"));
        assert!(!glob_match("src/auth/*.rs", "src/authz/session.rs"));
        assert!(!glob_match("src/auth/session.rs", "src/auth/session.rs2"));
        assert!(!glob_match("?", ""));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*b*c", "axxbyy"));
    }

    #[test]
    fn since_accepts_rfc3339_and_relative() {
        let now = 1_787_000_000;
        assert_eq!(
            parse_since("2026-09-01T10:00:00Z", now).unwrap(),
            envelope::parse_rfc3339_to_unix("2026-09-01T10:00:00Z").unwrap()
        );
        assert_eq!(parse_since("1d", now).unwrap(), now - 86_400);
        assert_eq!(parse_since("12h", now).unwrap(), now - 12 * 3_600);
        assert_eq!(parse_since("30m", now).unwrap(), now - 1_800);
        assert_eq!(parse_since("0d", now).unwrap(), now);
        for bad in ["", "d", "1w", "-1d", "1.5d", "yesterday", "2026-09-01"] {
            assert!(parse_since(bad, now).is_err(), "{bad:?} should be rejected");
        }
    }
}
