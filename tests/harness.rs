//! `owl harness` through the real binary: a temp home and a `PATH` holding only
//! the stub executables of the fixture, so `found`/`path` and `scan` are pinned exactly.
//! Every mutator is checked against the `config.json` it leaves behind, not only its output.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use owlpost::config::Config;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    home: TempDir,
    bin: TempDir,
}

/// A home and a bin dir (the whole `PATH`); `stubs` become executable files in it.
fn fixture(stubs: &[&str]) -> Fixture {
    let f = Fixture {
        home: tempfile::tempdir().unwrap(),
        bin: tempfile::tempdir().unwrap(),
    };
    for name in stubs {
        let p = f.bin.path().join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    f
}

/// A home holding only the identity key: a half-initialised home with no `config.json`,
/// which `owl harness` must read as empty, never as an error.
fn fixture_without_config(stubs: &[&str]) -> Fixture {
    let f = fixture(stubs);
    common::prepare_home_with(f.home.path(), &common::id(2), &[], |_| {});
    std::fs::remove_file(Config::path(f.home.path())).unwrap();
    f
}

/// The sorted file names directly in the home.
fn home_files(f: &Fixture) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(f.home.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

impl Fixture {
    fn owl(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
        c.env_remove("OWLPOST_HOME")
            // The kimi fallback is `<HOME>/.kimi-code/bin/kimi`; a nonexistent HOME
            // keeps `kimi` unfound no matter what the real user home holds.
            .env("HOME", "/nonexistent-owlpost-home")
            .env("PATH", self.bin.path())
            .arg("--home")
            .arg(self.home.path());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.owl().args(args).output().unwrap()
    }

    fn config(&self) -> Config {
        Config::load(self.home.path()).unwrap()
    }

    fn config_bytes(&self) -> Vec<u8> {
        std::fs::read(Config::path(self.home.path())).unwrap()
    }

    fn config_json(&self) -> Value {
        serde_json::from_slice(&self.config_bytes()).unwrap()
    }

    fn stub_path(&self, name: &str) -> String {
        self.bin.path().join(name).to_string_lossy().into_owned()
    }
}

fn text(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn json_of(out: &Output) -> Value {
    serde_json::from_str(&text(out).0).unwrap()
}

/// The default table minus `fake` and `opencode`: `claude` (drafting, stubbed), `codex`
/// (missing) and `kimi` (disabled).
fn base_config() -> Config {
    let mut cfg = Config::default();
    cfg.harnesses.retain(|k, _| k != "fake" && k != "opencode");
    cfg
}

fn list_row(cfg: &Config, name: &str, found: Option<String>) -> Value {
    let h = &cfg.harnesses[name];
    json!({
        "name": name,
        "cmd": h.cmd,
        "answer_path": h.answer_path,
        "enabled": h.enabled,
        "drafting": cfg.responder.harness == name,
        "found": found.is_some(),
        "path": found,
    })
}

#[test]
fn list_pins_json_rows_and_the_human_table() {
    let f = fixture(&["claude"]);
    base_config().save(f.home.path()).unwrap();

    let out = f.run(&["--json", "harness", "list"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    let cfg = base_config();
    assert_eq!(
        json_of(&out),
        json!([
            list_row(&cfg, "claude", Some(f.stub_path("claude"))),
            list_row(&cfg, "codex", None),
            list_row(&cfg, "kimi", None),
        ])
    );

    let out = f.run(&["harness", "list"]);
    assert_eq!(out.status.code(), Some(0));
    let (stdout, _) = text(&out);
    let mut lines = stdout.lines();
    let header = lines.next().unwrap();
    for col in ["NAME", "DRAFTS", "FOUND", "COMMAND"] {
        assert!(header.contains(col), "{header}");
    }
    let claude = lines.next().unwrap();
    assert!(claude.starts_with("claude  yes"), "{stdout}");
    assert!(claude.contains(&f.stub_path("claude")), "{stdout}");
    assert!(
        claude.ends_with("claude -p --allowed-tools Read,Grep,Glob --output-format json {prompt}"),
        "{stdout}"
    );
    let codex = lines.next().unwrap();
    assert!(codex.starts_with("codex"), "{stdout}");
    assert!(codex.contains("not found"), "{stdout}");
    let kimi = lines.next().unwrap();
    assert!(kimi.starts_with("kimi"), "{stdout}");
    assert!(
        kimi.contains(
            "disabled: read-only enforcement under -p unverified (see concept.md open questions)"
        ),
        "{stdout}"
    );
}

#[test]
fn scan_adds_only_the_known_harnesses_found_on_path() {
    let f = fixture(&["codex"]);
    let mut cfg = Config::default();
    cfg.harnesses.remove("codex");
    cfg.harnesses.remove("fake");
    cfg.name = "Alex".into();
    cfg.projects
        .insert("owlpost".into(), "/repo/owlpost".into());
    cfg.responder.timeout_secs = 42;
    cfg.save(f.home.path()).unwrap();

    let out = f.run(&["harness", "scan"]);
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert_eq!(
        stdout,
        "scan: codex on PATH · claude, kimi, opencode not found\nadded codex\n"
    );
    // config.json gained codex with its default definition; the other keys survive.
    let after = f.config();
    assert_eq!(
        after.harnesses["codex"],
        Config::default().harnesses["codex"]
    );
    assert_eq!(after.harnesses.len(), cfg.harnesses.len() + 1);
    assert_other_keys_survive(&after);
    assert_other_harnesses_unchanged(&cfg, &after, "codex");

    // Again on the updated config: codex is found but already there, so nothing is added.
    let out = f.run(&["--json", "harness", "scan"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        json_of(&out),
        json!([
            {"name": "claude", "found": false, "path": null, "added": false},
            {"name": "codex", "found": true, "path": f.stub_path("codex"), "added": false},
            {"name": "kimi", "found": false, "path": null, "added": false},
            {"name": "opencode", "found": false, "path": null, "added": false},
        ])
    );
    assert_eq!(
        f.config().harnesses,
        after.harnesses,
        "a scan that adds nothing changed no harness entry"
    );
}

/// A scan that adds nothing must not re-save config.json: the fixture is written compact,
/// so any save would visibly change the bytes.
#[test]
fn scan_adding_nothing_leaves_config_json_byte_identical() {
    let f = fixture(&[]);
    let compact = serde_json::to_vec(&base_config()).unwrap();
    assert_ne!(
        compact,
        serde_json::to_vec_pretty(&base_config()).unwrap(),
        "a save (pretty) would visibly change the bytes"
    );
    std::fs::write(Config::path(f.home.path()), &compact).unwrap();

    let out = f.run(&["harness", "scan"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    assert_eq!(
        f.config_bytes(),
        compact,
        "a scan that adds nothing re-saved config.json"
    );
}

/// The human scan line leaves out an empty part: nothing found is only "not found",
/// everything found is only "on PATH".
#[test]
fn scan_leaves_out_an_empty_part_of_the_line() {
    let f = fixture(&[]);
    base_config().save(f.home.path()).unwrap();
    let out = f.run(&["harness", "scan"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    assert_eq!(
        text(&out).0,
        "scan: claude, codex, kimi, opencode not found\n"
    );

    let f = fixture(&["claude", "codex", "kimi", "opencode"]);
    base_config().save(f.home.path()).unwrap();
    let out = f.run(&["harness", "scan"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    assert_eq!(
        text(&out).0,
        "scan: claude, codex, kimi, opencode on PATH\nadded opencode\n"
    );
}

/// `found` for a command that is a relative path with a separator (`bin/tool`) resolves
/// against the process cwd to the canonical absolute path; no PATH entry is consulted.
#[test]
fn list_resolves_a_relative_path_command_against_the_cwd() {
    let f = fixture(&[]);
    let work = tempfile::tempdir().unwrap();
    let tool = work.path().join("bin").join("tool");
    std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
    std::fs::write(&tool, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut cfg = base_config();
    cfg.harnesses.clear();
    cfg.harnesses.insert(
        "tool".into(),
        owlpost::config::Harness {
            cmd: vec!["bin/tool".into(), "{prompt}".into()],
            answer_path: "raw".into(),
            enabled: true,
            disabled_reason: None,
            env: Default::default(),
            model: None,
        },
    );
    cfg.save(f.home.path()).unwrap();

    let out = f
        .owl()
        .current_dir(work.path())
        .args(["--json", "harness", "list"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    let rows = json_of(&out);
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "tool")
        .expect("the tool row");
    let want = std::fs::canonicalize(&tool)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(row["found"], true, "{row}");
    assert_eq!(row["path"], json!(want), "{row}");
}

/// The keys no mutator touches: a non-default operator name, a project entry and a
/// non-default responder timeout.
fn assert_other_keys_survive(cfg: &Config) {
    assert_eq!(cfg.name, "Alex");
    assert_eq!(
        cfg.projects.get("owlpost").map(String::as_str),
        Some("/repo/owlpost")
    );
    assert_eq!(cfg.responder.timeout_secs, 42);
}

/// Every harness entry except `touched` is identical before and after (whole entries).
fn assert_other_harnesses_unchanged(before: &Config, after: &Config, touched: &str) {
    let mut before = before.harnesses.clone();
    let mut after = after.harnesses.clone();
    before.remove(touched);
    after.remove(touched);
    assert_eq!(before, after, "harnesses other than {touched} changed");
}

#[test]
fn add_edit_use_remove_change_config_json() {
    let f = fixture(&["claude", "gemini"]);
    let mut start = base_config();
    start.name = "Alex".into();
    start
        .projects
        .insert("owlpost".into(), "/repo/owlpost".into());
    start.responder.timeout_secs = 42;
    start.save(f.home.path()).unwrap();

    // add: defaults answer_path to raw; json is the harness's list row.
    let before = f.config();
    let out = f.run(&[
        "--json", "harness", "add", "gemini", "--", "gemini", "-p", "{prompt}",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    let cfg = f.config();
    let gemini = &cfg.harnesses["gemini"];
    assert_eq!(gemini.cmd, ["gemini", "-p", "{prompt}"]);
    assert_eq!(gemini.answer_path, "raw");
    assert!(gemini.enabled);
    assert_eq!(
        json_of(&out),
        list_row(&cfg, "gemini", Some(f.stub_path("gemini")))
    );
    assert_other_harnesses_unchanged(&before, &f.config(), "gemini");

    let before = f.config();
    let out = f.run(&[
        "harness",
        "add",
        "plain",
        "--answer-path",
        "result.text",
        "--",
        "plain",
        "run",
    ]);
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout, "added harness plain\n");
    assert_eq!(f.config().harnesses["plain"].answer_path, "result.text");
    assert_other_keys_survive(&f.config());
    assert_other_harnesses_unchanged(&before, &f.config(), "plain");

    // edit replaces the command only.
    let before = f.config();
    let out = f.run(&["harness", "edit", "gemini", "--", "gemini", "chat"]);
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout, "harness gemini now runs gemini chat\n");
    let gemini = &f.config().harnesses["gemini"];
    assert_eq!(gemini.cmd, ["gemini", "chat"]);
    assert_eq!(gemini.answer_path, "raw", "edit keeps the other fields");
    assert_other_keys_survive(&f.config());
    assert_other_harnesses_unchanged(&before, &f.config(), "gemini");

    // use points responder.harness at it; it touches no harness entry.
    let before = f.config();
    let out = f.run(&["--json", "harness", "use", "gemini"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(json_of(&out), json!({"name": "gemini", "drafting": true}));
    assert_eq!(f.config().responder.harness, "gemini");
    assert_other_keys_survive(&f.config());
    assert_eq!(f.config().harnesses, before.harnesses);
    let before = f.config();
    let out = f.run(&["harness", "use", "codex"]);
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout, "codex now drafts answers\n");
    assert_eq!(f.config().responder.harness, "codex");
    assert_eq!(f.config().harnesses, before.harnesses);

    // remove drops it from the table.
    let before = f.config();
    let out = f.run(&["--json", "harness", "remove", "gemini"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(json_of(&out), json!({"name": "gemini", "removed": true}));
    assert!(!f.config().harnesses.contains_key("gemini"));
    assert_other_keys_survive(&f.config());
    assert_other_harnesses_unchanged(&before, &f.config(), "gemini");
    let before = f.config();
    let out = f.run(&["harness", "remove", "plain"]);
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout, "removed harness plain\n");
    assert!(!f.config().harnesses.contains_key("plain"));
    assert_other_harnesses_unchanged(&before, &f.config(), "plain");
}

#[test]
fn errors_leave_config_json_untouched() {
    let f = fixture(&["claude"]);
    base_config().save(f.home.path()).unwrap();
    let before = f.config_bytes();

    let cases: &[(&[&str], &str)] = &[
        (
            &["harness", "add", "claude", "--", "claude"],
            "harness claude exists — use owl harness edit",
        ),
        (
            &["harness", "edit", "ghost", "--", "ghost"],
            "no harness ghost",
        ),
        (&["harness", "remove", "ghost"], "no harness ghost"),
        (&["harness", "use", "ghost"], "no harness ghost"),
        (
            &["harness", "remove", "claude"],
            "claude drafts answers — choose another with owl harness use first",
        ),
        (
            &["harness", "use", "kimi"],
            "harness kimi is disabled: read-only enforcement under -p unverified (see concept.md open questions)",
        ),
    ];
    for (args, message) in cases {
        let out = f.run(args);
        let (stdout, stderr) = text(&out);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {stdout}");
        assert!(stderr.contains(message), "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
        assert_eq!(f.config_bytes(), before, "{args:?} changed config.json");
    }
}

/// The raw config.json every mutator test below starts from: an unknown top-level key,
/// unknown nested keys (in `responder` and in one harness entry), defaults deliberately
/// absent (`outbox_ttl_days`, `notify`, `responder.redact`) and `memory_root` written as the
/// text of a symlink to `mem_target`.
fn raw_fixture(f: &Fixture, mem_target: &Path) -> Value {
    let link = f.home.path().join("mem");
    std::os::unix::fs::symlink(mem_target, &link).unwrap();
    let doc = json!({
        "name": "Alex",
        "future_key": {"a": 1},
        "projects": {"owlpost": "/repo/owlpost"},
        "responder": {
            "harness": "claude",
            "timeout_secs": 42,
            "unknown_resp": true,
            "memory_root": link.to_string_lossy(),
        },
        "harnesses": {
            "claude": {"cmd": ["claude", "-p", "{prompt}"], "answer_path": "result", "extra": "x"},
            "kimi": {"cmd": ["kimi", "{prompt}"], "answer_path": "raw", "enabled": false, "disabled_reason": "off"},
        },
    });
    std::fs::write(
        Config::path(f.home.path()),
        serde_json::to_vec_pretty(&doc).unwrap(),
    )
    .unwrap();
    doc
}

/// Why the doc comparisons below catch the old full rewrite: a `Config::save` of this home
/// would write out the absent defaults, drop the unknown keys and canonicalise `memory_root`.
fn assert_save_would_rewrite(f: &Fixture, before: &Value) {
    assert!(before.get("outbox_ttl_days").is_none(), "{before}");
    assert!(before.get("notify").is_none(), "{before}");
    assert!(before["responder"].get("redact").is_none(), "{before}");
    let typed = f.config();
    assert_eq!(typed.outbox_ttl_days, 14);
    assert!(typed.notify);
    assert_eq!(typed.responder.redact.len(), 2);
    let link = f.home.path().join("mem").to_string_lossy().into_owned();
    assert_eq!(before["responder"]["memory_root"], json!(link));
    assert_ne!(
        typed.responder.memory_root.as_deref(),
        Some(link.as_str()),
        "Config::load canonicalises the symlink, so a save would rewrite memory_root"
    );
}

/// Both docs minus the harness entries the command may touch (and, for `use`, the drafting
/// key): what remains must be equal — unknown keys survive, absent defaults stay absent and
/// memory_root keeps the link text. Equality is on the parsed values: the re-serialised file
/// is key-sorted, so bytes are not comparable.
fn assert_only_touched(before: &Value, after: &Value, harnesses: &[&str], drafting: bool) {
    let strip = |doc: &Value| {
        let mut doc = doc.clone();
        for name in harnesses {
            doc["harnesses"].as_object_mut().unwrap().remove(*name);
        }
        if drafting {
            doc["responder"].as_object_mut().unwrap().remove("harness");
        }
        doc
    };
    assert_eq!(strip(after), strip(before));
}

/// The home holds exactly config.json and the memory_root symlink: no tmp files left behind.
fn assert_home_clean(f: &Fixture) {
    let mut names: Vec<String> = std::fs::read_dir(f.home.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["config.json", "mem"]);
}

#[test]
fn add_changes_only_the_new_harness_key() {
    let f = fixture(&["gemini"]);
    let mem = tempfile::tempdir().unwrap();
    let before = raw_fixture(&f, mem.path());
    assert_save_would_rewrite(&f, &before);

    let out = f.run(&["harness", "add", "gemini", "--", "gemini", "-p", "{prompt}"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);

    let after = f.config_json();
    assert_eq!(
        after["harnesses"]["gemini"],
        json!({"cmd": ["gemini", "-p", "{prompt}"], "answer_path": "raw", "enabled": true})
    );
    assert_only_touched(&before, &after, &["gemini"], false);
    assert_home_clean(&f);
}

#[test]
fn edit_changes_only_the_cmd_key() {
    let f = fixture(&["claude"]);
    let mem = tempfile::tempdir().unwrap();
    let before = raw_fixture(&f, mem.path());
    assert_save_would_rewrite(&f, &before);

    let out = f.run(&["harness", "edit", "claude", "--", "claude", "chat"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);

    let after = f.config_json();
    let mut want = before["harnesses"]["claude"].clone();
    want["cmd"] = json!(["claude", "chat"]);
    assert_eq!(
        after["harnesses"]["claude"], want,
        "edit kept the entry's other keys, including the unknown extra"
    );
    assert_only_touched(&before, &after, &["claude"], false);
    assert_home_clean(&f);
}

#[test]
fn remove_deletes_only_the_harness_key() {
    let f = fixture(&[]);
    let mem = tempfile::tempdir().unwrap();
    let before = raw_fixture(&f, mem.path());
    assert_save_would_rewrite(&f, &before);
    assert!(
        before["harnesses"].get("kimi").is_some(),
        "the precondition: kimi is in the table"
    );

    let out = f.run(&["harness", "remove", "kimi"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);

    let after = f.config_json();
    assert!(after["harnesses"].get("kimi").is_none(), "{after}");
    assert_only_touched(&before, &after, &["kimi"], false);
    assert_home_clean(&f);
}

#[test]
fn use_changes_only_responder_harness() {
    let f = fixture(&[]);
    let mem = tempfile::tempdir().unwrap();
    let mut before = raw_fixture(&f, mem.path());
    before["harnesses"]["codex"] = json!({"cmd": ["codex", "{prompt}"], "answer_path": "raw"});
    std::fs::write(
        Config::path(f.home.path()),
        serde_json::to_vec_pretty(&before).unwrap(),
    )
    .unwrap();
    assert_save_would_rewrite(&f, &before);
    assert_eq!(
        before["responder"]["harness"], "claude",
        "the precondition: codex does not draft yet"
    );

    let out = f.run(&["harness", "use", "codex"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);

    let after = f.config_json();
    assert_eq!(after["responder"]["harness"], "codex");
    assert_only_touched(&before, &after, &[], true);
    assert_home_clean(&f);
}

#[test]
fn scan_adds_only_the_added_keys() {
    let f = fixture(&["codex"]);
    let mem = tempfile::tempdir().unwrap();
    let before = raw_fixture(&f, mem.path());
    assert_save_would_rewrite(&f, &before);

    let out = f.run(&["harness", "scan"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);

    let after = f.config_json();
    assert_eq!(
        after["harnesses"]["codex"],
        serde_json::to_value(&Config::default().harnesses["codex"]).unwrap()
    );
    assert_only_touched(&before, &after, &["codex"], false);
    assert_home_clean(&f);
}

/// A file without `harnesses` has the `#[serde(default)]` table in effect; `add` materialises
/// it, so the default harnesses stay effective alongside the new one.
#[test]
fn add_without_a_harnesses_key_keeps_the_defaults() {
    let f = fixture(&[]);
    let mem = tempfile::tempdir().unwrap();
    let mut before = raw_fixture(&f, mem.path());
    before.as_object_mut().unwrap().remove("harnesses");
    std::fs::write(
        Config::path(f.home.path()),
        serde_json::to_vec_pretty(&before).unwrap(),
    )
    .unwrap();
    assert_eq!(
        f.config().harnesses,
        Config::default().harnesses,
        "the default table is in effect"
    );

    let out = f.run(&["harness", "add", "x", "--", "x", "run"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);

    let after = f.config_json();
    let mut want = serde_json::to_value(&Config::default().harnesses).unwrap();
    want["x"] = json!({"cmd": ["x", "run"], "answer_path": "raw", "enabled": true});
    assert_eq!(
        after["harnesses"], want,
        "claude, codex, ... stay effective"
    );
    let mut after_rest = after.clone();
    after_rest.as_object_mut().unwrap().remove("harnesses");
    assert_eq!(after_rest, before, "nothing outside harnesses changed");
    assert_home_clean(&f);
}

/// A home with a key but no `config.json` is not an error: the missing file reads as empty,
/// so `add` creates it holding nothing but the harness table — the default entries plus the
/// new one — and nothing else appears in the home.
#[test]
fn add_on_a_home_without_config_json_creates_it_from_the_defaults() {
    let f = fixture_without_config(&[]);
    assert_eq!(
        home_files(&f),
        ["key"],
        "precondition: an identity, but no config.json"
    );

    let out = f.run(&["harness", "add", "mine", "--", "mytool"]);
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(stdout, "added harness mine\n");

    let after = f.config_json();
    assert_eq!(
        after.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["harnesses"],
        "the created config.json holds only the harness table: {after}"
    );
    let mut want = serde_json::to_value(&Config::default().harnesses).unwrap();
    want["mine"] = json!({"cmd": ["mytool"], "answer_path": "raw", "enabled": true});
    assert_eq!(
        after["harnesses"], want,
        "the default harnesses plus mine: {after}"
    );
    assert_eq!(
        home_files(&f),
        ["config.json", "key"],
        "only config.json appeared in the home"
    );
}
