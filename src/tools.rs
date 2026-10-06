//! Tool-call requests (§3, §9): running the one tool a peer named.
//!
//! This module holds the **only** `Command` spawn of the tool path, and it is called from
//! exactly one place: `owl draft <id>` in `src/cli/draft.rs`. The daemon
//! (`src/server.rs`, `src/daemon.rs`, `src/auto.rs`, `src/pull.rs`) validates a tool-call
//! request and spools it for consent; it never starts a child for one, whatever policy the
//! peer has.
//!
//! The peer's input never touches `argv`. It goes to the child on stdin as the compact JSON
//! of the request's `input` object plus one `\n`, and stdin is then closed. The child's
//! environment is `OWLPOST_TOOL`, `OWLPOST_TOOL_NAME`, `OWLPOST_PEER` and the inherited
//! `PATH` — nothing else, so nothing of the owner's shell and nothing of the request reaches
//! it by another door.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde_json::{Map, Value};

use crate::config::{Config, Tool};
use crate::content::cut_at_char_boundary;
use crate::envelope::{Body, Kind, MAX_OUTPUT_BYTES, Payload};
use crate::runner;

/// How often the wait loop looks at the child (Every wait on a child is bounded).
const POLL: Duration = Duration::from_millis(50);

/// A tool-call request, already separated from the wire body. No `Eq`: `serde_json::Value`
/// has none (floats), so the input map only compares with `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub struct Request<'a> {
    pub tool: &'a str,
    pub input: &'a Map<String, Value>,
}

/// What `owl inbox` puts in its `summary` column and the timeline shows on a request row:
/// the tool name and the compact input. The 60-char cut is the listing's own
/// (`cli::first_line`), so every view cuts at the same width.
pub fn body_summary(body: &Body) -> String {
    match body {
        Body::ToolCall { tool, input, .. } => {
            format!(
                "{tool} {}",
                serde_json::to_string(input).unwrap_or_else(|_| "{}".into())
            )
        }
        _ => "tool-call".to_string(),
    }
}

/// The request a tool-call record carries; an error for any other kind of record. Keyed on
/// `payload.kind` first, for the reason `content::request_of` is.
pub fn request_of<'a>(id: &str, payload: &'a Payload) -> anyhow::Result<Request<'a>> {
    let Body::ToolCall { tool, input, .. } = &payload.body else {
        bail!("record {id} is not a tool-call request");
    };
    if payload.kind != Kind::ToolCall {
        bail!("record {id} is not a tool-call request");
    }
    Ok(Request { tool, input })
}

/// The registry entry for `name`; an error when the owner removed the tool between the
/// request's arrival and the human's `owl draft`.
pub fn lookup<'a>(config: &'a Config, name: &str) -> anyhow::Result<&'a Tool> {
    config
        .responder
        .tools
        .get(name)
        .with_context(|| format!("unknown tool {name}"))
}

/// The directory the tool runs in: the checkout its `cwd` names, or `$OWLPOST_HOME`.
pub fn cwd_of(config: &Config, home: &Path, tool: &Tool) -> anyhow::Result<PathBuf> {
    match &tool.cwd {
        None => Ok(home.to_path_buf()),
        Some(key) => config
            .projects
            .get(key)
            .map(PathBuf::from)
            .with_context(|| format!("cwd {key} is not a configured checkout")),
    }
}

/// What `owl draft` stores on a tool-call record and `owl send` signs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Redacted, cut to [`MAX_OUTPUT_BYTES`] when `truncated`.
    pub output: String,
    /// The child's code, or `-1` when it was killed by the timeout or by a signal.
    pub exit_code: i32,
    /// Wall time of the run; the timeout itself when the child was killed for exceeding it.
    pub duration_ms: u64,
    pub truncated: bool,
    pub redactions: u32,
    /// Byte length of the whole redacted output, before any cut.
    pub full_bytes: usize,
    /// The tool's name, so `owl show` and the timeline can name what ran.
    pub tool: String,
}

/// Runs `tool` with `input` on stdin and gives back the redacted, cut output.
///
/// A non-zero exit is **not** an error: a failing build is a legitimate answer and the
/// caller stores it as the draft. `Err` means the tool could not be started at all.
pub fn run(
    config: &Config,
    home: &Path,
    name: &str,
    tool: &Tool,
    input: &Map<String, Value>,
    peer: &str,
) -> anyhow::Result<Run> {
    let cwd = cwd_of(config, home, tool)?;
    let mut command = Command::new(&tool.argv[0]);
    command
        .args(&tool.argv[1..])
        .current_dir(&cwd)
        .env_clear()
        .env("OWLPOST_TOOL", "1")
        .env("OWLPOST_TOOL_NAME", name)
        .env("OWLPOST_PEER", peer)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    // The input as it goes to the child: compact JSON, one line, never an argument.
    let stdin_bytes = format!(
        "{}\n",
        serde_json::to_string(input).unwrap_or_else(|_| "{}".into())
    );
    let timeout = Duration::from_millis(tool.timeout_ms);
    let started = Instant::now();
    let child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!("tool {name}: no such file or directory")
        } else {
            anyhow::anyhow!("tool {name}: {e}")
        }
    })?;
    let outcome = wait_bounded(child, stdin_bytes.into_bytes(), timeout)?;
    let duration_ms = if outcome.timed_out {
        tool.timeout_ms
    } else {
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
    };
    // stdout first, then stderr under its own line — only when the tool wrote any.
    let mut combined = outcome.stdout;
    if !outcome.stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str("--- stderr ---\n");
        combined.push_str(&outcome.stderr);
    }
    let (redacted, redactions) = runner::redact(&config.responder.redact, &combined)?;
    let full_bytes = redacted.len();
    let truncated = full_bytes > MAX_OUTPUT_BYTES;
    let output = if truncated {
        cut_at_char_boundary(&redacted, MAX_OUTPUT_BYTES).to_string()
    } else {
        redacted
    };
    Ok(Run {
        output,
        exit_code: outcome.exit_code,
        duration_ms,
        truncated,
        redactions: u32::try_from(redactions).unwrap_or(u32::MAX),
        full_bytes,
        tool: name.to_string(),
    })
}

impl Run {
    /// The draft JSON a tool-call record carries: the `StoredDraft` keys every other command
    /// reads plus the ones `owl send` and `owl show` need.
    pub fn to_draft(&self) -> Value {
        serde_json::json!({
            "text": self.output,
            "harness": "human",
            "redactions": self.redactions,
            "status": "ok",
            "drafted_at": crate::envelope::rfc3339_now(),
            "tool": self.tool,
            "exit_code": self.exit_code,
            "duration_ms": self.duration_ms,
            "truncated": self.truncated,
            "full_bytes": self.full_bytes,
        })
    }

    /// The line `owl draft` prints for a tool-call record (§9), before the output itself.
    pub fn draft_line(&self) -> String {
        let mut line = format!(
            "ran {} in {:.1}s — exit {}, {} bytes, {} redaction{}",
            self.tool,
            self.duration_ms as f64 / 1000.0,
            self.exit_code,
            self.output.len(),
            self.redactions,
            if self.redactions == 1 { "" } else { "s" }
        );
        if self.truncated {
            line.push_str(&format!(
                ", truncated, {MAX_OUTPUT_BYTES} of {} bytes",
                self.full_bytes
            ));
        }
        line
    }

    /// The line `owl show` prints on the asker's side over the output block (§9).
    pub fn show_line(exit_code: i32, duration_ms: u64, bytes: usize) -> String {
        format!(
            "exit {exit_code} · {:.1}s · {bytes} bytes",
            duration_ms as f64 / 1000.0
        )
    }
}

/// The output as `owl show` prints it inside its ```` ```text ```` block: at most
/// [`crate::content::CONTENT_SHOW_LINES`] lines, then one line naming what was left out.
/// The same cap as a content reply's, so one number governs every long body.
pub fn display(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let cap = crate::content::CONTENT_SHOW_LINES;
    if lines.len() <= cap {
        return output.to_string();
    }
    format!(
        "{}\n… {} more lines — {} bytes",
        lines[..cap].join("\n"),
        lines.len() - cap,
        output.len()
    )
}

/// The run `owl draft` stored on a record, read back from `rec.draft`. `None` when the
/// record has no draft or the draft is not a tool one (it carries no `exit_code`).
pub fn stored(rec: &crate::spool::Record) -> Option<Run> {
    let v = rec.draft.as_ref()?;
    let output = v.get("text")?.as_str()?.to_string();
    let num = |k: &str| v.get(k).and_then(Value::as_u64);
    Some(Run {
        exit_code: i32::try_from(v.get("exit_code")?.as_i64()?).ok()?,
        duration_ms: num("duration_ms").unwrap_or(0),
        truncated: v.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        redactions: num("redactions")
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
        full_bytes: num("full_bytes")
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(output.len()),
        tool: v
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        output,
    })
}

struct Outcome {
    stdout: String,
    stderr: String,
    exit_code: i32,
    timed_out: bool,
}

/// Writes `stdin_bytes`, drains both pipes on their own threads and polls `try_wait` until
/// `timeout`; on expiry the child is killed and reaped. Every wait is bounded, and
/// the pipes are drained by the readers throughout, so a tool that writes more than one pipe
/// buffer cannot deadlock the poll loop.
///
/// ponytail: only the direct child is killed — a grandchild it forked is orphaned; the
/// reader and writer threads are detached, so an orphan holding a pipe cannot block us.
fn wait_bounded(
    mut child: Child,
    stdin_bytes: Vec<u8>,
    timeout: Duration,
) -> anyhow::Result<Outcome> {
    if let Some(mut pipe) = child.stdin.take() {
        // On its own thread: a child that never reads its stdin would otherwise block the
        // write once the input passes the pipe buffer.
        std::thread::spawn(move || {
            let _ = pipe.write_all(&stdin_bytes);
            let _ = pipe.flush();
        });
    }
    let stdout = capture(child.stdout.take());
    let stderr = capture(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let (status, timed_out) = loop {
        if let Some(s) = child.try_wait()? {
            break (s, false);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let s = child.wait()?;
            break (s, true);
        }
        std::thread::sleep(POLL);
    };
    if !timed_out {
        let _ = stdout.1.join();
        let _ = stderr.1.join();
    }
    let take = |buf: &Arc<Mutex<Vec<u8>>>| {
        String::from_utf8_lossy(&buf.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
    };
    Ok(Outcome {
        stdout: take(&stdout.0),
        stderr: take(&stderr.0),
        // A child killed by the timeout or by any signal has no code of its own: `-1` is
        // what goes back to the peer, and the duration then names the timeout.
        exit_code: status.code().unwrap_or(-1),
        timed_out,
    })
}

type Captured = (Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>);

fn capture<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> Captured {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&buf);
    let handle = std::thread::spawn(move || {
        let Some(mut pipe) = pipe else { return };
        let mut chunk = [0u8; 4096];
        while let Ok(n) = pipe.read(&mut chunk) {
            if n == 0 {
                break;
            }
            sink.lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(&chunk[..n]);
        }
    });
    (buf, handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(tool: &str, input: Value) -> Payload {
        Payload::tool_call(
            "owl:a",
            "owl:b",
            tool,
            input.as_object().unwrap().clone(),
            None,
        )
    }

    /// The summary is the tool name and the compact input; the cut to 60 chars is the
    /// listing's, so the summary itself stays whole here.
    #[test]
    fn summary_names_the_tool_and_its_compact_input() {
        let p = call("test", serde_json::json!({ "package": "auth" }));
        assert_eq!(body_summary(&p.body), r#"test {"package":"auth"}"#);
        // Not a tool-call body at all: the fallback, never a panic.
        let q = Payload::question("a", "b", "p", None, "why?");
        assert_eq!(body_summary(&q.body), "tool-call");
    }

    /// A question payload is never read as a tool call, whatever its body looks like.
    #[test]
    fn request_of_refuses_a_non_tool_kind() {
        let q = Payload::question("a", "b", "p", None, "why?");
        assert!(request_of("q1", &q).is_err());
        let mut p = call("test", serde_json::json!({}));
        assert!(request_of("t1", &p).is_ok());
        p.kind = Kind::Question;
        assert!(
            request_of("t1", &p).is_err(),
            "the tag decides, not the body"
        );
    }

    /// The printed line names the tool, the duration, the exit code, the bytes and the
    /// redaction count — singular for one, and the truncation when the cut bit.
    #[test]
    fn draft_line_reports_the_run() {
        let run = |redactions: u32, truncated: bool| Run {
            output: "x".repeat(3120),
            exit_code: 0,
            duration_ms: 4120,
            truncated,
            redactions,
            full_bytes: if truncated { 999_999 } else { 3120 },
            tool: "test".into(),
        };
        assert_eq!(
            run(1, false).draft_line(),
            "ran test in 4.1s — exit 0, 3120 bytes, 1 redaction"
        );
        assert_eq!(
            run(2, false).draft_line(),
            "ran test in 4.1s — exit 0, 3120 bytes, 2 redactions"
        );
        assert_eq!(
            run(0, false).draft_line(),
            "ran test in 4.1s — exit 0, 3120 bytes, 0 redactions"
        );
        assert!(
            run(1, true)
                .draft_line()
                .ends_with(", truncated, 262144 of 999999 bytes"),
            "{}",
            run(1, true).draft_line()
        );
    }

    /// `stored` reads back exactly what `to_draft` wrote, and refuses a draft that is not a
    /// tool one (a content draft carries no `exit_code`).
    #[test]
    fn to_draft_round_trips_through_stored() {
        let run = Run {
            output: "hi\n".into(),
            exit_code: 3,
            duration_ms: 12,
            truncated: false,
            redactions: 1,
            full_bytes: 3,
            tool: "test".into(),
        };
        let rec = crate::spool::Record {
            raw: String::new(),
            sig: String::new(),
            state: "drafted".into(),
            seen: false,
            received_at: "2026-09-01T10:00:00Z".into(),
            draft: Some(run.to_draft()),
            meta: Value::Null,
        };
        assert_eq!(stored(&rec).unwrap(), run);
        let plain = crate::spool::Record {
            draft: Some(serde_json::json!({ "text": "hi", "harness": "human" })),
            ..rec
        };
        assert!(stored(&plain).is_none(), "no exit_code, not a tool draft");
    }
}
