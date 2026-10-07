//! `owl project` through the real binary on a temp home: add (default path and name, git
//! origin, `--name`, update, refusals), list (table, `missing`, `--json`) and remove, each
//! checked against the `config.json` it leaves behind.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use owlpost::config::Config;
use serde_json::json;

fn owl(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_owl"))
        .env_remove("OWLPOST_HOME")
        .current_dir(cwd)
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn canon(p: &Path) -> String {
    p.canonicalize().unwrap().to_string_lossy().into_owned()
}

#[test]
fn add_list_remove_change_config_json() {
    let home = tempfile::tempdir().unwrap();
    common::prepare_home_with(home.path(), &common::id(2), &[], |_| {});
    let work = tempfile::tempdir().unwrap();
    let plain = work.path().join("plain");
    let repo = work.path().join("repo");
    std::fs::create_dir(&plain).unwrap();
    std::fs::create_dir(&repo).unwrap();
    for args in [
        &["init", "-q"][..],
        &["remote", "add", "origin", "git@github.com:org/repo.git"],
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }

    // Default path (the cwd) and name (the directory name, no git).
    let out = stdout(&owl(home.path(), &plain, &["project", "add"]));
    assert_eq!(out, format!("added plain → {}\n", canon(&plain)));
    // Default name from the origin remote, path given relative to the cwd.
    let out = stdout(&owl(home.path(), work.path(), &["project", "add", "repo"]));
    assert_eq!(
        out,
        format!("added github.com/org/repo → {}\n", canon(&repo))
    );
    // An existing key is updated.
    let out = stdout(&owl(
        home.path(),
        work.path(),
        &["project", "add", "plain", "--name", "github.com/org/repo"],
    ));
    assert_eq!(
        out,
        format!("updated github.com/org/repo → {}\n", canon(&plain))
    );
    let cfg = Config::load(home.path()).unwrap();
    assert_eq!(cfg.projects["plain"], canon(&plain));
    assert_eq!(cfg.projects["github.com/org/repo"], canon(&plain));
    // The fixture's own project survives the raw edit.
    assert!(cfg.projects.contains_key(common::PROJECT));

    // Refusals leave the file untouched.
    let before = std::fs::read(Config::path(home.path())).unwrap();
    std::fs::write(work.path().join("file"), "x").unwrap();
    for bad in ["nope", "file"] {
        let out = owl(home.path(), work.path(), &["project", "add", bad]);
        assert_eq!(out.status.code(), Some(1), "{bad}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(&format!("{bad} is not a directory")), "{err}");
    }
    let out = owl(home.path(), work.path(), &["project", "remove", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no project nope"));
    assert_eq!(std::fs::read(Config::path(home.path())).unwrap(), before);

    // List: a gone checkout is marked `missing`.
    let plain_path = canon(&plain);
    std::fs::remove_dir(&plain).unwrap();
    let out = stdout(&owl(home.path(), work.path(), &["project", "list"]));
    let line = |name: &str| {
        out.lines()
            .find(|l| l.starts_with(&format!("{name} ")))
            .unwrap_or_else(|| panic!("{name}: {out}"))
            .to_string()
    };
    assert!(line("plain").trim_end().ends_with("missing"), "{out}");
    assert!(
        line(common::PROJECT).trim_end().ends_with("missing"),
        "{out}"
    );
    let out = stdout(&owl(
        home.path(),
        work.path(),
        &["--json", "project", "list"],
    ));
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(
        rows.as_array()
            .unwrap()
            .contains(&json!({ "name": "plain", "path": plain_path, "exists": false })),
        "{rows}"
    );

    let out = stdout(&owl(
        home.path(),
        work.path(),
        &["project", "remove", "plain"],
    ));
    assert_eq!(out, "removed plain\n");
    let cfg = Config::load(home.path()).unwrap();
    assert!(!cfg.projects.contains_key("plain"));
    assert!(cfg.projects.contains_key("github.com/org/repo"));
}

#[test]
fn list_on_an_empty_table_says_how_to_add() {
    let home = tempfile::tempdir().unwrap();
    let out = stdout(&owl(home.path(), home.path(), &["project", "list"]));
    assert_eq!(out, "no projects — owl project add [path]\n");
}
