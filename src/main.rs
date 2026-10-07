//! clap entry; dispatch to subcommands.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod cli;

use owlpost::{config, contacts, daemon, identity};

#[derive(Parser)]
#[command(
    name = "owl",
    version,
    about = "owlpost — ask your peers' agents about their code"
)]
struct Cli {
    /// Owlpost home directory (overrides $OWLPOST_HOME)
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,
    /// Machine-readable JSON output
    #[arg(long, global = true)]
    json: bool,
    /// Suppress non-essential output
    #[arg(short, long, global = true)]
    quiet: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

// ponytail: each subcommand declares only the args it needs.
#[derive(Subcommand)]
enum Cmd {
    /// Create home, key, config; print fingerprint
    Init {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        email: Vec<String>,
    },
    /// Identity summary
    Whoami,
    /// Print own card, or fetch and print a peer's card
    Card { peer: Option<String> },
    /// Per-peer presence plus the daemon's pull summary (from daemon.status)
    Presence,
    /// Probe one peer now (exit 0 online, 2 offline)
    Ping { peer: String },
    /// Contact book: list | export | show <peer> | remove <peer>
    Contact {
        #[command(subcommand)]
        cmd: ContactCmd,
    },
    /// Add a peer file to the global contacts (or the repo's .agents/peers/ with --local)
    Add {
        /// Path to the peer file, the JSON itself, or `-` for stdin
        source: String,
        /// Write into <git root>/.agents/peers/ instead of $OWLPOST_HOME/contacts/
        #[arg(long)]
        local: bool,
    },
    /// Release held questions from a peer: policy manual (default), --once (no policy),
    /// --always (auto; a hand-added contact needs --i-verified-the-fingerprint)
    Allow {
        peer: String,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        always: bool,
        #[arg(long)]
        i_verified_the_fingerprint: bool,
    },
    /// Policy never: held questions are denied, new ones get 403 unavailable
    Deny { peer: String },
    /// Send a question to a peer
    Ask(cli::ask::AskArgs),
    /// Ask a peer for one file at a ref, or for one entry of their memory store
    Request(cli::request::RequestArgs),
    /// Ask a peer to run one tool of their registry with a JSON input
    Call(cli::call::CallArgs),
    /// Where every open question stands (the peer's task state), or one by id
    Status { id: Option<String> },
    /// List or count inbox records
    Inbox {
        #[arg(long)]
        count: bool,
        #[arg(long)]
        new: bool,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        format: Option<String>,
        /// Poll the count every 5 s (OWLPOST_FOLLOW_SECS) and print the counter sentence when it changes
        #[arg(long, requires = "count", conflicts_with = "format")]
        follow: bool,
        /// Session id of the `--follow` marker $OWLPOST_HOME/watch/<ID>
        #[arg(long, requires = "follow", value_name = "ID", value_parser = cli::inbox::parse_session_id)]
        session: Option<String>,
        // ponytail: accepted and ignored so a not-yet-reinstalled older plugin (hooks.json
        // still passing `--session-start`) keeps working with this binary; drop it once every
        // machine has run `owl update`.
        #[arg(long, hide = true)]
        session_start: bool,
        /// Claude Code hook event name echoed in the `--format claude` line (hookEventName)
        #[arg(long, default_value = "UserPromptSubmit")]
        hook_event: String,
    },
    /// Full content of a record; marks it seen
    Show {
        id: String,
        /// claude|codex|kimi: the Markdown message table (plain = today's output)
        #[arg(long)]
        format: Option<String>,
    },
    /// Run the responder and store a draft
    Draft {
        id: String,
        #[arg(long)]
        harness: Option<String>,
        /// Store this text as the draft instead of running a harness (harness `human`)
        #[arg(long, conflicts_with = "harness")]
        text: Option<String>,
        /// With --text: the text is an in-session agent's answer (harness `agent`, redacted)
        #[arg(long, requires = "text")]
        agent: bool,
        /// Print the responder prompt for the record and stop (nothing runs, nothing changes)
        #[arg(long, conflicts_with_all = ["harness", "text", "agent"])]
        prompt: bool,
    },
    /// Open the draft in $EDITOR
    Edit { id: String },
    /// Sign and move to outbox
    Send { id: String },
    /// Discard a record
    Reject { id: String },
    /// Route an inbox record to one live Claude Code session (what the daemon does on arrival)
    Route { id: String },
    /// The whole conversation with one person (no peer: one row per person)
    Thread {
        peer: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        context: Option<String>,
        /// Only archived chats (list) or archived threads of the peer (timeline)
        #[arg(long)]
        archived: bool,
    },
    /// Hide a chat from `owl thread` (one thread of it with --context)
    Archive {
        peer: String,
        #[arg(long)]
        context: Option<String>,
    },
    /// Bring an archived chat (or thread) back to `owl thread`
    Unarchive {
        peer: String,
        #[arg(long)]
        context: Option<String>,
    },
    /// Move a chat (or one thread of it with --context) to the trash; `owl undo` restores it
    Delete {
        peer: String,
        #[arg(long)]
        context: Option<String>,
    },
    /// Restore the newest trash batch
    Undo,
    /// Finished exchanges
    History {
        #[arg(long)]
        peer: Option<String>,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        since: Option<String>,
    },
    /// Block until a matching inbox record arrives
    Watch {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Run the listener and loops
    Daemon {
        /// Stay attached to the terminal (currently the only mode; `owl install` supervises)
        #[arg(long)]
        foreground: bool,
    },
    /// Install the launchd/systemd service
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Uninstall the service
    Uninstall,
    /// Stop the installed service (the unit stays; `owl start` brings it back)
    Stop,
    /// Start the installed service again
    Start,
    /// Check key, config, endpoints, harnesses, daemon
    Doctor,
    /// Harness table of config.json: list | scan | add | edit | remove | use
    Harness {
        #[command(subcommand)]
        cmd: HarnessCmd,
    },
    /// Projects peers can ask about (config.json projects): add | list | remove
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// MCP server over stdio: the contact book as `to://` resources (Claude Code `@owl:` mentions)
    Mcp,
    /// One-shot first install: init (if no key), install the daemon, install the Claude Code plugin
    Setup {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        email: Vec<String>,
        /// Marketplace source for `claude plugin marketplace add` (a local plugin dir or owner/repo)
        #[arg(long, value_name = "DIR|REPO")]
        plugin_source: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Update the owl binary, restart the daemon, reinstall the Claude Code plugin
    Update {
        /// Build from this checkout with `cargo install` instead of downloading a release
        #[arg(long)]
        source: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum HarnessCmd {
    /// One row per configured harness: name, drafts, found, command
    List,
    /// Add the known harnesses found on PATH that the config lacks
    Scan,
    /// Add a harness; the command is everything after `--`
    Add {
        name: String,
        /// Where the answer text sits in the harness output
        #[arg(long, default_value = "raw")]
        answer_path: String,
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Replace a harness's command (everything after `--`)
    Edit {
        name: String,
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Remove a harness
    Remove { name: String },
    /// The harness that drafts answers (responder.harness)
    Use { name: String },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Map a checkout (default: the current directory) under a project key
    Add {
        path: Option<PathBuf>,
        /// The key peers ask about (default: what `owl ask` sends from there — the origin
        /// remote as host/org/repo, else the directory name)
        #[arg(long)]
        name: Option<String>,
    },
    /// One row per project: name, checkout, `missing` when the checkout is gone
    List,
    /// Unmap a project
    Remove { name: String },
}

#[derive(Subcommand)]
enum ContactCmd {
    /// One row per contact: name, fingerprint, source (global|local), policy
    List {
        /// Only $OWLPOST_HOME/contacts/
        #[arg(long, conflicts_with = "local")]
        global: bool,
        /// Only <git root>/.agents/peers/
        #[arg(long)]
        local: bool,
    },
    /// Print one contact as JSON
    Show { peer: String },
    /// This machine's peer file (from config + key) as JSON
    Export,
    /// Delete a contact's file from the global book (or .agents/peers/ with --local)
    Remove {
        peer: String,
        #[arg(long)]
        local: bool,
    },
}

fn whoami(home: &Path, json: bool) -> anyhow::Result<()> {
    if !config::Config::path(home).exists() || !home.join("key").exists() {
        anyhow::bail!(
            "no identity found in {} — run `owl init` first",
            home.display()
        );
    }
    let cfg = config::Config::load(home)?;
    let id = identity::Identity::load(home)?;
    let pk = id.verifying_key();
    let fp = identity::fingerprint(&pk);
    let pubkey = identity::pubkey_string(&pk);
    if json {
        let out = serde_json::json!({
            "fingerprint": fp,
            "pubkey": pubkey,
            "name": cfg.name,
            "endpoints": cfg.endpoints,
        });
        println!("{out}");
    } else {
        println!("fingerprint: {fp}");
        println!("pubkey: {pubkey}");
        println!("name: {}", cfg.name);
        println!("endpoints: {}", cfg.endpoints.join(", "));
    }
    Ok(())
}

fn require_identity(home: &Path) -> anyhow::Result<(config::Config, identity::Identity)> {
    if !config::Config::path(home).exists() || !home.join("key").exists() {
        anyhow::bail!(
            "no identity found in {} — run `owl init` first",
            home.display()
        );
    }
    Ok((config::Config::load(home)?, identity::Identity::load(home)?))
}

fn contact(home: &Path, cmd: ContactCmd, json: bool) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let book = || contacts::ContactBook::load(home, &cwd);
    match cmd {
        ContactCmd::List { global, local } => {
            let book = match (global, local) {
                (true, _) => contacts::ContactBook::load_scope(home, &cwd, "global")?,
                (_, true) => contacts::ContactBook::load_scope(home, &cwd, "local")?,
                _ => book()?,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&book.contacts)?);
            } else {
                println!(
                    "{:<20} {:<20} {:<6} POLICY",
                    "NAME", "FINGERPRINT", "SOURCE"
                );
                for c in &book.contacts {
                    let mode = c.policy.as_ref().map_or("-", |p| p.mode.as_str());
                    println!(
                        "{:<20} {:<20} {:<6} {mode}",
                        c.name, c.fingerprint, c.source
                    );
                }
            }
        }
        ContactCmd::Show { peer } => {
            let book = book()?;
            println!("{}", serde_json::to_string_pretty(book.resolve(&peer)?)?);
        }
        ContactCmd::Export => {
            let (cfg, id) = require_identity(home)?;
            let out = serde_json::json!({
                "name": cfg.name,
                "emails": cfg.emails,
                "pubkey": identity::pubkey_string(&id.verifying_key()),
                "endpoints": cfg.endpoints,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        ContactCmd::Remove { peer, local } => {
            let scope = if local { "local" } else { "global" };
            match contacts::ContactBook::remove(home, &cwd, scope, &peer) {
                Ok((name, _)) => println!("removed {name} ({scope})"),
                Err(e) => {
                    // The same peer in the other scope gets a hint naming the flag.
                    let hint = match book()?.resolve(&peer) {
                        Ok(other) if other.source == "local" && !local => {
                            format!("{} is a local contact; use --local", other.name)
                        }
                        Ok(other) if other.source == "global" && local => {
                            format!("{} is a global contact; drop --local", other.name)
                        }
                        _ => return Err(e),
                    };
                    return Err(cli::user_error(hint));
                }
            }
        }
    }
    Ok(())
}

/// `owl daemon [--foreground]`. Both forms run attached: detaching is the service manager's
/// job (`owl install`), so the flag documents intent rather than changing behaviour.
// ponytail: no self-daemonising — launchd/systemd keep the process in the foreground anyway.
fn daemon_cmd(home: &Path, _foreground: bool) -> anyhow::Result<()> {
    let (cfg, _id) = require_identity(home)?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(daemon::run_foreground(home, cfg))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let home = config::home_dir(cli.home.as_deref());
    let result = match cli.cmd {
        Cmd::Init { name, email } => cli::setup::init(&home, name, email),
        Cmd::Whoami => whoami(&home, cli.json),
        Cmd::Contact { cmd } => contact(&home, cmd, cli.json),
        Cmd::Add { source, local } => cli::add::run(&home, &source, local),
        Cmd::Daemon { foreground } => daemon_cmd(&home, foreground),
        Cmd::Inbox {
            format, hook_event, ..
        } if hook_event == cli::inbox::FILE_CHANGED && format.as_deref() != Some("claude") => {
            // Only Claude Code has a FileChanged hook: a usage error (exit 2) whose stderr
            // text tells it apart from the wake's exit 2.
            use clap::{CommandFactory, error::ErrorKind};
            Cli::command()
                .error(
                    ErrorKind::ArgumentConflict,
                    "--hook-event FileChanged requires --format claude",
                )
                .exit()
        }
        Cmd::Inbox {
            count,
            format,
            hook_event,
            ..
        } if hook_event == cli::inbox::SESSION_END
            && (!count || format.as_deref() != Some("claude")) =>
        {
            // SessionEnd only tears down the session's wake dir: it is the Claude hook's
            // `--count --format claude` invocation or a usage error.
            use clap::{CommandFactory, error::ErrorKind};
            Cli::command()
                .error(
                    ErrorKind::ArgumentConflict,
                    "--hook-event SessionEnd requires --count --format claude",
                )
                .exit()
        }
        Cmd::Inbox {
            count,
            new,
            all,
            format,
            follow,
            session,
            session_start: _,
            hook_event,
        } => cli::inbox::run(
            &home,
            cli::inbox::Opts {
                count,
                new,
                all,
                format,
                json: cli.json,
                follow,
                session,
                hook_event,
            },
        ),
        Cmd::Allow {
            peer,
            once,
            always,
            i_verified_the_fingerprint,
        } => cli::allow::run(
            &home,
            &peer,
            cli::allow::Opts {
                once,
                always,
                verified: i_verified_the_fingerprint,
            },
            cli.json,
        ),
        Cmd::Deny { peer } => cli::deny::run(&home, &peer, cli.json),
        // `#N` from the `owl inbox` listing is accepted wherever a record id is.
        Cmd::Show { id, format } => cli::resolve_id(&home, &id)
            .and_then(|id| cli::show::run(&home, &id, cli.json, format.as_deref())),
        Cmd::Draft {
            id,
            harness,
            text,
            agent,
            prompt,
        } => cli::resolve_id(&home, &id).and_then(|id| {
            cli::draft::run(
                &home,
                &id,
                cli::draft::Opts {
                    harness: harness.as_deref(),
                    text,
                    agent,
                    prompt,
                },
                cli.json,
            )
        }),
        Cmd::Edit { id } => {
            cli::resolve_id(&home, &id).and_then(|id| cli::edit::run(&home, &id, cli.json))
        }
        Cmd::Send { id } => {
            cli::resolve_id(&home, &id).and_then(|id| cli::send::run(&home, &id, cli.json))
        }
        Cmd::Reject { id } => {
            cli::resolve_id(&home, &id).and_then(|id| cli::reject::run(&home, &id, cli.json))
        }
        Cmd::Route { id } => {
            cli::resolve_id(&home, &id).and_then(|id| cli::route::run(&home, &id, cli.json))
        }
        Cmd::Thread {
            peer,
            since,
            context,
            archived,
        } => cli::thread::run(
            &home,
            peer.as_deref(),
            cli::thread::Filters {
                since,
                context,
                archived,
            },
            cli.json,
        ),
        Cmd::Archive { peer, context } => cli::archive::run(
            &home,
            &peer,
            context.as_deref(),
            cli::archive::Action::Archive,
            cli.json,
        ),
        Cmd::Unarchive { peer, context } => cli::archive::run(
            &home,
            &peer,
            context.as_deref(),
            cli::archive::Action::Unarchive,
            cli.json,
        ),
        Cmd::Delete { peer, context } => cli::archive::run(
            &home,
            &peer,
            context.as_deref(),
            cli::archive::Action::Delete,
            cli.json,
        ),
        Cmd::Undo => cli::archive::undo(&home, cli.json),
        Cmd::History { peer, path, since } => {
            cli::history::run(&home, cli::history::Filters { peer, path, since }, cli.json)
        }
        Cmd::Ask(args) => cli::ask::run(&home, args, cli.json, cli.quiet),
        Cmd::Request(args) => cli::request::run(&home, args, cli.json),
        Cmd::Call(args) => cli::call::run(&home, args, cli.json),
        Cmd::Status { id } => cli::status::run(&home, id.as_deref(), cli.json),
        Cmd::Card { peer } => cli::card::run(&home, peer.as_deref()),
        Cmd::Presence => cli::presence::presence(&home, cli.json),
        Cmd::Ping { peer } => cli::presence::ping(&home, &peer, cli.json),
        Cmd::Watch { id, timeout } => cli::watch::run(&home, id.as_deref(), timeout),
        Cmd::Install { dry_run } => {
            cli::install::owl_path().and_then(|owl| cli::install::install(&home, dry_run, &owl))
        }
        Cmd::Uninstall => cli::install::uninstall(),
        Cmd::Stop => cli::install::stop_service(),
        Cmd::Start => cli::install::start_service(),
        Cmd::Doctor => cli::doctor::run(&home, cli.json),
        Cmd::Harness { cmd } => cli::harness::run(&home, cmd, cli.json),
        Cmd::Project { cmd } => cli::project::run(&home, cmd, cli.json),
        Cmd::Mcp => cli::mcp::run(&home),
        Cmd::Setup {
            name,
            email,
            plugin_source,
            dry_run,
        } => cli::setup::run_setup(
            &home,
            cli::setup::Opts {
                name,
                emails: email,
                plugin_source,
                dry_run,
            },
        ),
        Cmd::Update { source, dry_run } => {
            cli::update::run_update(&home, source.as_deref(), dry_run)
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("owl: {e:#}");
            ExitCode::from(cli::exit_code(&e))
        }
    }
}
