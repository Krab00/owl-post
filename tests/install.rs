//! `install --dry-run` prints the unit and writes nothing; `install` / `uninstall`
//! create and remove the unit file under `$HOME` with loading skipped).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn owl(home: &Path, user_home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
    c.env_remove("OWLPOST_HOME")
        .env_remove("OWLPOST_INSTALL_NO_LOAD")
        .env("HOME", user_home)
        .arg("--home")
        .arg(home);
    c
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// Where the current OS keeps the unit under `user_home`.
fn unit_path(user_home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        user_home.join("Library/LaunchAgents/dev.owlpost.owl.plist")
    } else {
        user_home.join(".config/systemd/user/owlpost.service")
    }
}

fn owl_binary() -> String {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_owl"))
        .unwrap()
        .display()
        .to_string()
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(files_under(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn assert_unit(unit: &str, home: &Path) {
    assert!(unit.contains(&owl_binary()), "absolute owl path:\n{unit}");
    assert!(unit.contains("daemon"), "{unit}");
    assert!(unit.contains("--home"), "{unit}");
    assert!(unit.contains(&home.display().to_string()), "{unit}");
    if cfg!(target_os = "macos") {
        assert!(unit.starts_with("<?xml"), "{unit}");
        assert!(unit.contains("<string>dev.owlpost.owl</string>"), "{unit}");
        assert!(unit.contains(&format!(
            "<string>{}</string>\n        <string>daemon</string>\n        <string>--home</string>\n        <string>{}</string>",
            owl_binary(),
            home.display()
        )));
    } else {
        assert!(unit.starts_with("[Unit]"), "{unit}");
        assert!(unit.contains(&format!(
            "ExecStart=\"{}\" daemon --home \"{}\"",
            owl_binary(),
            home.display()
        )));
        assert!(unit.contains("WantedBy=default.target"), "{unit}");
    }
}

#[test]
fn install_dry_run_prints_unit_and_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let out = owl(home.path(), user_home.path())
        .args(["install", "--dry-run"])
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stderr: {stderr}");
    assert_unit(&stdout, home.path());
    assert!(
        !stdout.contains("installed"),
        "dry run must not report a write"
    );
    assert!(
        files_under(user_home.path()).is_empty(),
        "dry run wrote {:?}",
        files_under(user_home.path())
    );
    assert!(!unit_path(user_home.path()).exists());
    assert!(
        files_under(home.path()).is_empty(),
        "dry run touched the owlpost home"
    );
}

#[test]
fn install_dry_run_uses_the_given_home() {
    // Two different homes produce two different units: the flag is not ignored.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let unit_a = text(
        &owl(a.path(), user_home.path())
            .args(["install", "--dry-run"])
            .output()
            .unwrap(),
    )
    .0;
    let unit_b = text(
        &owl(b.path(), user_home.path())
            .args(["install", "--dry-run"])
            .output()
            .unwrap(),
    )
    .0;
    assert_ne!(unit_a, unit_b);
    assert!(unit_a.contains(&a.path().display().to_string()));
    assert!(!unit_a.contains(&b.path().display().to_string()));
    assert!(unit_b.contains(&b.path().display().to_string()));
    // A relative --home is made absolute in the unit.
    let cwd = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_owl"))
        .env_remove("OWLPOST_HOME")
        .env("HOME", user_home.path())
        .current_dir(cwd.path())
        .args(["--home", "rel-home", "install", "--dry-run"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let unit = text(&out).0;
    assert!(
        unit.contains(&cwd.path().join("rel-home").display().to_string()),
        "{unit}"
    );
}

#[test]
fn install_then_uninstall_creates_and_removes_unit_file() {
    let home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let unit = unit_path(user_home.path());

    let out = owl(home.path(), user_home.path())
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("install")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "install: {stderr}");
    assert!(unit.is_file(), "unit file missing at {}", unit.display());
    assert!(
        stdout.contains(&format!("installed {}", unit.display())),
        "{stdout}"
    );
    assert!(stdout.contains("load skipped"), "{stdout}");
    assert!(!stdout.contains("started"), "{stdout}");
    assert!(
        !unit.with_extension("tmp").exists(),
        "temp file left behind"
    );
    let content = std::fs::read_to_string(&unit).unwrap();
    assert_unit(&content, home.path());
    // The written unit equals the dry-run output.
    let dry = text(
        &owl(home.path(), user_home.path())
            .args(["install", "--dry-run"])
            .output()
            .unwrap(),
    )
    .0;
    assert_eq!(content, dry);

    // Installing again overwrites in place (a different home lands in the file).
    let home2 = tempfile::tempdir().unwrap();
    let out = owl(home2.path(), user_home.path())
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("install")
        .output()
        .unwrap();
    assert!(out.status.success());
    let content = std::fs::read_to_string(&unit).unwrap();
    assert!(content.contains(&home2.path().display().to_string()));
    assert!(!content.contains(&home.path().display().to_string()));

    let out = owl(home.path(), user_home.path())
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("uninstall")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "uninstall: {stderr}");
    assert!(!unit.exists(), "unit file still present");
    assert!(stdout.contains("stop skipped"), "{stdout}");
    assert!(
        stdout.contains(&format!("removed {}", unit.display())),
        "{stdout}"
    );

    // Uninstalling again is a no-op that says so.
    let out = owl(home.path(), user_home.path())
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("uninstall")
        .output()
        .unwrap();
    let (stdout, _) = text(&out);
    assert!(out.status.success());
    assert!(stdout.contains("not installed"), "{stdout}");
}

/// A second `owl install` with the same binary and home leaves the unit file as it is.
#[test]
fn install_with_an_unchanged_unit_does_not_rewrite_it() {
    let home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let unit = unit_path(user_home.path());
    let install = || {
        let out = owl(home.path(), user_home.path())
            .env("OWLPOST_INSTALL_NO_LOAD", "1")
            .arg("install")
            .output()
            .unwrap();
        let (stdout, stderr) = text(&out);
        assert!(out.status.success(), "install: {stderr}");
        stdout
    };
    install();
    let written = std::fs::read_to_string(&unit).unwrap();
    let modified = std::fs::metadata(&unit).unwrap().modified().unwrap();
    let stdout = install();
    assert!(
        stdout.contains(&format!("unchanged {}\n", unit.display())),
        "{stdout}"
    );
    assert!(stdout.contains("load skipped"), "{stdout}");
    assert!(!stdout.contains("installed"), "{stdout}");
    assert_eq!(std::fs::read_to_string(&unit).unwrap(), written);
    assert_eq!(
        std::fs::metadata(&unit).unwrap().modified().unwrap(),
        modified
    );
}

#[test]
fn install_blocked_target_is_a_clean_error() {
    let home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let unit = unit_path(user_home.path());
    std::fs::create_dir_all(unit.join("child")).unwrap();
    let out = owl(home.path(), user_home.path())
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("install")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr.contains("owl: "), "{stderr}");
    assert!(stderr.contains("renaming to"), "{stderr}");
    assert!(!stdout.contains("installed"), "{stdout}");
    assert!(unit.is_dir(), "blocker untouched");
    assert!(!unit.with_extension("tmp").exists(), "temp file cleaned up");
}

#[test]
fn install_without_home_env_fails() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_owl"))
        .env_remove("OWLPOST_HOME")
        .env_remove("HOME")
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("--home")
        .arg(home.path())
        .arg("install")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out).1.contains("HOME is not set"), "{}", text(&out).1);
    // dry-run needs no HOME at all.
    let out = Command::new(env!("CARGO_BIN_EXE_owl"))
        .env_remove("OWLPOST_HOME")
        .env_remove("HOME")
        .arg("--home")
        .arg(home.path())
        .args(["install", "--dry-run"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out).1);
}

/// A fake `systemctl` / `launchctl` / `id` on a temp PATH, each appending its argv to `log`.
fn fake_service_manager(dir: &Path, exit_code: u8) -> (PathBuf, PathBuf) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.join("calls.log");
    let write_exe = |name: &str, body: String| {
        let script = bin.join(name);
        std::fs::write(&script, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    };
    for name in ["systemctl", "launchctl"] {
        write_exe(
            name,
            format!(
                "#!/bin/sh\necho \"{name} $*\" >> '{}'\nexit {exit_code}\n",
                log.display()
            ),
        );
    }
    write_exe("id", "#!/bin/sh\necho 501\n".into());
    (bin, log)
}

fn with_fake_path(mut cmd: Command, bin: &Path) -> Command {
    let mut paths = vec![bin.to_path_buf()];
    if let Some(p) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&p));
    }
    cmd.env("PATH", std::env::join_paths(paths).unwrap());
    cmd
}

fn calls(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn install_and_uninstall_drive_the_service_manager() {
    let home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (bin, log) = fake_service_manager(scratch.path(), 0);
    let unit = unit_path(user_home.path());

    let out = with_fake_path(owl(home.path(), user_home.path()), &bin)
        .arg("install")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "install: {stderr}");
    assert!(unit.is_file());
    assert!(stdout.contains("installed "), "{stdout}");
    assert!(stdout.contains("started "), "{stdout}");
    assert!(!stdout.contains("skipped"), "{stdout}");
    let expected_install: Vec<String> = if cfg!(target_os = "macos") {
        vec![format!("launchctl bootstrap gui/501 {}", unit.display())]
    } else {
        vec![
            "systemctl --user daemon-reload".into(),
            "systemctl --user enable --now owlpost.service".into(),
        ]
    };
    assert_eq!(calls(&log), expected_install);

    let out = with_fake_path(owl(home.path(), user_home.path()), &bin)
        .arg("uninstall")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "uninstall: {stderr}");
    assert!(!unit.exists());
    assert!(stdout.contains("stopped "), "{stdout}");
    assert!(stdout.contains("removed "), "{stdout}");
    let mut expected_all = expected_install.clone();
    if cfg!(target_os = "macos") {
        expected_all.push("launchctl bootout gui/501/dev.owlpost.owl".into());
    } else {
        expected_all.push("systemctl --user disable --now owlpost.service".into());
        expected_all.push("systemctl --user daemon-reload".into());
    }
    assert_eq!(calls(&log), expected_all);
}

#[test]
fn service_manager_failure_is_reported() {
    let home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (bin, log) = fake_service_manager(scratch.path(), 3);
    let unit = unit_path(user_home.path());

    let out = with_fake_path(owl(home.path(), user_home.path()), &bin)
        .arg("install")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(unit.is_file(), "the unit is written before loading");
    assert!(stdout.contains("installed "), "{stdout}");
    assert!(!stdout.contains("started"), "{stdout}");
    if cfg!(target_os = "macos") {
        // bootstrap failed → the `load -w` fallback ran and failed too.
        assert_eq!(calls(&log).len(), 2, "{:?}", calls(&log));
        assert!(stderr.contains("launchctl load -w"), "{stderr}");
    } else {
        assert_eq!(calls(&log), ["systemctl --user daemon-reload"]);
        assert!(
            stderr.contains("systemctl --user daemon-reload failed"),
            "{stderr}"
        );
    }

    // Uninstall with a failing manager: reports the error, keeps the unit file.
    let out = with_fake_path(owl(home.path(), user_home.path()), &bin)
        .arg("uninstall")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(unit.is_file(), "unit kept when stopping fails");
    assert!(!stdout.contains("removed"), "{stdout}");
    assert!(stderr.contains("failed"), "{stderr}");
}

/// `owl stop` / `owl start` drive the service manager and keep the unit file.
#[test]
fn stop_and_start_drive_the_service_manager_and_keep_the_unit() {
    let home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (bin, log) = fake_service_manager(scratch.path(), 0);
    let unit = unit_path(user_home.path());
    let out = owl(home.path(), user_home.path())
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("install")
        .output()
        .unwrap();
    assert!(out.status.success(), "install: {}", text(&out).1);

    let out = with_fake_path(owl(home.path(), user_home.path()), &bin)
        .arg("stop")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stop: {stderr}");
    assert_eq!(stdout, "stopped owl daemon\n");
    assert!(unit.is_file(), "stop keeps the unit");
    let mut expected: Vec<String> = if cfg!(target_os = "macos") {
        vec![
            "launchctl print gui/501/dev.owlpost.owl".into(),
            "launchctl bootout gui/501/dev.owlpost.owl".into(),
        ]
    } else {
        vec!["systemctl --user stop owlpost.service".into()]
    };
    assert_eq!(calls(&log), expected);

    let out = with_fake_path(owl(home.path(), user_home.path()), &bin)
        .arg("start")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "start: {stderr}");
    assert_eq!(stdout, "started owl daemon\n");
    assert!(unit.is_file());
    expected.push(if cfg!(target_os = "macos") {
        "launchctl kickstart gui/501/dev.owlpost.owl".into()
    } else {
        "systemctl --user enable --now owlpost.service".into()
    });
    assert_eq!(calls(&log), expected);
}

/// A failing manager: `stop` of a service launchd does not know is a no-op on macOS; `start`
/// falls back to loading the unit and reports that failure.
#[test]
fn stop_and_start_with_a_failing_service_manager() {
    let home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (bin, log) = fake_service_manager(scratch.path(), 3);
    let unit = unit_path(user_home.path());
    let out = owl(home.path(), user_home.path())
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        .arg("install")
        .output()
        .unwrap();
    assert!(out.status.success(), "install: {}", text(&out).1);

    let out = with_fake_path(owl(home.path(), user_home.path()), &bin)
        .arg("stop")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    if cfg!(target_os = "macos") {
        assert!(out.status.success(), "{stderr}");
        assert_eq!(stdout, "stopped owl daemon\n");
        assert_eq!(calls(&log), ["launchctl print gui/501/dev.owlpost.owl"]);
    } else {
        assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
        assert!(
            stderr.contains("systemctl --user stop owlpost.service failed"),
            "{stderr}"
        );
    }

    let out = with_fake_path(owl(home.path(), user_home.path()), &bin)
        .arg("start")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(!stdout.contains("started"), "{stdout}");
    assert!(stderr.contains("failed"), "{stderr}");
    assert!(unit.is_file());
}

/// No unit: `stop` says so and succeeds, `start` fails pointing at `owl install`; neither
/// touches the service manager or writes anything.
#[test]
fn stop_and_start_without_a_service_installed() {
    let home = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (bin, log) = fake_service_manager(scratch.path(), 0);
    let unit = unit_path(user_home.path());

    let out = with_fake_path(owl(home.path(), user_home.path()), &bin)
        .arg("stop")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(
        stdout,
        format!(
            "no owl daemon service installed ({} is absent)\n",
            unit.display()
        )
    );

    let out = with_fake_path(owl(home.path(), user_home.path()), &bin)
        .arg("start")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains("no owl daemon service installed"),
        "{stderr}"
    );
    assert!(stderr.contains("run owl install"), "{stderr}");
    assert!(calls(&log).is_empty(), "{:?}", calls(&log));
    assert!(files_under(user_home.path()).is_empty());
}
