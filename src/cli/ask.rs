//! `owl ask` (§9, architecture §3.1): peer resolution or `git blame` candidates, project
//! detection, asker-side cache, send, `asks/` record, `--wait` polling of the peer's outbox
//! through the daemon's ingestion path (`owlpost::pull`) and of the peer's Task
//! (`GET /v1/questions/{id}`), `--reply-to` (thread) and `--context` (snippet).

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::Args;
use owlpost::client::{self, Iroh, SendOutcome};
use owlpost::contacts::{Contact, ContactBook};
use owlpost::envelope::{self, Body, Envelope, MAX_CONTEXT_BYTES, Payload};
use owlpost::identity;
use owlpost::pull::{self, OpenAsk, Verdict};
use owlpost::render;
use owlpost::spool::{Dir, Record, Spool};
use serde::Serialize;
use serde_json::{Value, json};

use super::ExitError;

/// How often `--wait` polls the peer's outbox and Task (§3.5).
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Candidates shown by `--file`.
pub const MAX_CANDIDATES: usize = 3;

const USAGE: &str = "usage: owl ask <peer> [path] \"<question>\" | owl ask --file <path> [--peer <peer>] \"<question>\"";

#[derive(Debug, Args)]
pub struct AskArgs {
    /// `<peer> [path] "<question>"`; `--peer` and `--file` each replace one positional
    #[arg(value_names = ["PEER", "[PATH]", "QUESTION"])]
    pub args: Vec<String>,
    /// Project id (default: normalised `origin` remote of the current repo, else the directory name)
    #[arg(long, value_name = "ID")]
    pub project: Option<String>,
    /// After `accepted`, poll the peer's outbox for this many seconds (exit 4 on timeout)
    #[arg(long, value_name = "SECS")]
    pub wait: Option<u64>,
    /// Skip the asker-side cache
    #[arg(long)]
    pub no_cache: bool,
    /// Propose peers from `git blame` of this file; it is also the question's path
    #[arg(long, value_name = "PATH")]
    pub file: Option<String>,
    /// Peer to ask (fingerprint, e-mail, name prefix, or an `@owl:to://…` / `to://…` resource
    /// URI); with `--file` skips the pick
    #[arg(long, value_name = "PEER")]
    pub peer: Option<String>,
    /// Continue an earlier exchange with this peer: reuse its thread id (an ask, a finished
    /// question or a received answer)
    #[arg(long, value_name = "ID")]
    pub reply_to: Option<String>,
    /// Attach a snippet (a diff, an error, an excerpt; at most 8192 bytes) from this file, `-` for stdin
    #[arg(long, value_name = "FILE")]
    pub context: Option<String>,
}

/// The thread id of exchange `id` with `contact` (`--reply-to`): an `asks/` or
/// `done/` question we sent, an answer we received (`inbox/` or `done/`), or a question the
/// peer asked us (`inbox/` or `done/`, answered or rejected). Exit 1 for an
/// unknown id (`no exchange <id>`), an exchange with another peer (`<id> was asked to
/// <name>, not <peer>`) or one that carries no thread id.
pub fn thread_of(
    spool: &Spool,
    book: &ContactBook,
    id: &str,
    contact: &Contact,
) -> anyhow::Result<String> {
    let found = [Dir::Asks, Dir::Done, Dir::Inbox]
        .into_iter()
        .find_map(|d| spool.get(d, id).ok().flatten())
        .and_then(|rec| serde_json::from_str::<Payload>(&rec.raw).ok());
    let Some(payload) = found else {
        return Err(ExitError::error(1, format!("no exchange {id}")));
    };
    // A request of ours names the peer in `to`, a reply we received in `from` (a content request and its reply behave exactly like a question and its answer), and so
    // does a request the peer asked us.
    let peer = if payload.kind.is_request() {
        &payload.to
    } else {
        &payload.from
    };
    if *peer != contact.fingerprint && payload.from != contact.fingerprint {
        return Err(ExitError::error(
            1,
            format!(
                "{id} was asked to {}, not {}",
                render::peer_name(book, peer),
                contact.name
            ),
        ));
    }
    payload
        .context_id
        .ok_or_else(|| ExitError::error(1, format!("{id} carries no thread id")))
}

/// The `--context` snippet: the file (`-` = stdin), trimmed, at most `MAX_CONTEXT_BYTES`
/// bytes (exit 1 `context is <n> bytes, max 8192` beyond that).
pub fn read_context(source: &str) -> anyhow::Result<String> {
    let text = if source == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("reading the context from stdin")?;
        s
    } else {
        std::fs::read_to_string(source).with_context(|| format!("reading {source}"))?
    };
    match envelope::check_context(&text) {
        Ok(t) => Ok(t.to_string()),
        Err(n) => Err(ExitError::error(
            1,
            format!("context is {n} bytes, max {MAX_CONTEXT_BYTES}"),
        )),
    }
}

/// The three inputs after positionals and `--file`/`--peer` have been reconciled.
#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    /// `None` = pick from `git blame` candidates.
    pub peer: Option<String>,
    /// `None` = a question about the repository as a whole.
    pub path: Option<String>,
    pub question: String,
}

/// `--peer` covers PEER, `--file` covers PATH; the remaining positionals fill the rest in order.
/// Without `--file`, two remaining positionals are `<path> <question>`, one is just `<question>`.
pub fn parse_positionals(
    args: &[String],
    peer_flag: Option<&str>,
    file_flag: Option<&str>,
) -> anyhow::Result<Parsed> {
    let mut it = args.iter().cloned();
    let peer = match (peer_flag, file_flag) {
        (Some(p), _) => Some(p.to_string()),
        (None, Some(_)) => None,
        (None, None) => Some(
            it.next()
                .with_context(|| format!("missing <peer>\n{USAGE}"))?,
        ),
    };
    let mut rest: Vec<String> = it.collect();
    let path = match file_flag {
        Some(f) => Some(f.to_string()),
        None if rest.len() >= 2 => Some(rest.remove(0)),
        None => None,
    };
    let mut it = rest.into_iter();
    let question = it
        .next()
        .with_context(|| format!("missing <question>\n{USAGE}"))?;
    if let Some(extra) = it.next() {
        bail!("unexpected argument {extra:?}\n{USAGE}");
    }
    if question.trim().is_empty() {
        bail!("question is empty");
    }
    Ok(Parsed {
        peer,
        path,
        question,
    })
}

pub fn run(home: &Path, args: AskArgs, json: bool, quiet: bool) -> anyhow::Result<()> {
    let (_cfg, identity) = crate::require_identity(home)?;
    let cwd = std::env::current_dir().context("reading the current directory")?;
    let book = ContactBook::load(home, &cwd)?;
    let parsed = parse_positionals(&args.args, args.peer.as_deref(), args.file.as_deref())?;
    let contact: Contact = match &parsed.peer {
        Some(query) => book.resolve(query)?.clone(),
        None => {
            let file = args.file.as_deref().expect("peer is None only with --file");
            let stats = blame(&cwd, Path::new(file))?;
            let cands = candidates(&stats, &book.contacts);
            if json {
                println!("{}", serde_json::to_string_pretty(&cands)?);
                return Ok(());
            }
            let picked = pick(&cands, file)?;
            book.resolve(&picked.fingerprint)?.clone()
        }
    };
    let project = args.project.clone().unwrap_or_else(|| detect_project(&cwd));
    let spool = Spool::new(home)?;
    // A thread continues with `--reply-to`, else starts here; a snippet travels as
    // `body.context`. Either makes the question "threaded": it is answered for this thread
    // only, so neither cache serves or stores it.
    let context_id = match &args.reply_to {
        Some(rid) => thread_of(&spool, &book, rid, &contact)?,
        None => uuid::Uuid::now_v7().to_string(),
    };
    let context = args.context.as_deref().map(read_context).transpose()?;
    let threaded = args.reply_to.is_some() || context.is_some();
    let hash = envelope::question_hash(&project, parsed.path.as_deref(), &parsed.question);
    if !args.no_cache
        && !threaded
        && let Some(hit) = cache_lookup(&spool, &hash, quiet)
    {
        return print_answer(&hit, json);
    }
    let own = identity::fingerprint(&identity.verifying_key());
    let mut payload = Payload::question(
        &own,
        &contact.fingerprint,
        &project,
        parsed.path.as_deref(),
        &parsed.question,
    );
    payload.context_id = Some(context_id);
    if let Body::Question { context: slot, .. } = &mut payload.body {
        *slot = context;
    }
    let envelope = Envelope::sign(&payload, &identity);
    let meta = json!({ "peer": contact.fingerprint, "hash": hash, "threaded": threaded });
    // iroh goes through the local daemon (the CLI must not bind a second endpoint with the
    // identity key); without a daemon only the contact's endpoints are tried.
    let iroh = Iroh::from_home(home);
    let mut asked = record(&envelope, "waiting", meta.clone());
    // Record birth.
    owlpost::events::push(&mut asked, "asked", Some("human"), None);
    let reserved = Reserved::new(&spool, &payload.id, &asked)?;
    match client::send_question(&identity, &contact, &iroh, &envelope)? {
        SendOutcome::Answer(hit) => {
            let (answer, answer_env) = (hit.payload, hit.envelope);
            // The peer picked the answer's id; it must not replace or shadow a
            // record here — the reserved ask included, so it cannot be the question's own
            // id. A re-offer of the answer already in the inbox is not rewritten.
            match pull::answer_id(&spool, &answer_env, &answer)? {
                pull::AnswerId::Refused => {
                    bail!(
                        "answer {} collides with a record here; not stored",
                        answer.id
                    )
                }
                pull::AnswerId::Stored => {}
                pull::AnswerId::New => pull::store_answer(
                    &spool,
                    &answer_env,
                    &answer,
                    &contact.fingerprint,
                    &hash,
                    &payload.id,
                    !threaded,
                )?,
            }
            let mut done_meta = meta.clone();
            done_meta["answer"] = json!(answer.id);
            let mut asked = record(&envelope, "answered", done_meta);
            // Record birth: asked and answered in the same call.
            owlpost::events::push(&mut asked, "asked", Some("human"), None);
            owlpost::events::push(&mut asked, "answer-received", None, None);
            spool.put(Dir::Done, &payload.id, &asked)?;
            // The reservation in asks/ goes when `reserved` drops: the ask is done.
            print_answer(&answer, json)
        }
        SendOutcome::Accepted { id, state } => {
            reserved.keep();
            let text = state.as_deref().and_then(envelope::state_text);
            let accepted = match text {
                Some(t) => format!("accepted {id} — {t}"),
                None => format!("accepted {id}"),
            };
            match args.wait {
                None => {
                    if json {
                        println!(
                            "{}",
                            json!({ "status": "accepted", "id": id, "state": state })
                        );
                    } else {
                        println!("{accepted}");
                    }
                    Ok(())
                }
                Some(secs) => {
                    if !quiet {
                        eprintln!("{accepted}; waiting up to {secs}s for an answer");
                    }
                    let ask = OpenAsk {
                        id,
                        peer: contact.fingerprint.clone(),
                        hash,
                        path: parsed.path.clone().unwrap_or_else(|| "-".to_string()),
                        threaded,
                    };
                    wait_for_answer(&identity, &contact, &iroh, &spool, &ask, secs, json, quiet)
                }
            }
        }
        SendOutcome::Unavailable => Err(ExitError::error(
            2,
            format!(
                "unavailable: {} does not answer questions (policy never or responder disabled)",
                contact.name
            ),
        )),
        SendOutcome::RateLimited { retry_after_secs } => Err(ExitError::error(
            3,
            match retry_after_secs {
                Some(s) => format!("rate limited, retry after {s}s"),
                None => "rate limited".to_string(),
            },
        )),
        SendOutcome::Offline { errors } => Err(ExitError::error(
            2,
            format!(
                "offline: no endpoint of {} reachable ({})",
                contact.name,
                errors.join("; ")
            ),
        )),
    }
}

/// A readable cached answer; a corrupt cache file is a miss (warned), never a failed ask.
fn cache_lookup(spool: &Spool, hash: &str, quiet: bool) -> Option<Payload> {
    match spool.cache_get(hash) {
        Ok(Some(rec)) => match serde_json::from_str::<Payload>(&rec.raw) {
            Ok(p) if p.kind == envelope::Kind::Answer => Some(p),
            _ => {
                if !quiet {
                    eprintln!("owl: warning: ignoring corrupt cache entry {hash}");
                }
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            if !quiet {
                eprintln!("owl: warning: ignoring unreadable cache entry: {e:#}");
            }
            None
        }
    }
}

/// An ask written to `asks/` BEFORE it is sent. The peer knows the id from the
/// moment the request leaves, so a record written only after the reply would leave it a
/// window to post a question here under that id. Removed again on drop unless `keep` — a
/// send that ends without an ask to wait for records nothing.
pub(super) struct Reserved<'a> {
    spool: &'a Spool,
    id: String,
    keep: bool,
}

impl<'a> Reserved<'a> {
    pub(super) fn new(spool: &'a Spool, id: &str, rec: &Record) -> anyhow::Result<Self> {
        spool.put_new(Dir::Asks, id, rec)?;
        Ok(Reserved {
            spool,
            id: id.to_string(),
            keep: false,
        })
    }

    /// The peer accepted the question: the reservation is the open ask.
    pub(super) fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for Reserved<'_> {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(self.spool.path(Dir::Asks, &self.id));
        }
    }
}

fn record(env: &Envelope, state: &str, meta: Value) -> Record {
    Record {
        raw: env.raw.clone(),
        sig: env.sig.clone(),
        state: state.to_string(),
        seen: false,
        received_at: envelope::rfc3339_now(),
        draft: None,
        meta,
    }
}

fn print_answer(answer: &Payload, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(answer)?);
        return Ok(());
    }
    match &answer.body {
        Body::Answer { answer, .. } => println!("{answer}"),
        Body::Question { .. }
        | Body::Content { .. }
        | Body::ContentReply { .. }
        | Body::ToolCall { .. }
        | Body::ToolReply { .. } => {
            bail!("payload {} is not an answer", answer.id)
        }
    }
    Ok(())
}

/// Poll `GET /v1/outbox` every `POLL_INTERVAL` until an answer to `ask` shows up or `secs`
/// have passed (exit 4; the `asks/` record stays `waiting` for the daemon's pull loop). Each
/// envelope goes through the daemon's `pull::ingest_envelope`, so a forged or unrelated entry
/// is skipped and left unacked exactly as the pull loop would. On the same tick the peer's
/// Task is fetched: every change of its text prints `<HH:MM> <text>` to stderr
/// (nothing when quiet); `REJECTED` ends the wait at once — `declined by <name>: <text>`,
/// exit 2, the ask moved to `done/` as `declined`.
#[allow(clippy::too_many_arguments)]
fn wait_for_answer(
    identity: &identity::Identity,
    contact: &Contact,
    iroh: &Iroh,
    spool: &Spool,
    ask: &OpenAsk,
    secs: u64,
    json: bool,
    quiet: bool,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let open = BTreeMap::from([(ask.id.clone(), ask.clone())]);
    let mut warned = false;
    let mut last_text: Option<String> = None;
    loop {
        match client::fetch_outbox(identity, contact, iroh) {
            Ok(items) => {
                for env in &items {
                    match pull::ingest_envelope(identity, contact, iroh, spool, &open, env)? {
                        Verdict::Ingested(ing) => {
                            if let Some(e) = &ing.ack_error
                                && !quiet
                            {
                                eprintln!("owl: warning: ack of {} failed: {e:#}", ing.answer.id);
                            }
                            return print_answer(&ing.answer, json);
                        }
                        Verdict::Forged | Verdict::Unrelated | Verdict::Refused => {}
                    }
                }
            }
            Err(e) => {
                if !quiet && !warned {
                    eprintln!("owl: warning: polling {}: {e:#}", contact.name);
                    warned = true;
                }
            }
        }
        // A peer without the route (404) or unreachable this tick: nothing to report.
        if let Ok(Some(task)) = client::fetch_task(identity, contact, iroh, &ask.id) {
            pull::note_task_state(spool, &ask.id, &task);
            if task.rejected() {
                pull::close_declined(spool, &ask.id)?;
                return Err(ExitError::error(
                    2,
                    format!("declined by {}: {}", contact.name, task.text),
                ));
            }
            if last_text.as_deref() != Some(task.text.as_str()) {
                if !quiet {
                    eprintln!(
                        "{} {}",
                        render::local_hh_mm(&envelope::rfc3339_now()),
                        task.text
                    );
                }
                last_text = Some(task.text);
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(ExitError::error(
                4,
                format!(
                    "timeout: no answer within {secs}s; ask {} stays waiting for the daemon",
                    ask.id
                ),
            ));
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline - now));
    }
}

// ---- project detection -------------------------------------------------------------------
// ponytail: `git` is shelled out for `remote get-url` and `blame` — a git library would drop
// the PATH dependency but is not in the §1 crate list.

/// `origin` remote normalised to `host/org/repo`, else the directory name.
pub fn detect_project(cwd: &Path) -> String {
    if let Some(p) = git_origin(cwd).as_deref().and_then(normalise_remote) {
        return p;
    }
    cwd.file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map_or_else(|| "unknown".to_string(), str::to_string)
}

fn git_origin(cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!url.is_empty()).then_some(url)
}

/// `git@github.com:org/repo.git`, `ssh://git@host[:port]/org/repo`, `https://host/org/repo/`
/// → `host/org/repo` (host lowercased, user/port/`.git`/trailing `/` dropped). Anything
/// without a host and a path (local paths, `file://`) → `None`.
pub fn normalise_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = if let Some((_, rest)) = url.split_once("://") {
        rest.to_string()
    } else if let Some((user_host, path)) = url.split_once(':')
        && !user_host.contains('/')
        && user_host.len() > 1 // a single letter before the colon is a drive, not a host
        && !path.starts_with("//")
    {
        format!("{user_host}/{path}")
    } else {
        return None;
    };
    let (host, path) = rest.split_once('/')?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.split(':').next().unwrap_or_default().to_lowercase();
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{host}/{path}"))
}

// ---- git blame candidates ------------------------------------------------------------------

/// Line counts from `git blame --line-porcelain`: `total` counts every blamed line, whether or
/// not its group carried an `author-mail`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BlameStats {
    pub total: usize,
    pub by_email: BTreeMap<String, usize>,
}

/// Header = `<hex sha> <orig line> <final line> [<group size>]`; metadata `key value` lines
/// follow, then the content line prefixed by a tab.
fn is_header(line: &str) -> bool {
    let mut it = line.split(' ');
    let sha_ok = it
        .next()
        .is_some_and(|s| s.len() >= 40 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    let num =
        |s: Option<&str>| s.is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
    sha_ok && num(it.next()) && num(it.next())
}

pub fn parse_blame(porcelain: &str) -> BlameStats {
    let mut stats = BlameStats::default();
    for line in porcelain.lines() {
        if is_header(line) {
            stats.total += 1;
        } else if let Some(mail) = line.strip_prefix("author-mail ") {
            let mail = mail.trim().trim_start_matches('<').trim_end_matches('>');
            *stats.by_email.entry(mail.to_lowercase()).or_default() += 1;
        }
    }
    stats
}

/// Runs `git blame --line-porcelain` on `file` (relative to `cwd` unless absolute).
pub fn blame(cwd: &Path, file: &Path) -> anyhow::Result<BlameStats> {
    let full: PathBuf = if file.is_absolute() {
        file.to_path_buf()
    } else {
        cwd.join(file)
    };
    let dir = full
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| cwd.to_path_buf(), Path::to_path_buf);
    let name = full
        .file_name()
        .with_context(|| format!("{} has no file name", file.display()))?;
    let out = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["blame", "--line-porcelain", "--"])
        .arg(name)
        .output()
        .context("running git blame")?;
    if !out.status.success() {
        bail!(
            "git blame {} failed: {}",
            file.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_blame(&String::from_utf8_lossy(&out.stdout)))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub name: String,
    pub fingerprint: String,
    pub lines: usize,
    pub share: f64,
}

/// Contacts owning at least one blamed line (any of their emails, case-insensitive), top
/// `MAX_CANDIDATES` by line count (ties by name).
pub fn candidates(stats: &BlameStats, contacts: &[Contact]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = contacts
        .iter()
        .map(|c| {
            let lines = c
                .emails
                .iter()
                .filter_map(|e| stats.by_email.get(&e.to_lowercase()))
                .sum::<usize>();
            Candidate {
                name: c.name.clone(),
                fingerprint: c.fingerprint.clone(),
                lines,
                share: if stats.total == 0 {
                    0.0
                } else {
                    lines as f64 / stats.total as f64
                },
            }
        })
        .filter(|c| c.lines > 0)
        .collect();
    out.sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.name.cmp(&b.name)));
    out.truncate(MAX_CANDIDATES);
    out
}

fn format_candidates(cands: &[Candidate]) -> String {
    cands
        .iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                "{}) {:<20} {:<20} {} lines ({:.0}%)\n",
                i + 1,
                c.name,
                c.fingerprint,
                c.lines,
                c.share * 100.0
            )
        })
        .collect()
}

/// Interactive numbered pick on a terminal; otherwise list the candidates and exit 1.
fn pick<'a>(cands: &'a [Candidate], file: &str) -> anyhow::Result<&'a Candidate> {
    if cands.is_empty() {
        bail!("no contact matches the git blame authors of {file}");
    }
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        eprint!("{}", format_candidates(cands));
        return Err(ExitError::error(
            1,
            "stdin is not a terminal: pass --peer <peer> to choose, or --json to list",
        ));
    }
    print!("{}", format_candidates(cands));
    print!("ask which peer [1-{}]? ", cands.len());
    std::io::stdout().flush()?;
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let n: usize = line
        .trim()
        .parse()
        .with_context(|| format!("not a number: {:?}", line.trim()))?;
    cands
        .get(n.wrapping_sub(1))
        .with_context(|| format!("pick 1-{}", cands.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn positionals_follow_the_flags() {
        assert_eq!(
            parse_positionals(&s(&["bea", "src/x.rs", "why?"]), None, None).unwrap(),
            Parsed {
                peer: Some("bea".into()),
                path: Some("src/x.rs".into()),
                question: "why?".into()
            }
        );
        assert_eq!(
            parse_positionals(&s(&["why?"]), None, Some("src/x.rs")).unwrap(),
            Parsed {
                peer: None,
                path: Some("src/x.rs".into()),
                question: "why?".into()
            }
        );
        assert_eq!(
            parse_positionals(&s(&["why?"]), Some("bea"), Some("src/x.rs")).unwrap(),
            Parsed {
                peer: Some("bea".into()),
                path: Some("src/x.rs".into()),
                question: "why?".into()
            }
        );
        assert_eq!(
            parse_positionals(&s(&["src/x.rs", "why?"]), Some("bea"), None).unwrap(),
            Parsed {
                peer: Some("bea".into()),
                path: Some("src/x.rs".into()),
                question: "why?".into()
            }
        );
        let err = |args: &[&str], p: Option<&str>, f: Option<&str>| {
            parse_positionals(&s(args), p, f).unwrap_err().to_string()
        };
        assert!(err(&[], None, None).contains("missing <peer>"));
        assert!(err(&["bea"], None, None).contains("missing <question>"));
        // Two positionals after the peer are `<path> <question>`; one is a repo-level question.
        assert_eq!(
            parse_positionals(&s(&["bea", "why?"]), None, None).unwrap(),
            Parsed {
                peer: Some("bea".into()),
                path: None,
                question: "why?".into()
            }
        );
        assert_eq!(
            parse_positionals(&s(&["why?"]), Some("bea"), None).unwrap(),
            Parsed {
                peer: Some("bea".into()),
                path: None,
                question: "why?".into()
            }
        );
        assert!(err(&["bea", "  "], None, None).contains("question is empty"));
        assert!(err(&[], None, Some("f")).contains("missing <question>"));
        assert!(err(&["bea", "p", "q", "extra"], None, None).contains("unexpected argument"));
        assert!(err(&["q", "extra"], Some("bea"), Some("f")).contains("unexpected argument"));
        assert!(err(&["bea", "p", "  "], None, None).contains("question is empty"));
    }

    #[test]
    fn remote_normalisation() {
        for (url, want) in [
            ("git@github.com:org/repo.git", "github.com/org/repo"),
            ("git@github.com:org/repo", "github.com/org/repo"),
            ("https://github.com/org/repo.git", "github.com/org/repo"),
            ("https://github.com/org/repo", "github.com/org/repo"),
            ("https://github.com/org/repo/", "github.com/org/repo"),
            ("https://github.com/org/repo.git/", "github.com/org/repo"),
            ("ssh://git@github.com/org/repo.git", "github.com/org/repo"),
            (
                "ssh://git@github.com:2222/org/repo.git",
                "github.com/org/repo",
            ),
            (
                "https://user@GitLab.com/group/sub/repo.git",
                "gitlab.com/group/sub/repo",
            ),
            (
                "git@bitbucket.org:team/repo.git\n",
                "bitbucket.org/team/repo",
            ),
            ("git://example.org/x.git", "example.org/x"),
        ] {
            assert_eq!(normalise_remote(url).as_deref(), Some(want), "{url}");
        }
        for bad in [
            "",
            "/srv/git/repo.git",
            "file:///srv/git/repo.git",
            "https://github.com",
            "https://github.com/",
            "github.com",
            "git@github.com:",
            "https:///org/repo",
            "C:/repos/x",
        ] {
            assert_eq!(normalise_remote(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn detect_project_falls_back_to_dir_name() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("myproj");
        std::fs::create_dir(&proj).unwrap();
        assert_eq!(detect_project(&proj), "myproj");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&proj)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        assert_eq!(detect_project(&proj), "myproj", "repo without origin");
        git(&["remote", "add", "origin", "/srv/local.git"]);
        assert_eq!(detect_project(&proj), "myproj", "local-path origin");
        git(&["remote", "set-url", "origin", "git@github.com:org/repo.git"]);
        assert_eq!(detect_project(&proj), "github.com/org/repo");
        let nested = proj.join("sub");
        std::fs::create_dir(&nested).unwrap();
        assert_eq!(detect_project(&nested), "github.com/org/repo", "subdir");
    }

    const PORCELAIN: &str = "\
0123456789abcdef0123456789abcdef01234567 1 1 2
author Ana
author-mail <ana@example.org>
summary first
filename f.txt
\tline one
0123456789abcdef0123456789abcdef01234567 2 2
author Ana
author-mail <Ana@Example.org>
summary first
filename f.txt
\tline two
fedcba9876543210fedcba9876543210fedcba98 1 3 1
author Bea
author-mail <bea@example.org>
summary 0123456789abcdef0123456789abcdef01234567 1 3 looks like a header but is not
filename f.txt
\tline three
89abcdef0123456789abcdef0123456789abcdef 1 4 1
author Nobody
summary no mail line at all
filename f.txt
\tline four
";

    #[test]
    fn blame_parser_counts_headers_and_mails() {
        let stats = parse_blame(PORCELAIN);
        assert_eq!(
            stats.total, 4,
            "one per header, including the mail-less group"
        );
        assert_eq!(stats.by_email.len(), 2);
        assert_eq!(stats.by_email["ana@example.org"], 2, "case-folded");
        assert_eq!(stats.by_email["bea@example.org"], 1);
        assert_eq!(parse_blame(""), BlameStats::default());
        assert!(!is_header("author-mail <x@y>"));
        assert!(
            !is_header("0123456789abcdef0123456789abcdef0123456 1 1"),
            "39 hex"
        );
        assert!(!is_header("0123456789abcdef0123456789abcdef01234567 x 1"));
        assert!(!is_header("0123456789abcdef0123456789abcdef01234567 1"));
        assert!(is_header("0123456789abcdef0123456789abcdef01234567 1 1"));
    }

    fn contact(name: &str, emails: &[&str]) -> Contact {
        Contact {
            name: name.into(),
            emails: emails.iter().map(|e| e.to_string()).collect(),
            pubkey: String::new(),
            endpoints: vec![],
            source: "local".into(),
            policy: None,
            added_at: None,
            fingerprint: format!("owl:{}", name.to_lowercase()),
        }
    }

    #[test]
    fn candidates_are_matched_ranked_and_capped() {
        // 20 blamed lines but only 12 carry a known mail: the share denominator is the
        // header count, not the sum of matched (or mailed) lines. Zed owns as many lines as
        // Bea and more than Ana, so the expected order [Bea, Zed, Ana] is the line-count
        // order (ties by name) and neither the name order (Ana, Bea, Cat) nor the contact
        // order (Cat, Bea, Eve, Ana, Dan, Zed).
        let mut stats = BlameStats {
            total: 20,
            ..Default::default()
        };
        for (m, n) in [
            ("ana@example.org", 2),
            ("bea@example.org", 3),
            ("bea@work.example", 1),
            ("cat@example.org", 1),
            ("dan@example.org", 1),
            ("zed@example.org", 4),
        ] {
            stats.by_email.insert(m.into(), n);
        }
        let contacts = [
            contact("Cat", &["cat@example.org"]),
            contact("Bea", &["BEA@example.org", "bea@work.example"]),
            contact("Eve", &["eve@example.org"]),
            contact("Ana", &["ana@example.org"]),
            contact("Dan", &["dan@example.org"]),
            contact("Zed", &["zed@example.org"]),
        ];
        let got = candidates(&stats, &contacts);
        let names: Vec<&str> = got.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["Bea", "Zed", "Ana"],
            "Bea 4 (two emails, case-folded) ties Zed 4 and wins by name; Ana 2 beats Cat 1 and Dan 1; Eve absent; capped at 3"
        );
        assert_eq!(got[0].lines, 4);
        assert_eq!(got[0].fingerprint, "owl:bea");
        assert_eq!(got[1].lines, 4);
        assert_eq!(got[1].fingerprint, "owl:zed");
        assert_eq!(got[2].lines, 2);
        assert_eq!(got[0].share, 0.2, "4 of 20 blamed lines");
        assert_eq!(got[1].share, 0.2);
        assert_eq!(got[2].share, 0.1);
        assert!(candidates(&BlameStats::default(), &contacts).is_empty());
        let unmatched = [contact("Eve", &["eve@example.org"])];
        assert!(candidates(&stats, &unmatched).is_empty());
        // Cat 1 and Dan 1 tie and would be ranked by name were the cap wider: with the
        // top three removed, Cat comes before Dan.
        let tail = [
            contact("Dan", &["dan@example.org"]),
            contact("Cat", &["cat@example.org"]),
        ];
        let tail_names: Vec<String> = candidates(&stats, &tail)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(tail_names, ["Cat", "Dan"], "equal counts rank by name");
        let json = serde_json::to_value(&got).unwrap();
        assert_eq!(json[1]["name"], "Zed");
        assert_eq!(json[1]["lines"], 4);
        assert_eq!(json[1]["share"], 0.2);
    }

    #[test]
    fn pick_refuses_empty_list() {
        let err = pick(&[], "f.rs").unwrap_err().to_string();
        assert!(err.contains("no contact matches"), "{err}");
    }
}
