//! `owl project`: the `projects` table of `config.json` — map a checkout under the key peers
//! ask about (`add`), list the table (`list`), drop a key (`remove`). Writes go through the
//! raw JSON like `owl harness`, so only the touched key changes. The daemon re-reads the table
//! for every incoming question, so a change applies without a restart.

use std::path::{Path, PathBuf};

use owlpost::config::Config;
use serde_json::{Value, json};

use super::harness::edit_raw;
use super::{print_json, print_table, user_error};
use crate::ProjectCmd;

pub fn run(home: &Path, cmd: ProjectCmd, json: bool) -> anyhow::Result<()> {
    match cmd {
        ProjectCmd::Add { path, name } => {
            let config = Config::load(home)?;
            let path = path.unwrap_or_else(|| PathBuf::from("."));
            let dir = std::fs::canonicalize(&path)
                .ok()
                .filter(|d| d.is_dir())
                .ok_or_else(|| user_error(format!("{} is not a directory", path.display())))?;
            // A harness runs read-only in the checkout; the owlpost home holds the key.
            if let Ok(h) = std::fs::canonicalize(home)
                && h.starts_with(&dir)
            {
                return Err(user_error(format!(
                    "{} contains the owlpost home {}",
                    dir.display(),
                    h.display()
                )));
            }
            let name = match name {
                Some(n) => {
                    let n = n.trim().to_string();
                    if n.starts_with('/')
                        || n.contains('\\')
                        || n.contains("..")
                        || n.chars().any(char::is_control)
                        || n.chars().count() > 200
                    {
                        return Err(user_error(
                            "the project name must be one line of at most 200 characters, without \\, .. or a leading /",
                        ));
                    }
                    n
                }
                // The same key `owl ask` sends from this checkout.
                None => super::ask::detect_project(&dir),
            };
            if name.trim().is_empty() {
                return Err(user_error("the project name is empty"));
            }
            let dir = dir.to_string_lossy().into_owned();
            let updated = config.projects.contains_key(&name);
            edit_raw(home, |raw| {
                raw["projects"][&name] = json!(dir);
                Ok(())
            })?;
            if json {
                print_json(&json!({ "name": name, "path": dir, "updated": updated }))
            } else {
                let verb = if updated { "updated" } else { "added" };
                println!("{verb} {name} → {dir}");
                Ok(())
            }
        }
        ProjectCmd::List => {
            let config = Config::load(home)?;
            if json {
                let rows: Vec<Value> = config
                    .projects
                    .iter()
                    .map(|(name, p)| json!({ "name": name, "path": p, "exists": Path::new(p).exists() }))
                    .collect();
                return print_json(&Value::Array(rows));
            }
            if config.projects.is_empty() {
                println!("no projects — owl project add [path]");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = config
                .projects
                .iter()
                .map(|(name, p)| {
                    let missing = if Path::new(p).exists() { "" } else { "missing" };
                    vec![name.clone(), p.clone(), missing.into()]
                })
                .collect();
            print_table(&["NAME", "PATH", ""], &rows);
            Ok(())
        }
        ProjectCmd::Remove { name } => {
            let config = Config::load(home)?;
            if !config.projects.contains_key(&name) {
                return Err(user_error(format!("no project {name}")));
            }
            // `Config::load` refuses a tool whose `cwd` names no configured project, so
            // removing its project would stop every command.
            if let Some((tool, _)) = config
                .responder
                .tools
                .iter()
                .find(|(_, t)| t.cwd.as_deref() == Some(name.as_str()))
            {
                return Err(user_error(format!(
                    "tool {tool} runs in {name} — change its cwd first"
                )));
            }
            edit_raw(home, |raw| {
                if let Some(table) = raw["projects"].as_object_mut() {
                    table.remove(&name);
                }
                Ok(())
            })?;
            if json {
                print_json(&json!({ "name": name, "removed": true }))
            } else {
                println!("removed {name}");
                Ok(())
            }
        }
    }
}
