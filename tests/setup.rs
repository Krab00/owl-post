//! `owl setup` and `owl update` end their Claude Code steps with the
//! idempotent user-scope registration of the `owl` MCP server. A fake `claude` (and `cargo`)
//! on a PATH of its own logs every argv; `claude mcp get owl` exits per case.

mod common;

#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use common::{fake_calls, fake_claude};

const MCP_GET: &str = "claude mcp get owl";
#[cfg(target_os = "linux")]
const MCP_ADD: &str = "claude mcp add --scope user owl -- owl mcp";

/// `owl` with `$OWLPOST_HOME` = `home`, `HOME` = `user_home` (the systemd unit lands there,
/// `OWLPOST_INSTALL_NO_LOAD` keeps `systemctl` out of it) and `PATH` = exactly `bin`.
fn owl(home: &Path, user_home: &Path, bin: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_owl"));
    c.env_remove("OWLPOST_HOME")
        .env("HOME", user_home)
        .env("OWLPOST_INSTALL_NO_LOAD", "1")
        // `owl update` replaces this file instead of the test binary.
        .env("OWLPOST_UPDATE_ACTIVE", user_home.join("active-owl"))
        .env("PATH", bin)
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

/// One cell of the matrix: runs `args` with a fake `claude` whose `mcp get owl` exits
/// `get_exit`; returns (stdout, log lines).
fn run(args: &[&str], get_exit: i32) -> (String, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("owlpost");
    let user_home = dir.path().join("user");
    std::fs::create_dir_all(&user_home).unwrap();
    let (bin, log) = fake_claude(dir.path(), get_exit);
    let active = user_home.join("active-owl");
    std::fs::write(&active, "old owl\n").unwrap();
    let out = owl(&home, &user_home, &bin).args(args).output().unwrap();
    let (stdout, stderr) = text(&out);
    assert!(
        out.status.success(),
        "{args:?}: exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    if !args.contains(&"--dry-run") {
        // The unit runs the path the caller passed — `owl setup` the canonical test
        // binary, `owl update` the active file it replaced (not the test binary running it).
        let program = if args[0] == "setup" {
            std::fs::canonicalize(env!("CARGO_BIN_EXE_owl")).unwrap()
        } else {
            active.clone()
        };
        assert_unit_runs(&user_home, &program, &home);
    }
    if args[0] == "update" && !args.contains(&"--dry-run") {
        // The active file now holds the fake build, and the line says so.
        assert_eq!(std::fs::read_to_string(&active).unwrap(), "fake owl\n");
        assert!(
            stdout.contains(&format!("installed {}\n", active.display())),
            "{stdout}"
        );
    } else {
        assert_eq!(std::fs::read_to_string(&active).unwrap(), "old owl\n");
    }
    // The build root is a fresh temp dir per run: strip it so the calls compare exactly.
    let calls = fake_calls(&log)
        .into_iter()
        .map(|c| c.split(" --root ").next().unwrap().to_string())
        .collect();
    (stdout, calls)
}

#[cfg(target_os = "linux")]
const SETUP_PLUGIN: [&str; 2] = [
    "claude plugin marketplace add /repo/plugins/claude-code",
    "claude plugin install owlpost@owlpost-local --scope user",
];
#[cfg(target_os = "linux")]
const UPDATE_PLUGIN: [&str; 3] = [
    "claude plugin marketplace update owlpost-local",
    "claude plugin uninstall owlpost@owlpost-local",
    "claude plugin install owlpost@owlpost-local --scope user",
];

#[cfg(target_os = "linux")]
/// `claude --version` (the PATH probe) runs first, then the binary step (`owl update`
/// only), the plugin steps, and the `mcp` tail.
fn expected(binary: &[&str], plugin: &[&str], tail: &[&str]) -> Vec<String> {
    std::iter::once("claude --version")
        .chain(binary.iter().copied())
        .chain(plugin.iter().copied())
        .chain(tail.iter().copied())
        .map(str::to_string)
        .collect()
}

#[cfg(target_os = "linux")]
const UPDATE_BINARY: [&str; 1] = ["cargo install --path /repo --locked"];

/// The path a `would replace <path>` line names (the `OWLPOST_UPDATE_ACTIVE` file).
fn stdout_active(stdout: &str) -> String {
    let line = stdout
        .lines()
        .find(|l| l.starts_with("would replace "))
        .unwrap_or_else(|| panic!("no would replace line in {stdout}"));
    assert!(line.ends_with("/user/active-owl"), "{line}");
    line["would replace ".len()..].to_string()
}

const SETUP: [&str; 3] = ["setup", "--plugin-source", "/repo/plugins/claude-code"];
const UPDATE: [&str; 3] = ["update", "--source", "/repo"];

#[cfg(target_os = "linux")] // reads the systemd unit
#[test]
fn setup_registers_owl_mcp_after_the_plugin_steps() {
    let (stdout, calls) = run(&SETUP, 1);
    assert_eq!(
        calls,
        expected(&[], &SETUP_PLUGIN, &[MCP_GET, MCP_ADD]),
        "{stdout}"
    );
    assert!(stdout.contains(&format!("+ {MCP_ADD}\n")), "{stdout}");
    assert!(!stdout.contains("already registered"), "{stdout}");
    // The identity and the daemon unit were created before the Claude Code steps.
    assert!(stdout.contains("installed "), "{stdout}");
}

#[cfg(target_os = "linux")] // reads the systemd unit
#[test]
fn setup_skips_registration_when_owl_mcp_is_registered() {
    let (stdout, calls) = run(&SETUP, 0);
    assert_eq!(calls, expected(&[], &SETUP_PLUGIN, &[MCP_GET]), "{stdout}");
    assert!(
        stdout.contains("mcp server owl already registered\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("mcp add"), "{stdout}");
}

#[test]
fn setup_dry_run_prints_the_registration_and_runs_nothing() {
    let (stdout, calls) = run(&[SETUP[0], SETUP[1], SETUP[2], "--dry-run"], 1);
    // Only the read-only probes ran: no plugin step, no `mcp add`.
    assert_eq!(calls, ["claude --version", MCP_GET], "{stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        [
            "would run: owl init",
            "would run: owl install",
            "would run: claude plugin marketplace add /repo/plugins/claude-code",
            "would run: claude plugin install owlpost@owlpost-local --scope user",
            "would run: claude mcp add --scope user owl -- owl mcp",
        ]
    );
    let (stdout, calls) = run(&[SETUP[0], SETUP[1], SETUP[2], "--dry-run"], 0);
    assert_eq!(calls, ["claude --version", MCP_GET], "{stdout}");
    assert!(
        stdout.ends_with("mcp server owl already registered\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("mcp add"), "{stdout}");
}

#[cfg(target_os = "linux")] // reads the systemd unit
#[test]
fn update_registers_owl_mcp_after_the_plugin_steps() {
    let (stdout, calls) = run(&UPDATE, 1);
    assert_eq!(
        calls,
        expected(&UPDATE_BINARY, &UPDATE_PLUGIN, &[MCP_GET, MCP_ADD]),
        "{stdout}"
    );
    assert!(stdout.contains(&format!("+ {MCP_ADD}\n")), "{stdout}");
    assert!(!stdout.contains("already registered"), "{stdout}");
}

#[cfg(target_os = "linux")] // reads the systemd unit
#[test]
fn update_skips_registration_when_owl_mcp_is_registered() {
    let (stdout, calls) = run(&UPDATE, 0);
    assert_eq!(
        calls,
        expected(&UPDATE_BINARY, &UPDATE_PLUGIN, &[MCP_GET]),
        "{stdout}"
    );
    assert!(
        stdout.contains("mcp server owl already registered\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("mcp add"), "{stdout}");
}

#[test]
fn update_dry_run_prints_the_registration_and_runs_nothing() {
    let (stdout, calls) = run(&[UPDATE[0], UPDATE[1], UPDATE[2], "--dry-run"], 1);
    assert_eq!(calls, ["claude --version", MCP_GET], "{stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        [
            "would run: cargo install --path /repo --locked --root <tmpdir>",
            &format!("would replace {}", stdout_active(&stdout)),
            "would run: owl uninstall && owl install",
            "would run: claude plugin marketplace update owlpost-local",
            "would run: claude plugin uninstall owlpost@owlpost-local",
            "would run: claude plugin install owlpost@owlpost-local --scope user",
            "would run: claude mcp add --scope user owl -- owl mcp",
        ]
    );
    let (stdout, calls) = run(&[UPDATE[0], UPDATE[1], UPDATE[2], "--dry-run"], 0);
    assert_eq!(calls, ["claude --version", MCP_GET], "{stdout}");
    assert!(
        stdout.ends_with("mcp server owl already registered\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("mcp add"), "{stdout}");
}

/// Without `claude` the registration is skipped with the plugin steps, in both modes.
#[test]
fn no_claude_on_path_skips_the_registration() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("owlpost");
    let user_home = dir.path().join("user");
    std::fs::create_dir_all(&user_home).unwrap();
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    for args in [&SETUP[..], &UPDATE[..]] {
        let out = owl(&home, &user_home, &empty)
            .args(args)
            .arg("--dry-run")
            .output()
            .unwrap();
        let (stdout, stderr) = text(&out);
        assert!(out.status.success(), "{args:?}: {stderr}");
        assert!(stdout.contains("claude not on PATH"), "{stdout}");
        assert!(!stdout.contains("mcp"), "{stdout}");
    }
}

/// A temp layout for one `owl update` run: `(dir, home, user_home, bin, log)` with the fake
/// `claude`/`cargo` on `bin`; the caller sets `OWLPOST_UPDATE_ACTIVE`.
fn update_layout(
    get_exit: i32,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("owlpost");
    let user_home = dir.path().join("user");
    std::fs::create_dir_all(&user_home).unwrap();
    let (bin, log) = fake_claude(dir.path(), get_exit);
    (dir, home, user_home, bin, log)
}

/// The systemd unit under `user_home` has exactly `ExecStart="<program>" daemon --home "<home>"`.
fn assert_unit_runs(user_home: &Path, program: &Path, home: &Path) {
    let unit = user_home.join(".config/systemd/user/owlpost.service");
    let unit_text = std::fs::read_to_string(&unit).unwrap();
    let exec = unit_text
        .lines()
        .find(|l| l.starts_with("ExecStart="))
        .unwrap_or_else(|| panic!("no ExecStart in:\n{unit_text}"));
    assert_eq!(
        exec,
        format!(
            "ExecStart=\"{}\" daemon --home \"{}\"",
            program.display(),
            home.display()
        ),
        "unit:\n{unit_text}"
    );
    assert!(!unit_text.contains("(deleted)"), "unit:\n{unit_text}");
}

/// No daemon unit was written under `user_home` (neither the systemd nor the launchd path).
fn assert_no_unit(user_home: &Path) {
    assert!(!user_home.join(".config").exists(), "systemd unit written");
    assert!(!user_home.join("Library").exists(), "launchd plist written");
}

/// End to end: the active file is a copied `/bin/sleep` that a child is
/// executing while `owl update --source` runs; the child survives, the path holds the
/// built bytes with mode 0755, no `.new` is left, and `installed <active>` is printed
/// before the unit and plugin steps.
#[cfg(target_os = "linux")] // reads the systemd unit
#[test]
fn update_replaces_a_running_active_binary_end_to_end() {
    let (_dir, home, user_home, bin, log) = update_layout(0);
    let active = user_home.join("active-owl");
    std::fs::copy("/bin/sleep", &active).unwrap();
    std::fs::set_permissions(&active, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut child = loop {
        match Command::new(&active).arg("30").spawn() {
            Ok(c) => break c,
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => panic!("spawning the sleep copy: {e}"),
        }
    };
    let out = owl(&home, &user_home, &bin)
        .env("OWLPOST_UPDATE_ACTIVE", &active)
        .args(UPDATE)
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        child.try_wait().unwrap().is_none(),
        "the running child is unaffected"
    );
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(std::fs::read_to_string(&active).unwrap(), "fake owl\n");
    let mode = std::fs::metadata(&active).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755, "mode {mode:o}");
    assert!(!active.with_extension("new").exists(), ".new left behind");
    // The unit names the replaced file, not the test binary that ran the update.
    assert_unit_runs(&user_home, &active, &home);
    let installed = format!("installed {}\n", active.display());
    assert!(stdout.contains(&installed), "{stdout}");
    // The replacement is printed before the unit reinstall and the plugin steps.
    let at = stdout.find(&installed).unwrap();
    assert!(stdout[at..].contains("+ claude plugin install"), "{stdout}");
    assert!(!stdout[..at].contains("claude plugin"), "{stdout}");
    let calls = fake_calls(&log);
    assert!(
        calls[1].starts_with("cargo install --path /repo --locked --root "),
        "{calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with("claude plugin install")),
        "{calls:?}"
    );
}

/// The build step produced no file: a hard error naming the missing built path, the active
/// file untouched, and neither the unit steps nor the plugin steps run.
#[test]
fn update_without_a_built_file_stops_before_the_unit_and_plugin_steps() {
    let (_dir, home, user_home, bin, log) = update_layout(0);
    // A `cargo` that logs and exits 0 without writing `<root>/bin/owl`.
    std::fs::write(
        bin.join("cargo"),
        format!(
            "#!/bin/sh\necho \"cargo $*\" >> '{}'\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    let active = user_home.join("active-owl");
    std::fs::write(&active, "old owl\n").unwrap();
    let out = owl(&home, &user_home, &bin)
        .env("OWLPOST_UPDATE_ACTIVE", &active)
        .args(UPDATE)
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}\n{stderr}");
    assert!(stderr.contains("did not produce "), "{stderr}");
    assert!(stderr.contains("/bin/owl"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&active).unwrap(), "old owl\n");
    assert!(!stdout.contains("installed "), "{stdout}");
    assert_no_unit(&user_home);
    let calls = fake_calls(&log);
    assert_eq!(
        calls.len(),
        2,
        "only the probe and the build ran: {calls:?}"
    );
    assert_eq!(calls[0], "claude --version");
    assert!(calls[1].starts_with("cargo install "), "{calls:?}");
}

/// The replacement itself fails (the active path's directory does not exist): the error
/// names the copy, and the unit reinstall and plugin steps never run.
#[test]
fn update_with_a_failed_replacement_runs_no_unit_or_plugin_step() {
    let (_dir, home, user_home, bin, log) = update_layout(0);
    let active = user_home.join("no-such-dir").join("owl");
    let out = owl(&home, &user_home, &bin)
        .env("OWLPOST_UPDATE_ACTIVE", &active)
        .args(UPDATE)
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}\n{stderr}");
    assert!(stderr.contains("copying "), "{stderr}");
    assert!(
        stderr.contains(&active.with_extension("new").display().to_string()),
        "{stderr}"
    );
    assert!(!stdout.contains("installed "), "{stdout}");
    assert_no_unit(&user_home);
    let calls = fake_calls(&log);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(
        calls
            .iter()
            .all(|c| !c.starts_with("claude plugin") && !c.starts_with("claude mcp")),
        "{calls:?}"
    );
}

/// A second `owl update` with the same active path and home finds the unit unchanged: it is
/// neither removed nor rewritten, only the service restart is attempted (skipped here).
/// A unit with other text is uninstalled and installed as before.
#[test]
fn update_restarts_an_unchanged_unit_instead_of_reinstalling_it() {
    let (_dir, home, user_home, bin, _log) = update_layout(0);
    let unit = if cfg!(target_os = "macos") {
        user_home.join("Library/LaunchAgents/dev.owlpost.owl.plist")
    } else {
        user_home.join(".config/systemd/user/owlpost.service")
    };
    let update = || {
        let out = owl(&home, &user_home, &bin).args(UPDATE).output().unwrap();
        let (stdout, stderr) = text(&out);
        assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
        stdout
    };
    let first = update();
    assert!(
        first.contains(&format!("installed {}\n", unit.display())),
        "{first}"
    );
    let written = std::fs::read_to_string(&unit).unwrap();
    let modified = std::fs::metadata(&unit).unwrap().modified().unwrap();

    let second = update();
    assert!(
        second.contains(&format!("unchanged {}\n", unit.display())),
        "{second}"
    );
    assert!(
        second.contains("restart skipped (OWLPOST_INSTALL_NO_LOAD is set)\n"),
        "{second}"
    );
    assert!(!second.contains("removed "), "{second}");
    assert!(
        !second.contains(&format!("installed {}\n", unit.display())),
        "{second}"
    );
    assert_eq!(std::fs::read_to_string(&unit).unwrap(), written);
    assert_eq!(
        std::fs::metadata(&unit).unwrap().modified().unwrap(),
        modified
    );

    std::fs::write(&unit, "stale").unwrap();
    let third = update();
    assert!(
        third.contains(&format!("removed {}\n", unit.display())),
        "{third}"
    );
    assert!(
        third.contains(&format!("installed {}\n", unit.display())),
        "{third}"
    );
    assert_eq!(std::fs::read_to_string(&unit).unwrap(), written);
}

/// `child` finished within 60 s, or it is killed and the test fails naming `what`.
#[cfg(target_os = "linux")] // only the Linux-only update test below uses it
fn wait_bounded(mut child: std::process::Child, what: &str) -> Output {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what} did not exit within 60 s");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// End to end and without `OWLPOST_UPDATE_ACTIVE`: a copy of the test
/// `owl` at `<tmp>/bin/owl` runs `update --source` and so rename-replaces its own file. The
/// fake `cargo` "builds" the same binary with a trailing marker (still a valid ELF, but not
/// the bytes already there). The unit under the temp `HOME` must name exactly
/// `<tmp>/bin/owl` — not `<tmp>/bin/owl (deleted)`, which is what `/proc/self/exe` reads
/// after the rename — and `<tmp>/bin/owl doctor` afterwards reports `ok binary`.
#[cfg(target_os = "linux")]
#[test]
fn update_of_the_running_binary_writes_the_unit_from_the_captured_path() {
    let (_dir, home, user_home, bin, log) = update_layout(0);
    // Canonical, so the unit's `ExecStart` (from the canonicalised `current_exe`) compares
    // exactly.
    let bin = std::fs::canonicalize(&bin).unwrap();
    let real = env!("CARGO_BIN_EXE_owl");
    let active = bin.join("owl");
    std::fs::copy(real, &active).unwrap();
    std::fs::set_permissions(&active, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        bin.join("cargo"),
        format!(
            "#!/bin/sh\necho \"cargo $*\" >> '{}'\n\
             root=''; while [ $# -gt 0 ]; do [ \"$1\" = --root ] && root=\"$2\"; shift; done\n\
             /bin/mkdir -p \"$root/bin\" && /bin/cat '{real}' > \"$root/bin/owl\" \
             && printf 'owl-030 built\\n' >> \"$root/bin/owl\" && /bin/chmod 755 \"$root/bin/owl\"\n",
            log.display()
        ),
    )
    .unwrap();
    let mut built = std::fs::read(real).unwrap();
    built.extend_from_slice(b"owl-030 built\n");
    let cmd = |args: &[&str]| {
        let mut c = Command::new(&active);
        c.env_remove("OWLPOST_HOME")
            .env_remove("OWLPOST_UPDATE_ACTIVE")
            .env("HOME", &user_home)
            .env("OWLPOST_INSTALL_NO_LOAD", "1")
            .env("PATH", &bin)
            .arg("--home")
            .arg(&home)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        c
    };
    // A concurrent fork in another test thread can hold the copy's write fd for an instant.
    let child = loop {
        match cmd(&UPDATE).spawn() {
            Ok(c) => break c,
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => panic!("spawning {}: {e}", active.display()),
        }
    };
    let out = wait_bounded(child, "owl update");
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains(&format!("installed {}\n", active.display())),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read(&active).unwrap(),
        built,
        "active holds the built bytes"
    );
    assert!(!active.with_extension("new").exists(), ".new left behind");
    // The unit names exactly the path captured before the replacement.
    assert_unit_runs(&user_home, &active, &home);
    assert!(!stdout.contains("(deleted)"), "{stdout}");
    // The replaced binary, run from its new file, reports itself as the one on PATH
    // and in the unit.
    let out = wait_bounded(cmd(&["doctor"]).spawn().unwrap(), "owl doctor");
    let (stdout, stderr) = text(&out);
    let binary: Vec<&str> = stdout.lines().filter(|l| l.contains(" binary: ")).collect();
    assert_eq!(
        binary,
        [format!("ok   binary: {}", active.display()).as_str()],
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(!stdout.contains("(deleted)"), "{stdout}");
}
