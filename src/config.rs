//! `Config` struct, `$OWLPOST_HOME` resolution, load/save with §3 defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

pub const DEFAULT_LISTEN: &str = "0.0.0.0:7411";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub name: String,
    pub emails: Vec<String>,
    pub listen: String,
    pub endpoints: Vec<String>,
    /// Relays for the iroh transport. Absent (`None`) = n0's public relays and DNS
    /// discovery; a list = exactly those (self-hosted) relays, no public discovery; `[]` =
    /// no relay at all (the endpoint is bound but only `endpoints` remain reachable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_urls: Option<Vec<String>>,
    pub pull_interval_secs: u64,
    pub outbox_ttl_days: u64,
    pub rate_limit_per_peer_per_hour: u64,
    pub notify: bool,
    pub responder: Responder,
    pub harnesses: BTreeMap<String, Harness>,
    pub projects: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Responder {
    pub enabled: bool,
    pub harness: String,
    pub scope: Scope,
    pub redact: Vec<String>,
    pub timeout_secs: u64,
    /// Root of the memory store a peer may ask one entry of. Absent by default
    /// (`owl init` writes nothing for it) and canonicalised by [`Config::load`], so the
    /// `memory` key check in `owl draft` compares two resolved paths. `scope.private_memory`
    /// must be `true` as well: this says *where*, that switch says *whether*.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_root: Option<String>,
    /// The tool registry a peer's `tool-call` request may name. Empty by default,
    /// so the feature is off until the owner adds a tool by hand; the registry *is* the
    /// allowlist, and a tool it does not name never reaches consent.
    #[serde(default)]
    pub tools: BTreeMap<String, Tool>,
}

/// One entry of `responder.tools`. The peer's input never touches `argv`: it goes
/// to the child on stdin, so there is no placeholder to interpolate and no quoting to get
/// wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    /// `argv[0]` is resolved on `PATH` like a harness command.
    pub argv: Vec<String>,
    /// A key of `config.projects` (the checkout the tool runs in), or `null` for
    /// `$OWLPOST_HOME`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub timeout_ms: u64,
    /// Cap on the compact serialisation of the request's `input` object, in bytes.
    pub max_input_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scope {
    pub repo: bool,
    pub project_files: bool,
    pub private_memory: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Harness {
    pub cmd: Vec<String>,
    pub answer_path: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Appended to `cmd` as `--model <model>` when set, so the responder need not run on the
    /// harness's default (most expensive) model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

fn yes() -> bool {
    true
}

impl Default for Scope {
    fn default() -> Self {
        Self {
            repo: true,
            project_files: true,
            private_memory: false,
        }
    }
}

impl Default for Responder {
    fn default() -> Self {
        Self {
            enabled: true,
            harness: "claude".into(),
            scope: Scope::default(),
            redact: vec![
                r"(?i)(api[_-]?key|secret|token|password)\s*[:=]\s*\S+".into(),
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----"
                    .into(),
            ],
            timeout_secs: 180,
            memory_root: None,
            tools: BTreeMap::new(),
        }
    }
}

fn harness(cmd: &[&str], answer_path: &str) -> Harness {
    Harness {
        cmd: cmd.iter().map(|s| s.to_string()).collect(),
        answer_path: answer_path.into(),
        enabled: true,
        disabled_reason: None,
        env: BTreeMap::new(),
        model: None,
    }
}

impl Default for Config {
    fn default() -> Self {
        let mut harnesses = BTreeMap::new();
        harnesses.insert(
            "claude".into(),
            harness(
                &[
                    "claude",
                    "-p",
                    "--allowed-tools",
                    "Read,Grep,Glob",
                    "--output-format",
                    "json",
                    "{prompt}",
                ],
                "result",
            ),
        );
        harnesses.insert(
            "codex".into(),
            harness(
                &[
                    "codex",
                    "exec",
                    "--sandbox",
                    "read-only",
                    "--ephemeral",
                    "--json",
                    "{prompt}",
                ],
                "last_message",
            ),
        );
        harnesses.insert(
            "opencode".into(),
            harness(
                &[
                    "opencode",
                    "run",
                    "--format",
                    "json",
                    "--agent",
                    "owl-readonly",
                    "{prompt}",
                ],
                "last_text",
            ),
        );
        harnesses.insert(
            "kimi".into(),
            Harness {
                enabled: false,
                disabled_reason: Some(
                    "read-only enforcement under -p unverified (see concept.md open questions)"
                        .into(),
                ),
                ..harness(
                    &["kimi", "-p", "{prompt}", "--output-format", "stream-json"],
                    "last_text",
                )
            },
        );
        harnesses.insert(
            "fake".into(),
            harness(&["tests/fixtures/fake-harness.sh", "{prompt}"], "raw"),
        );
        Self {
            name: String::new(),
            emails: Vec::new(),
            listen: DEFAULT_LISTEN.into(),
            endpoints: Vec::new(),
            relay_urls: None,
            pull_interval_secs: 60,
            outbox_ttl_days: 14,
            rate_limit_per_peer_per_hour: 20,
            notify: true,
            responder: Responder::default(),
            harnesses,
            projects: BTreeMap::new(),
        }
    }
}

/// `--home` > `$OWLPOST_HOME` > `$HOME/.config/owlpost`.
pub fn home_dir(cli_home: Option<&Path>) -> PathBuf {
    if let Some(h) = cli_home {
        return h.to_path_buf();
    }
    if let Some(h) = std::env::var_os("OWLPOST_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h);
    }
    // ponytail: HOME only — no `dirs` crate; Windows is not a target.
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join(".config")
        .join("owlpost")
}

impl Config {
    pub fn path(home: &Path) -> PathBuf {
        home.join("config.json")
    }

    /// Missing file → defaults. Malformed file → error.
    pub fn load(home: &Path) -> anyhow::Result<Config> {
        let path = Self::path(home);
        let mut config: Config = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        // The memory root is compared against a canonicalised entry path, so it is
        // resolved here. A root that does not exist stays as configured — `owl draft` is
        // where that is reported, not `Config::load`, which every command runs.
        if let Some(root) = &config.responder.memory_root
            && let Ok(resolved) = std::fs::canonicalize(root)
        {
            config.responder.memory_root = Some(resolved.to_string_lossy().into_owned());
        }
        // The tool registry is the allowlist, so a registry that cannot mean what
        // it says is refused here rather than at the moment a peer names the tool.
        config.check_tools()?;
        Ok(config)
    }

    /// The three refusals of §3's tool registry, each of them a `config:` error
    /// that stops every command until the owner fixes the file:
    ///
    /// - an empty `argv` — there is nothing to run;
    /// - an `argv` carrying the literal `{input}` — the input is never interpolated, it goes
    ///   to the child on stdin, and a config written as if it were would silently send the
    ///   peer's words to a tool that never reads them;
    /// - a `cwd` naming a project that is not configured.
    fn check_tools(&self) -> anyhow::Result<()> {
        for (name, tool) in &self.responder.tools {
            if tool.argv.is_empty() {
                anyhow::bail!("config: tools.{name}.argv must not be empty");
            }
            if tool.argv.iter().any(|a| a.contains("{input}")) {
                anyhow::bail!("config: tools.{name}.argv must not interpolate the input");
            }
            if let Some(cwd) = &tool.cwd
                && !self.projects.contains_key(cwd)
            {
                anyhow::bail!("config: tools.{name}.cwd names no configured project: {cwd}");
            }
        }
        Ok(())
    }

    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
        let path = Self::path(home);
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_missing() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Config::load(home.path()).unwrap();
        assert_eq!(cfg.listen, "0.0.0.0:7411");
        assert_eq!(cfg.pull_interval_secs, 60);
        assert_eq!(cfg.rate_limit_per_peer_per_hour, 20);
        for key in ["claude", "codex", "opencode", "kimi", "fake"] {
            assert!(cfg.harnesses.contains_key(key), "missing harness {key}");
        }
        assert!(!cfg.harnesses["kimi"].enabled);
        assert!(cfg.harnesses["kimi"].disabled_reason.is_some());
        assert!(cfg.harnesses["claude"].enabled);
        assert_eq!(
            cfg.harnesses["claude"].model, None,
            "no model flag by default"
        );
        assert_eq!(cfg.outbox_ttl_days, 14);
        assert!(cfg.notify);
        assert_eq!(cfg.responder.harness, "claude");
        assert_eq!(cfg.responder.timeout_secs, 180);
        assert_eq!(cfg.responder.redact.len(), 2);
        assert!(cfg.name.is_empty() && cfg.emails.is_empty());
        assert!(cfg.endpoints.is_empty() && cfg.projects.is_empty());
        assert_eq!(cfg.relay_urls, None, "absent = n0 public relays");
    }

    #[test]
    fn relay_urls_three_states() {
        let home = tempfile::tempdir().unwrap();
        let load = |json: &str| {
            std::fs::write(Config::path(home.path()), json).unwrap();
            Config::load(home.path())
        };
        assert_eq!(load(r#"{}"#).unwrap().relay_urls, None);
        assert_eq!(load(r#"{"relay_urls": null}"#).unwrap().relay_urls, None);
        assert_eq!(
            load(r#"{"relay_urls": []}"#).unwrap().relay_urls,
            Some(vec![])
        );
        assert_eq!(
            load(r#"{"relay_urls": ["https://relay.corp.example/"]}"#)
                .unwrap()
                .relay_urls,
            Some(vec!["https://relay.corp.example/".to_string()])
        );
        assert!(
            load(r#"{"relay_urls": "https://one"}"#).is_err(),
            "not a list"
        );
        assert!(load(r#"{"relay_urls": [1]}"#).is_err(), "not strings");
        // Saved: the absent state stays absent, a list roundtrips.
        Config::default().save(home.path()).unwrap();
        let raw = std::fs::read_to_string(Config::path(home.path())).unwrap();
        assert!(!raw.contains("relay_urls"), "{raw}");
        let cfg = Config {
            relay_urls: Some(vec![]),
            ..Default::default()
        };
        cfg.save(home.path()).unwrap();
        assert!(
            std::fs::read_to_string(Config::path(home.path()))
                .unwrap()
                .contains("\"relay_urls\": []")
        );
        assert_eq!(Config::load(home.path()).unwrap(), cfg);
    }

    #[test]
    fn defaults_match_design_section_3() {
        let cfg = Config::default();
        assert_eq!(cfg.outbox_ttl_days, 14);
        assert!(cfg.notify);
        assert!(cfg.responder.enabled);
        assert_eq!(cfg.responder.harness, "claude");
        assert_eq!(cfg.responder.timeout_secs, 180);
        assert_eq!(
            cfg.responder.scope,
            Scope {
                repo: true,
                project_files: true,
                private_memory: false
            }
        );
        assert_eq!(
            cfg.responder.redact,
            vec![
                r"(?i)(api[_-]?key|secret|token|password)\s*[:=]\s*\S+".to_string(),
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----"
                    .to_string(),
            ]
        );
        let cmd = |k: &str| cfg.harnesses[k].cmd.clone();
        let ap = |k: &str| cfg.harnesses[k].answer_path.as_str();
        assert_eq!(
            cmd("claude"),
            [
                "claude",
                "-p",
                "--allowed-tools",
                "Read,Grep,Glob",
                "--output-format",
                "json",
                "{prompt}"
            ]
        );
        assert_eq!(ap("claude"), "result");
        assert_eq!(
            cmd("codex"),
            [
                "codex",
                "exec",
                "--sandbox",
                "read-only",
                "--ephemeral",
                "--json",
                "{prompt}"
            ]
        );
        assert_eq!(ap("codex"), "last_message");
        assert_eq!(
            cmd("opencode"),
            [
                "opencode",
                "run",
                "--format",
                "json",
                "--agent",
                "owl-readonly",
                "{prompt}"
            ]
        );
        assert_eq!(ap("opencode"), "last_text");
        assert_eq!(
            cmd("kimi"),
            ["kimi", "-p", "{prompt}", "--output-format", "stream-json"]
        );
        assert_eq!(ap("kimi"), "last_text");
        assert!(!cfg.harnesses["kimi"].enabled);
        assert_eq!(
            cfg.harnesses["kimi"].disabled_reason.as_deref(),
            Some("read-only enforcement under -p unverified (see concept.md open questions)")
        );
        assert_eq!(cmd("fake"), ["tests/fixtures/fake-harness.sh", "{prompt}"]);
        assert_eq!(ap("fake"), "raw");
        for k in ["claude", "codex", "opencode", "fake"] {
            assert!(cfg.harnesses[k].enabled, "{k} enabled");
            assert!(cfg.harnesses[k].disabled_reason.is_none(), "{k} reason");
        }
        assert_eq!(cfg.harnesses.len(), 5);
    }

    #[test]
    fn roundtrip() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().join("nested"); // save must create the dir
        let mut cfg = Config {
            name: "Alex".into(),
            emails: vec!["k@example.com".into()],
            endpoints: vec!["host:7411".into()],
            ..Default::default()
        };
        cfg.projects
            .insert("github.com/x/y".into(), "/tmp/y".into());
        cfg.harnesses
            .get_mut("fake")
            .unwrap()
            .env
            .insert("FOO".into(), "bar".into());
        cfg.save(&home).unwrap();
        let loaded = Config::load(&home).unwrap();
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn partial_file_fills_defaults() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            Config::path(home.path()),
            r#"{"name":"A","harnesses":{"x":{"cmd":["x"],"answer_path":"raw"}}}"#,
        )
        .unwrap();
        let cfg = Config::load(home.path()).unwrap();
        assert_eq!(cfg.name, "A");
        assert_eq!(cfg.listen, DEFAULT_LISTEN);
        assert!(cfg.harnesses["x"].enabled);
        assert!(cfg.harnesses["x"].env.is_empty());
    }

    #[test]
    fn malformed_file_is_error() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(Config::path(home.path()), "{not json").unwrap();
        assert!(Config::load(home.path()).is_err());
    }

    #[test]
    fn home_precedence() {
        // Env-var mutation is process-global; keep it inside one test.
        let cli = tempfile::tempdir().unwrap();
        let env_home = tempfile::tempdir().unwrap();
        let user_home = tempfile::tempdir().unwrap();
        // SAFETY: tests in this module run in one process; no other test touches these vars.
        unsafe {
            std::env::set_var("HOME", user_home.path());
            std::env::set_var("OWLPOST_HOME", env_home.path());
        }
        assert_eq!(home_dir(Some(cli.path())), cli.path());
        assert_eq!(home_dir(None), env_home.path());
        let fallback = user_home.path().join(".config").join("owlpost");
        unsafe {
            std::env::set_var("OWLPOST_HOME", "");
        }
        assert_eq!(
            home_dir(None),
            fallback,
            "empty OWLPOST_HOME must fall back"
        );
        unsafe {
            std::env::remove_var("OWLPOST_HOME");
        }
        assert_eq!(home_dir(None), fallback);
    }
}
