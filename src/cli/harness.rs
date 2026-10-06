//! `owl harness`: the harness table of `config.json` — list the configured
//! harnesses with their on-PATH status, scan PATH for the known ones the config lacks, and
//! add/edit/remove/use entries. Mutators validate against the typed `Config::load` but write
//! through the raw JSON, so only the harness keys of the file change.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use owlpost::config::{Config, Harness};
use owlpost::runner;
use serde_json::{Map, Value, json};

use super::{print_json, print_table, user_error};
use crate::HarnessCmd;

/// The known harnesses (name order): the default table minus `fake`, a test fixture.
fn known() -> impl Iterator<Item = (String, Harness)> {
    let mut h = Config::default().harnesses;
    h.remove("fake");
    h.into_iter()
}

/// `found` + full path for a command's first word: the same resolution doctor makes. A
/// bare name found on PATH resolves to itself, so report the full `<dir>/<name>` of the
/// first PATH entry that holds it instead.
fn locate(first: &str, path_var: Option<&OsStr>, home: &Path) -> Option<String> {
    let resolved = runner::resolve_program_in(first, path_var, home).ok()?;
    if resolved != Path::new(first) {
        return Some(resolved.to_string_lossy().into_owned());
    }
    path_var.and_then(|paths| {
        std::env::split_paths(paths)
            .map(|d| d.join(first))
            .find(|p| p.is_file())
            .map(|p| p.to_string_lossy().into_owned())
    })
}

/// One `owl harness list --json` row (also what the add/edit mutators print).
fn row(config: &Config, name: &str, h: &Harness, env: &Env) -> Value {
    let path = h
        .cmd
        .first()
        .and_then(|f| locate(f, env.path.as_deref(), &env.home));
    json!({
        "name": name,
        "cmd": h.cmd,
        "answer_path": h.answer_path,
        "enabled": h.enabled,
        "drafting": config.responder.harness == name,
        "found": path.is_some(),
        "path": path,
    })
}

/// The process environment `found` is judged against, grabbed once per command.
struct Env {
    path: Option<std::ffi::OsString>,
    home: PathBuf,
}

impl Env {
    fn current() -> Env {
        Env {
            path: std::env::var_os("PATH"),
            home: PathBuf::from(std::env::var_os("HOME").unwrap_or_default()),
        }
    }
}

/// Read `config.json` as raw JSON (a missing file is `{}`), apply `change` and write it back
/// the way `Config::save` does. Mutators go through this instead of `Config::save`, which
/// would drop unknown keys, write out every default and canonicalise `memory_root`.
fn edit_raw(
    home: &Path,
    change: impl FnOnce(&mut Value) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let path = Config::path(home);
    let mut raw: Value = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    change(&mut raw)?;
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    std::fs::write(&path, serde_json::to_vec_pretty(&raw)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// The `harnesses` object of the raw document. A file without one still has the
/// `#[serde(default)]` table in effect, so materialise it from the loaded typed config before
/// editing — otherwise the write would drop every harness but the touched one.
fn raw_harnesses<'a>(
    raw: &'a mut Value,
    config: &Config,
) -> anyhow::Result<&'a mut Map<String, Value>> {
    if raw.get("harnesses").is_none() {
        raw["harnesses"] = serde_json::to_value(&config.harnesses)?;
    }
    Ok(raw["harnesses"]
        .as_object_mut()
        .expect("Config::load parsed harnesses as a table"))
}

fn list(config: &Config, json: bool) -> anyhow::Result<()> {
    let env = Env::current();
    if json {
        let rows: Vec<Value> = config
            .harnesses
            .iter()
            .map(|(name, h)| row(config, name, h, &env))
            .collect();
        return print_json(&Value::Array(rows));
    }
    let rows: Vec<Vec<String>> = config
        .harnesses
        .iter()
        .map(|(name, h)| {
            let drafting = if config.responder.harness == *name {
                "yes"
            } else {
                ""
            };
            let found = if !h.enabled {
                format!(
                    "disabled: {}",
                    h.disabled_reason.as_deref().unwrap_or("disabled")
                )
            } else {
                h.cmd
                    .first()
                    .and_then(|f| locate(f, env.path.as_deref(), &env.home))
                    .unwrap_or_else(|| "not found".into())
            };
            vec![name.clone(), drafting.into(), found, h.cmd.join(" ")]
        })
        .collect();
    print_table(&["NAME", "DRAFTS", "FOUND", "COMMAND"], &rows);
    Ok(())
}

fn scan(home: &Path, json: bool) -> anyhow::Result<()> {
    let env = Env::current();
    let config = Config::load(home)?;
    let mut additions = Vec::new();
    let mut rows = Vec::new();
    for (name, h) in known() {
        let path = h
            .cmd
            .first()
            .and_then(|f| locate(f, env.path.as_deref(), &env.home));
        let added = path.is_some() && !config.harnesses.contains_key(&name);
        if added {
            additions.push((name.clone(), h));
        }
        rows.push(json!({
            "name": name,
            "found": path.is_some(),
            "path": path,
            "added": added,
        }));
    }
    if !additions.is_empty() {
        edit_raw(home, |raw| {
            let table = raw_harnesses(raw, &config)?;
            for (name, h) in &additions {
                table.insert(name.clone(), serde_json::to_value(h)?);
            }
            Ok(())
        })?;
    }
    if json {
        return print_json(&Value::Array(rows));
    }
    let names = |pred: fn(&Value) -> bool| -> String {
        rows.iter()
            .filter(|r| pred(r))
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut parts = Vec::new();
    let on_path = names(|r| r["found"] == true);
    if !on_path.is_empty() {
        parts.push(format!("{on_path} on PATH"));
    }
    let not_found = names(|r| r["found"] != true);
    if !not_found.is_empty() {
        parts.push(format!("{not_found} not found"));
    }
    println!("scan: {}", parts.join(" · "));
    let added = names(|r| r["added"] == true);
    if !added.is_empty() {
        println!("added {added}");
    }
    Ok(())
}

pub fn run(home: &Path, cmd: HarnessCmd, json: bool) -> anyhow::Result<()> {
    match cmd {
        HarnessCmd::List => list(&Config::load(home)?, json),
        HarnessCmd::Scan => scan(home, json),
        HarnessCmd::Add {
            name,
            answer_path,
            cmd,
        } => {
            let config = Config::load(home)?;
            if config.harnesses.contains_key(&name) {
                return Err(user_error(format!(
                    "harness {name} exists — use owl harness edit"
                )));
            }
            let h = Harness {
                cmd,
                answer_path,
                enabled: true,
                disabled_reason: None,
                env: Default::default(),
                model: None,
            };
            edit_raw(home, |raw| {
                raw_harnesses(raw, &config)?.insert(name.clone(), serde_json::to_value(&h)?);
                Ok(())
            })?;
            if json {
                print_json(&row(&config, &name, &h, &Env::current()))?;
            } else {
                println!("added harness {name}");
            }
            Ok(())
        }
        HarnessCmd::Edit { name, cmd } => {
            let config = Config::load(home)?;
            let Some(h) = config.harnesses.get(&name) else {
                return Err(user_error(format!("no harness {name}")));
            };
            let mut h = h.clone();
            h.cmd = cmd;
            edit_raw(home, |raw| {
                let entry = raw_harnesses(raw, &config)?
                    .get_mut(&name)
                    .expect("the typed config has the entry");
                entry["cmd"] = serde_json::to_value(&h.cmd)?;
                Ok(())
            })?;
            if json {
                print_json(&row(&config, &name, &h, &Env::current()))?;
            } else {
                println!("harness {name} now runs {}", h.cmd.join(" "));
            }
            Ok(())
        }
        HarnessCmd::Remove { name } => {
            let config = Config::load(home)?;
            if !config.harnesses.contains_key(&name) {
                return Err(user_error(format!("no harness {name}")));
            }
            if config.responder.harness == name {
                return Err(user_error(format!(
                    "{name} drafts answers — choose another with owl harness use first"
                )));
            }
            edit_raw(home, |raw| {
                raw_harnesses(raw, &config)?.remove(&name);
                Ok(())
            })?;
            if json {
                print_json(&json!({ "name": name, "removed": true }))?;
            } else {
                println!("removed harness {name}");
            }
            Ok(())
        }
        HarnessCmd::Use { name } => {
            let config = Config::load(home)?;
            let Some(h) = config.harnesses.get(&name) else {
                return Err(user_error(format!("no harness {name}")));
            };
            if !h.enabled {
                let reason = h.disabled_reason.as_deref().unwrap_or("disabled");
                return Err(user_error(format!("harness {name} is disabled: {reason}")));
            }
            edit_raw(home, |raw| {
                raw["responder"]["harness"] = json!(name);
                Ok(())
            })?;
            if json {
                print_json(&json!({ "name": name, "drafting": true }))?;
            } else {
                println!("{name} now drafts answers");
            }
            Ok(())
        }
    }
}
