//! Content requests (§6, §9): reading the one file a peer asked for.
//!
//! Nothing here runs at arrival — the daemon only validates and spools the request
//! (`server::validate_request`). This module is what `owl draft <id>` calls: it resolves the
//! ref in the project's checkout (or the key in the memory store), reads the object, refuses
//! anything that is not UTF-8 text, runs the responder's redaction patterns over it and cuts
//! it at [`MAX_CONTENT_BYTES`] on a character boundary.
//!
//! No harness and no model are involved: the human is the only thing between the request and
//! the bytes leaving the machine.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::envelope::{Body, Kind, MAX_CONTENT_BYTES, Payload};
use crate::runner;

/// The two shapes a content request takes, already separated from the wire body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request<'a> {
    /// A file at a ref of a checkout named in `config.projects`.
    Path {
        project: &'a str,
        path: &'a str,
        /// `None` = the checkout's current branch (`HEAD`).
        git_ref: Option<&'a str>,
    },
    /// One entry of the configured memory store.
    Memory { key: &'a str },
}

/// What `owl inbox` puts in its `summary` column, what the timeline shows on a request row
/// and what the mod prints next to the Draft button: `<path>@<ref>` for a file,
/// `memory:<key>` for a memory entry. Takes the raw body, so every listing can use it without
/// first turning the body into a [`Request`].
pub fn body_summary(body: &Body) -> String {
    match body {
        Body::Content {
            path: Some(path),
            git_ref,
            ..
        } => format!("{path}@{}", git_ref.as_deref().unwrap_or("HEAD")),
        Body::Content {
            memory: Some(key), ..
        } => format!("memory:{key}"),
        _ => "content".to_string(),
    }
}

/// The request a content record carries; an error for any other kind of record.
///
/// Keyed on `payload.kind` first, never on the body alone: `Body` is untagged and a
/// `Body::Content` has only optional fields, so a malformed question body would otherwise
/// parse as a content request.
pub fn request_of<'a>(id: &str, payload: &'a Payload) -> anyhow::Result<Request<'a>> {
    let Body::Content {
        project,
        git_ref,
        path,
        memory,
    } = &payload.body
    else {
        bail!("record {id} is not a content request");
    };
    if payload.kind != Kind::Content {
        bail!("record {id} is not a content request");
    }
    match (path.as_deref(), memory.as_deref()) {
        (Some(path), None) => Ok(Request::Path {
            project: project.as_deref().unwrap_or_default(),
            path,
            git_ref: git_ref.as_deref(),
        }),
        (None, Some(key)) => Ok(Request::Memory { key }),
        _ => bail!("record {id} carries neither a path nor a memory key"),
    }
}

/// What `owl draft` stores on a content record and `owl send` signs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content {
    /// Redacted, cut to [`MAX_CONTENT_BYTES`] when `truncated`.
    pub text: String,
    /// Digest of the **whole** redacted content, before any cut — so an asker holding a
    /// prefix can tell that it is one.
    pub sha256: String,
    /// The commit the ref resolved to; `None` for a memory entry.
    pub ref_resolved: Option<String>,
    pub truncated: bool,
    pub redactions: u32,
    /// Byte length of the whole redacted content (equal to `text.len()` when not truncated).
    pub full_bytes: usize,
}

/// Hex of a SHA-256 digest.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Reads what `request` asks for, redacts it and cuts it. `Err` is always a user error
/// (`exit 1`): an unknown project, an unresolvable ref, a missing path, a non-text file, or a
/// memory key that leaves the store.
pub fn resolve(config: &Config, request: &Request<'_>) -> anyhow::Result<Content> {
    let (bytes, ref_resolved, label) = match request {
        Request::Path {
            project,
            path,
            git_ref,
        } => {
            let checkout = config
                .projects
                .get(*project)
                .with_context(|| format!("project {project} is not a configured checkout"))?;
            let git_ref = git_ref.unwrap_or("HEAD");
            let commit = rev_parse(Path::new(checkout), git_ref)?;
            let bytes = git_blob(Path::new(checkout), &commit, path, git_ref)?;
            (bytes, Some(commit), (*path).to_string())
        }
        Request::Memory { key } => {
            let bytes = read_memory(config, key)?;
            (bytes, None, format!("memory:{key}"))
        }
    };
    let text = match String::from_utf8(bytes) {
        // A `NUL` byte is valid UTF-8 and is exactly what a binary file smuggled through a
        // UTF-8 check looks like, so it is refused on its own.
        Ok(t) if !t.contains('\0') => t,
        _ => bail!("{label} is not text"),
    };
    let (redacted, redactions) = runner::redact(&config.responder.redact, &text)?;
    let sha256 = hex(&Sha256::digest(redacted.as_bytes()));
    let full_bytes = redacted.len();
    let truncated = full_bytes > MAX_CONTENT_BYTES;
    let text = if truncated {
        cut_at_char_boundary(&redacted, MAX_CONTENT_BYTES).to_string()
    } else {
        redacted
    };
    Ok(Content {
        text,
        sha256,
        ref_resolved,
        truncated,
        redactions: u32::try_from(redactions).unwrap_or(u32::MAX),
        full_bytes,
    })
}

/// Display cap of `owl show` for content (§9): beyond this many lines it prints the first
/// [`CONTENT_SHOW_LINES`] and one line naming what it left out.
pub const CONTENT_SHOW_LINES: usize = 200;

/// The content as `owl show` prints it inside its ```` ```text ```` block: at most
/// [`CONTENT_SHOW_LINES`] lines, then `… <n> more lines — <bytes> bytes, sha256 <hex>`.
pub fn display(content: &str, sha256: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= CONTENT_SHOW_LINES {
        return content.to_string();
    }
    let more = lines.len() - CONTENT_SHOW_LINES;
    format!(
        "{}\n… {more} more lines — {} bytes, sha256 {sha256}",
        lines[..CONTENT_SHOW_LINES].join("\n"),
        content.len()
    )
}

/// The digest line `owl show` prints under a received content reply. `false` means the
/// content in the record is not what its `sha256` says, and `owl show` exits 1.
///
/// A truncated reply is a third case: its content is a deliberate prefix, so its digest
/// cannot match and the line says what was received instead of crying tamper.
pub fn verify_line(content: &str, sha256: &str, truncated: bool) -> (String, bool) {
    if truncated {
        return (
            format!(
                "truncated — {} bytes received, sha256 {sha256} is of the full content",
                content.len()
            ),
            true,
        );
    }
    if hex(&Sha256::digest(content.as_bytes())) == sha256 {
        (format!("sha256 {sha256} — verified"), true)
    } else {
        (
            "sha256 mismatch — the content does not match its digest".to_string(),
            false,
        )
    }
}

/// The content `owl draft` stored on a record, read back from `rec.draft`. `None` when the
/// record has no draft or the draft is not a content one (it carries no `sha256`).
pub fn stored(rec: &crate::spool::Record) -> Option<Content> {
    let v = rec.draft.as_ref()?;
    let text = v.get("text")?.as_str()?.to_string();
    Some(Content {
        full_bytes: v
            .get("full_bytes")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(text.len()),
        text,
        sha256: v.get("sha256")?.as_str()?.to_string(),
        ref_resolved: v
            .get("ref_resolved")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        truncated: v
            .get("truncated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        redactions: v
            .get("redactions")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
    })
}

impl Content {
    /// The draft JSON a content record carries: the `StoredDraft` keys every other command
    /// reads (`text`, `harness`, `redactions`, `status`, `drafted_at`) plus the four content
    /// ones `owl send` and `owl show` need.
    pub fn to_draft(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "text": self.text,
            "harness": "human",
            "redactions": self.redactions,
            "status": "ok",
            "drafted_at": crate::envelope::rfc3339_now(),
            "sha256": self.sha256,
            "truncated": self.truncated,
            "full_bytes": self.full_bytes,
        });
        if let Some(r) = &self.ref_resolved {
            v["ref_resolved"] = serde_json::json!(r);
        }
        v
    }

    /// The line `owl draft` prints for a content record (§9).
    pub fn draft_line(&self, label: &str) -> String {
        let at = match &self.ref_resolved {
            Some(commit) => format!("{label}@{}", &commit[..commit.len().min(7)]),
            None => label.to_string(),
        };
        let mut line = format!(
            "content: {at} ({} bytes, {} redactions)",
            self.text.len(),
            self.redactions
        );
        if self.truncated {
            line.push_str(&format!(
                ", truncated, {MAX_CONTENT_BYTES} of {} bytes",
                self.full_bytes
            ));
        }
        line
    }
}

/// The longest prefix of `text` that is at most `max` bytes and ends on a character boundary.
/// A plain `&text[..max]` would panic (or, worse, split a `ł`) on a multi-byte file.
pub fn cut_at_char_boundary(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// `git rev-parse --verify --end-of-options <ref>^{commit}` in `checkout`. A ref that is
/// empty or starts with `-` is refused here, before `git` ever sees it: the server's
/// charset check lets `-h` and `--git-dir/x` through, and `--end-of-options` alone cannot be
/// relied on to catch them (the appended `^{commit}` already makes the argv one token).
fn rev_parse(checkout: &Path, git_ref: &str) -> anyhow::Result<String> {
    if git_ref.is_empty() || git_ref.starts_with('-') {
        bail!("malformed ref {git_ref}");
    }
    let out = git(
        checkout,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{git_ref}^{{commit}}"),
        ],
    )?;
    if !out.status.success() {
        bail!("unknown ref {git_ref}");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The blob at `<commit>:<path>`. `cat-file blob`, never `git show`: `git show` on a tree
/// prints a directory listing, which would turn the allowlist into a file browser, while
/// `cat-file blob` refuses anything that is not a blob itself.
fn git_blob(checkout: &Path, commit: &str, path: &str, git_ref: &str) -> anyhow::Result<Vec<u8>> {
    let spec = format!("{commit}:{path}");
    let out = git(checkout, &["cat-file", "blob", "--end-of-options", &spec])?;
    if !out.status.success() {
        bail!("unknown path {path} at {git_ref}");
    }
    Ok(out.stdout)
}

/// One `git` child in `checkout`, with the ambient git configuration kept out of the way so
/// the result does not depend on the owner's global config.
fn git(checkout: &Path, args: &[&str]) -> anyhow::Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .with_context(|| format!("running git in {}", checkout.display()))
}

/// `<memory_root>/<key>`, refused when the resolved path leaves the store (a symlink out of
/// it) — the shape check in `validate_request` only sees the key's text.
fn read_memory(config: &Config, key: &str) -> anyhow::Result<Vec<u8>> {
    let root = config
        .responder
        .memory_root
        .as_deref()
        .context("memory store not configured")?;
    let root = std::fs::canonicalize(root).context("memory store not configured")?;
    let target: PathBuf = root.join(key);
    let resolved =
        std::fs::canonicalize(&target).with_context(|| format!("unknown memory entry {key}"))?;
    if !resolved.starts_with(&root) {
        bail!("{key} is outside the memory store");
    }
    std::fs::read(&resolved).with_context(|| format!("reading {}", resolved.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `ref` a peer controls never reaches `git` as a flag: `-h`, `--help` and the empty
    /// ref are refused by name before the child runs, so git never prints its usage text.
    /// The `^{commit}` suffix alone is not the guard — it makes `-h^{commit}` one token, so
    /// dropping the leading-`-` refusal would silently depend on that accident.
    #[test]
    fn a_ref_that_looks_like_a_flag_is_refused_before_git_runs() {
        let dir = tempfile::tempdir().unwrap();
        let g = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("HOME", dir.path())
                .env("GIT_AUTHOR_NAME", "T")
                .env("GIT_AUTHOR_EMAIL", "t@example.org")
                .env("GIT_COMMITTER_NAME", "T")
                .env("GIT_COMMITTER_EMAIL", "t@example.org")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
            out
        };
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("f.txt"), "hi\n").unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-qm", "first"]);

        for bad in ["-h", "--help", "--git-dir/x", "--", ""] {
            let err = rev_parse(dir.path(), bad).unwrap_err().to_string();
            assert!(err.contains("malformed ref"), "{bad}: {err}");
            assert!(
                !err.to_lowercase().contains("usage"),
                "{bad} printed git help: {err}"
            );
        }
        // The twins: a well-formed ref still resolves, an unknown one still fails by name.
        let sha = rev_parse(dir.path(), "main").unwrap();
        assert_eq!(sha.len(), 40, "resolved sha: {sha}");
        let err = rev_parse(dir.path(), "nope").unwrap_err().to_string();
        assert!(err.contains("unknown ref nope"), "{err}");
    }

    /// The cut lands on a character boundary, never inside a `ł`, and never grows the text.
    #[test]
    fn cut_never_splits_a_character() {
        let text = format!("a{}", "ł".repeat(10));
        // 1 ASCII byte + 2-byte chars: every odd index above 1 is inside a character.
        for max in 0..text.len() + 3 {
            let cut = cut_at_char_boundary(&text, max);
            assert!(cut.len() <= max || text.len() <= max, "cut {max} grew");
            assert!(text.starts_with(cut), "cut {max} is not a prefix");
        }
        assert_eq!(cut_at_char_boundary(&text, 2), "a");
        assert_eq!(cut_at_char_boundary(&text, 3), "ał");
        assert_eq!(cut_at_char_boundary(&text, 100), text);
    }

    /// `body_summary` is what the inbox row, the timeline and the mod show.
    #[test]
    fn summary_names_the_path_with_its_ref_or_the_memory_key() {
        let body =
            |path: Option<&str>, git_ref: Option<&str>, memory: Option<&str>| Body::Content {
                project: Some("p".into()),
                git_ref: git_ref.map(str::to_string),
                path: path.map(str::to_string),
                memory: memory.map(str::to_string),
            };
        assert_eq!(
            body_summary(&body(Some("src/a.rs"), Some("main"), None)),
            "src/a.rs@main"
        );
        assert_eq!(
            body_summary(&body(Some("src/a.rs"), None, None)),
            "src/a.rs@HEAD",
            "no ref reads as HEAD"
        );
        assert_eq!(
            body_summary(&body(None, None, Some("notes/a.md"))),
            "memory:notes/a.md"
        );
        assert_eq!(body_summary(&body(None, None, None)), "content");
        // Not a content body at all: the fallback, never a panic.
        let q = Payload::question("a", "b", "p", None, "why?");
        assert_eq!(body_summary(&q.body), "content");
    }

    /// A question payload is never read as a content request, whatever its body looks like.
    #[test]
    fn request_of_refuses_a_non_content_kind() {
        let q = Payload::question("a", "b", "p", Some("src/a.rs"), "why?");
        assert!(request_of("q1", &q).is_err());
        let mut both = Payload::content("a", "b", Some("p"), None, Some("src/a.rs"), Some("k"));
        assert!(request_of("c1", &both).is_err(), "both path and memory");
        both.body = Body::Content {
            project: None,
            git_ref: None,
            path: None,
            memory: None,
        };
        assert!(request_of("c1", &both).is_err(), "neither");
    }
}
