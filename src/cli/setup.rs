//! `owl setup [--name] [--email] [--plugin-source <dir|repo>] [--dry-run]`: one command from a
//! fresh machine to a working owlpost — `owl init` (skipped when a key exists), `owl install`
//! for the daemon, then `claude plugin marketplace add` + `claude plugin install`, and the
//! `owl` MCP server registered at user scope (`claude mcp add --scope user owl -- owl mcp`).

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

use owlpost::{config, identity};

use super::install;

const MARKETPLACE: &str = "owlpost-local";
const DEFAULT_PLUGIN_SOURCE: &str = "Krab00/owl-post";

/// `owl init`: create home, key, config; print the fingerprint. Refuses when a key exists.
/// A name or email not given as a flag is asked for on the terminal (skipped when stdin is
/// not a tty, so scripts and the plugin's Bash calls never hang).
pub fn init(home: &Path, name: Option<String>, emails: Vec<String>) -> anyhow::Result<()> {
    // Check the key BEFORE touching config so a refused init leaves the home untouched.
    if home.join("key").exists() {
        bail!(
            "key already exists at {} — refusing to overwrite",
            home.join("key").display()
        );
    }
    let mut cfg = config::Config::load(home)?;
    if let Some(n) = name.or(ask("Your name")?) {
        cfg.name = n;
    }
    if !emails.is_empty() {
        cfg.emails = emails;
    } else if let Some(e) = ask("Your email")? {
        cfg.emails = vec![e];
    }
    cfg.save(home)?;
    let id = identity::Identity::generate();
    id.save(home)?;
    println!("{}", identity::fingerprint(&id.verifying_key()));
    Ok(())
}

/// One interactive prompt on a terminal; `None` when stdin is not a tty or the answer is empty.
fn ask(label: &str) -> anyhow::Result<Option<String>> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Ok(None);
    }
    print!("{label}: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let line = line.trim().to_string();
    Ok((!line.is_empty()).then_some(line))
}

pub struct Opts {
    pub name: Option<String>,
    pub emails: Vec<String>,
    pub plugin_source: Option<String>,
    pub dry_run: bool,
}

fn plugin_cmds(source: &str) -> Vec<Vec<String>> {
    let c = |args: &[&str]| {
        std::iter::once("claude".to_string())
            .chain(args.iter().map(|s| s.to_string()))
            .collect::<Vec<_>>()
    };
    vec![
        c(&["plugin", "marketplace", "add", source]),
        c(&[
            "plugin",
            "install",
            &format!("owlpost@{MARKETPLACE}"),
            "--scope",
            "user",
        ]),
    ]
}

/// The user-scope registration of `owl mcp` in Claude Code: a server bundled in a
/// plugin's `.mcp.json` is named `plugin:owlpost:owl`, which sinks its resources in the `@`
/// typeahead; one added at user scope is plain `owl`, so `@owl:al` finds the contact.
pub const MCP_ADD: [&str; 9] = [
    "claude", "mcp", "add", "--scope", "user", "owl", "--", "owl", "mcp",
];

/// `claude mcp get owl` exit status: `Ok(true)` when the server is registered.
fn mcp_registered() -> anyhow::Result<bool> {
    let status = Command::new("claude")
        .args(["mcp", "get", "owl"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("running claude mcp get owl")?;
    Ok(status.success())
}

/// Idempotent registration step shared by `owl setup` and `owl update`; the caller has
/// checked that `claude` is on PATH.
pub fn register_mcp(dry_run: bool) -> anyhow::Result<()> {
    if mcp_registered()? {
        println!("mcp server owl already registered");
        return Ok(());
    }
    let cmd: Vec<String> = MCP_ADD.iter().map(|s| s.to_string()).collect();
    if dry_run {
        println!("would run: {}", cmd.join(" "));
        return Ok(());
    }
    run(&cmd)
}

fn run(cmd: &[String]) -> anyhow::Result<()> {
    println!("+ {}", cmd.join(" "));
    let status = Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("running {}", cmd[0]))?;
    if !status.success() {
        bail!("{} failed ({status})", cmd.join(" "));
    }
    Ok(())
}

pub fn run_setup(home: &Path, opts: Opts) -> anyhow::Result<()> {
    let has_claude = Command::new("claude")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let source = opts
        .plugin_source
        .unwrap_or_else(|| DEFAULT_PLUGIN_SOURCE.to_string());
    let has_key = home.join("key").exists();
    if opts.dry_run {
        if has_key {
            println!("identity exists in {}: owl init skipped", home.display());
        } else {
            println!("would run: owl init");
        }
        println!("would run: owl install");
        if has_claude {
            for c in plugin_cmds(&source) {
                println!("would run: {}", c.join(" "));
            }
            register_mcp(true)?;
        } else {
            println!("claude not on PATH: plugin install skipped");
        }
        return Ok(());
    }
    if has_key {
        println!("identity exists in {}: owl init skipped", home.display());
    } else {
        init(home, opts.name, opts.emails)?;
    }
    install::install(home, false, &install::owl_path()?)?;
    if has_claude {
        for c in plugin_cmds(&source) {
            run(&c)?;
        }
        register_mcp(false)?;
    } else {
        println!("claude not on PATH: plugin install skipped");
    }
    println!("set up; run `owl doctor`, then restart your Claude Code session to load the plugin");
    Ok(())
}
