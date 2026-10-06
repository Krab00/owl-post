//! `owl update [--source <dir>] [--dry-run]`: replace the running `owl` binary (release
//! installer, or `cargo install` from a checkout), restart the daemon (re-registering it only
//! when its unit changed) so it runs the new binary, and reinstall the Claude Code plugin so
//! its copied command/hook files are refreshed (and register the `owl` MCP server at user
//! scope when it is missing).
//!
//! The new binary is built into a temp dir and then moved over the file that is
//! actually running (`install::owl_path()`, the canonicalised `current_exe`) — never into
//! `~/.cargo/bin` or `~/.local/bin` by name, which left the active copy stale when the two
//! differed. The move is a copy into the same directory plus `rename`, because a plain copy
//! over a running executable fails with `ETXTBSY`; the result is compared byte for byte.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

use super::install;
use super::setup::register_mcp;

const INSTALL_SH: &str =
    "https://raw.githubusercontent.com/Krab00/owl-post/main/scripts/install.sh";
const MARKETPLACE: &str = "owlpost-local";
/// Test hook: the file `owl update` replaces instead of the running binary, so a test can
/// drive the whole command without touching the test binary itself.
pub const ACTIVE_ENV: &str = "OWLPOST_UPDATE_ACTIVE";

/// The build/download step writing into `root`, and the file it produces there.
fn binary_cmd(source: Option<&Path>, root: &Path) -> (Vec<String>, PathBuf) {
    match source {
        Some(dir) => (
            vec![
                "cargo".into(),
                "install".into(),
                "--path".into(),
                dir.display().to_string(),
                "--locked".into(),
                "--root".into(),
                root.display().to_string(),
            ],
            root.join("bin").join("owl"),
        ),
        None => (
            vec![
                "sh".into(),
                "-c".into(),
                format!(
                    "f=$(mktemp) && curl -fsSL {INSTALL_SH} -o \"$f\" && sh \"$f\" --prefix \"{}\"",
                    root.display()
                ),
            ],
            root.join("owl"),
        ),
    }
}

/// `claude plugin ...` steps; the marketplace refresh may fail (local path marketplaces have
/// nothing to fetch), the reinstall must not.
fn plugin_cmds() -> Vec<(bool, Vec<String>)> {
    let c = |args: &[&str]| {
        std::iter::once("claude".to_string())
            .chain(args.iter().map(|s| s.to_string()))
            .collect::<Vec<_>>()
    };
    vec![
        (false, c(&["plugin", "marketplace", "update", MARKETPLACE])),
        (
            true,
            c(&["plugin", "uninstall", &format!("owlpost@{MARKETPLACE}")]),
        ),
        (
            true,
            c(&[
                "plugin",
                "install",
                &format!("owlpost@{MARKETPLACE}"),
                "--scope",
                "user",
            ]),
        ),
    ]
}

fn run(cmd: &[String], required: bool) -> anyhow::Result<()> {
    println!("+ {}", cmd.join(" "));
    let status = Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("running {}", cmd[0]))?;
    if !status.success() && required {
        bail!("{} failed ({status})", cmd.join(" "));
    }
    Ok(())
}

/// Puts `built` at `active` while `active` may be executing: copy to `<active>.new` in the
/// same directory (mode 0755), then `rename` over `active`. A process running the old file
/// keeps its inode; a plain copy would fail with `ETXTBSY`.
pub fn replace_binary(active: &Path, built: &Path) -> anyhow::Result<()> {
    let new = active.with_extension("new");
    std::fs::copy(built, &new)
        .with_context(|| format!("copying {} to {}", built.display(), new.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod {}", new.display()))?;
    }
    if let Err(e) = std::fs::rename(&new, active) {
        let _ = std::fs::remove_file(&new);
        return Err(e).with_context(|| format!("renaming over {}", active.display()));
    }
    Ok(())
}

/// `active` must now hold exactly the bytes of `built`; a mismatch names both paths.
pub fn verify_installed(active: &Path, built: &Path) -> anyhow::Result<()> {
    let got = std::fs::read(active).with_context(|| format!("reading {}", active.display()))?;
    let want = std::fs::read(built).with_context(|| format!("reading {}", built.display()))?;
    if got != want {
        bail!(
            "{} differs from the built {} after the replacement",
            active.display(),
            built.display()
        );
    }
    Ok(())
}

/// [`replace_binary`] + [`verify_installed`] + the `installed <active>` line.
pub fn install_binary(active: &Path, built: &Path) -> anyhow::Result<()> {
    replace_binary(active, built)?;
    verify_installed(active, built)?;
    println!("installed {}", active.display());
    Ok(())
}

fn active_path() -> anyhow::Result<PathBuf> {
    match std::env::var_os(ACTIVE_ENV).filter(|v| !v.is_empty()) {
        Some(p) => Ok(PathBuf::from(p)),
        None => install::owl_path(),
    }
}

pub fn run_update(home: &Path, source: Option<&Path>, dry_run: bool) -> anyhow::Result<()> {
    let active = active_path()?;
    let has_claude = Command::new("claude")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if dry_run {
        let (cmd, _) = binary_cmd(source, Path::new("<tmpdir>"));
        println!("would run: {}", cmd.join(" "));
        println!("would replace {}", active.display());
        println!("would run: owl uninstall && owl install");
        if has_claude {
            for (_, c) in plugin_cmds() {
                println!("would run: {}", c.join(" "));
            }
            register_mcp(true)?;
        } else {
            println!("claude not on PATH: plugin reinstall skipped");
        }
        return Ok(());
    }
    // A private build directory; `tempfile` is a dev-dependency only, so this is by hand.
    let tmp = std::env::temp_dir().join(format!("owl-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    let (cmd, built) = binary_cmd(source, &tmp);
    let result = run(&cmd, true).and_then(|()| {
        if !built.is_file() {
            bail!("{} did not produce {}", cmd.join(" "), built.display());
        }
        install_binary(&active, &built)
    });
    let _ = std::fs::remove_dir_all(&tmp);
    result?;
    // The unit names the binary path, which is now the one that was replaced in place: an
    // unchanged unit only restarts the service, a changed or missing one is uninstalled and
    // installed. `active` was read before the replacement: resolving it again here would
    // see `<path> (deleted)`.
    install::refresh(home, &active)?;
    if has_claude {
        for (required, c) in plugin_cmds() {
            run(&c, required)?;
        }
        register_mcp(false)?;
    } else {
        println!("claude not on PATH: plugin reinstall skipped");
    }
    println!("updated; restart your Claude Code session to load the plugin");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A copy of `/bin/sleep` at `path`, executable.
    fn sleep_binary(path: &Path) {
        std::fs::copy("/bin/sleep", path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// The active file is replaced while a child executes it; the child lives on, the
    /// path holds the built bytes, and the temp `.new` file is gone.
    #[test]
    fn replace_binary_swaps_a_running_executable() {
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join("owl");
        sleep_binary(&active);
        // Under parallel test threads a concurrent fork can still hold the copy's write fd
        // for an instant, so the very first exec may see ETXTBSY: retry briefly.
        let mut child = loop {
            match Command::new(&active).arg("30").spawn() {
                Ok(c) => break c,
                Err(e) if e.raw_os_error() == Some(libc_etxtbsy()) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => panic!("spawning the sleep copy: {e}"),
            }
        };
        // A plain copy over an executing file is exactly the failure this step avoids.
        let built = dir.path().join("built");
        std::fs::write(&built, b"#!/bin/sh\necho new\n").unwrap();
        #[cfg(target_os = "linux")]
        {
            let err = std::fs::copy(&built, &active).unwrap_err();
            assert_eq!(err.raw_os_error(), Some(libc_etxtbsy()), "{err}");
        }
        replace_binary(&active, &built).unwrap();
        assert_eq!(std::fs::read(&active).unwrap(), b"#!/bin/sh\necho new\n");
        assert!(
            !active.with_extension("new").exists(),
            "temp file renamed away"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&active).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        assert!(child.try_wait().unwrap().is_none(), "child still running");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    /// `ETXTBSY` ("text file busy"): 26 on Linux and macOS.
    fn libc_etxtbsy() -> i32 {
        26
    }

    #[test]
    fn verify_installed_accepts_identical_and_names_both_paths_on_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join("owl");
        let built = dir.path().join("built");
        std::fs::write(&built, b"v2").unwrap();
        replace_binary(&active, &built).unwrap();
        verify_installed(&active, &built).unwrap();
        // The built file is altered after the copy: a hard error naming both paths.
        std::fs::write(&built, b"v3").unwrap();
        let err = format!("{:#}", verify_installed(&active, &built).unwrap_err());
        assert!(err.contains(&active.display().to_string()), "{err}");
        assert!(err.contains(&built.display().to_string()), "{err}");
        assert!(err.contains("differs"), "{err}");
        // A missing built file is an error too, not a silent pass.
        std::fs::remove_file(&built).unwrap();
        assert!(verify_installed(&active, &built).is_err());
    }

    #[test]
    fn replace_binary_fails_cleanly_when_the_target_directory_is_unwritable() {
        let dir = tempfile::tempdir().unwrap();
        let built = dir.path().join("built");
        std::fs::write(&built, b"v2").unwrap();
        let missing = dir.path().join("no-such-dir").join("owl");
        let err = format!("{:#}", replace_binary(&missing, &built).unwrap_err());
        assert!(err.contains("copying"), "{err}");
        // A directory at the active path: the rename fails and the temp file is removed.
        let blocked = dir.path().join("owl");
        std::fs::create_dir_all(blocked.join("x")).unwrap();
        let err = format!("{:#}", replace_binary(&blocked, &built).unwrap_err());
        assert!(err.contains("renaming over"), "{err}");
        assert!(!blocked.with_extension("new").exists());
    }

    #[test]
    fn binary_cmd_builds_into_the_root_and_names_the_output() {
        let (cmd, built) = binary_cmd(Some(Path::new("/repo")), Path::new("/t"));
        assert_eq!(
            cmd,
            [
                "cargo", "install", "--path", "/repo", "--locked", "--root", "/t"
            ]
        );
        assert_eq!(built, PathBuf::from("/t/bin/owl"));
        let (cmd, built) = binary_cmd(None, Path::new("/t"));
        assert_eq!(cmd[..2], ["sh", "-c"]);
        assert!(cmd[2].contains(INSTALL_SH), "{}", cmd[2]);
        assert!(cmd[2].ends_with("sh \"$f\" --prefix \"/t\""), "{}", cmd[2]);
        assert_eq!(built, PathBuf::from("/t/owl"));
    }
}
