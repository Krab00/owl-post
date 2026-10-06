//! OS notifications (§11): `osascript` on macOS, `notify-send` on Linux, or whatever
//! `$OWLPOST_NOTIFY_CMD` names. The text never carries a question or answer body — only
//! `"<peer> asks about <path>"` / `"answer from <peer>"` / `"auto-answered <peer> about <path>"`. Nothing here can fail: a disabled
//! config, a missing binary or a spawn error all end in a silent (debug-logged) no-op.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::Config;

/// Environment variable naming a program to run instead of the OS notifier. It is invoked as
/// `<cmd> owlpost "<text>"` (title, then text), exactly like `notify-send`.
pub const CMD_ENV: &str = "OWLPOST_NOTIFY_CMD";
/// Title shown by every notification.
pub const TITLE: &str = "owlpost";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Question,
    Answer,
    /// The scheduler answered a question without a human (§3.4 auto).
    AutoAnswered,
    /// The peer's Task turned `REJECTED`: our question was declined.
    Declined,
}

/// The notification text for an event — the only data that ever leaves this module.
pub fn text(kind: Kind, peer_name: &str, path: &str) -> String {
    match kind {
        Kind::Question => format!("{peer_name} asks about {path}"),
        Kind::Answer => format!("answer from {peer_name}"),
        Kind::AutoAnswered => format!("auto-answered {peer_name} about {path}"),
        Kind::Declined => format!("declined by {peer_name}"),
    }
}

/// The program and its arguments for one notification: the override if set, else the
/// host OS notifier. `None` on a platform without a notifier (neither macOS nor Linux).
pub fn command_line(text: &str, cmd_override: Option<&str>) -> Option<(String, Vec<String>)> {
    if let Some(cmd) = cmd_override.filter(|c| !c.is_empty()) {
        return Some((cmd.to_string(), vec![TITLE.to_string(), text.to_string()]));
    }
    if cfg!(target_os = "macos") {
        let script = format!(
            "display notification \"{}\" with title \"{TITLE}\"",
            text.replace('\\', "\\\\").replace('"', "\\\"")
        );
        return Some(("osascript".to_string(), vec!["-e".to_string(), script]));
    }
    if cfg!(target_os = "linux") {
        return Some((
            "notify-send".to_string(),
            vec![TITLE.to_string(), text.to_string()],
        ));
    }
    None
}

/// Does `program` exist? A path with a separator is checked directly, a bare name on `PATH`.
pub fn program_exists(program: &str) -> bool {
    program_exists_in(program, std::env::var_os("PATH").as_deref())
}

pub fn program_exists_in(program: &str, path_var: Option<&std::ffi::OsStr>) -> bool {
    let p = Path::new(program);
    if p.components().count() > 1 || p.is_absolute() {
        return p.is_file();
    }
    path_var.is_some_and(|paths| {
        std::env::split_paths(paths)
            .map(|d| d.join(program))
            .any(|p| p.is_file())
    })
}

/// Fires a notification for `kind` if `config.notify` is on and a notifier can be found.
/// Reads `$OWLPOST_NOTIFY_CMD` and `PATH`; never returns an error.
pub fn notify(config: &Config, kind: Kind, peer_name: &str, path: &str) {
    let cmd = std::env::var(CMD_ENV).ok();
    notify_with(
        config,
        kind,
        peer_name,
        path,
        cmd.as_deref(),
        program_exists,
    );
}

/// `notify` with the override and the binary lookup injected (tests use a logging script and
/// a fake `which`). Spawns the notifier detached; a helper thread reaps it.
pub fn notify_with(
    config: &Config,
    kind: Kind,
    peer_name: &str,
    path: &str,
    cmd_override: Option<&str>,
    which: impl Fn(&str) -> bool,
) {
    if !config.notify {
        tracing::debug!("notifications disabled in config");
        return;
    }
    let text = text(kind, peer_name, path);
    let Some((program, args)) = command_line(&text, cmd_override) else {
        tracing::debug!("no notifier for this platform");
        return;
    };
    if !which(&program) {
        tracing::debug!(%program, "notifier not found; skipping");
        return;
    }
    spawn_detached(&program, &args);
}

fn spawn_detached(program: &str, args: &[String]) {
    let spawned = Command::new(PathBuf::from(program))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            // Reap on a helper thread so a slow notifier neither blocks the daemon nor
            // lingers as a zombie.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => tracing::debug!(%program, error = %e, "notifier failed to start"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A shell script that appends `"$1|$2"` to `<dir>/log` on every call.
    fn logging_script(dir: &Path) -> (PathBuf, PathBuf) {
        let script = dir.join("notify.sh");
        let log = dir.join("log");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s|%s\\n' \"$1\" \"$2\" >> '{}'\n",
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (script, log)
    }

    fn wait_for_log(log: &Path, lines: usize) -> String {
        let start = Instant::now();
        loop {
            let body = std::fs::read_to_string(log).unwrap_or_default();
            if body.lines().count() >= lines {
                return body;
            }
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "log never reached {lines} lines: {body:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn text_names_peer_and_path_only() {
        assert_eq!(
            text(Kind::Question, "Martin", "src/auth.rs"),
            "Martin asks about src/auth.rs"
        );
        assert_eq!(
            text(Kind::Answer, "Martin", "src/auth.rs"),
            "answer from Martin"
        );
        assert_eq!(
            text(Kind::AutoAnswered, "Martin", "src/auth.rs"),
            "auto-answered Martin about src/auth.rs"
        );
    }

    #[test]
    fn command_line_prefers_override_then_platform() {
        let (prog, args) = command_line("hi there", Some("/opt/notify")).unwrap();
        assert_eq!(prog, "/opt/notify");
        assert_eq!(args, ["owlpost", "hi there"]);
        // Empty override counts as unset.
        let platform = command_line("say \"x\"", Some(""));
        assert_eq!(platform, command_line("say \"x\"", None));
        if cfg!(target_os = "macos") {
            let (prog, args) = platform.unwrap();
            assert_eq!(prog, "osascript");
            assert_eq!(
                args,
                [
                    "-e",
                    "display notification \"say \\\"x\\\"\" with title \"owlpost\""
                ]
            );
        } else if cfg!(target_os = "linux") {
            let (prog, args) = platform.unwrap();
            assert_eq!(prog, "notify-send");
            assert_eq!(args, ["owlpost", "say \"x\""]);
        } else {
            assert!(platform.is_none());
        }
    }

    #[test]
    fn program_exists_checks_paths_and_path_var() {
        let dir = tempfile::tempdir().unwrap();
        let (script, _) = logging_script(dir.path());
        assert!(program_exists_in(script.to_str().unwrap(), None));
        assert!(!program_exists_in(
            dir.path().join("missing").to_str().unwrap(),
            None
        ));
        let path_var = std::env::join_paths([dir.path()]).unwrap();
        assert!(program_exists_in("notify.sh", Some(&path_var)));
        assert!(!program_exists_in("other.sh", Some(&path_var)));
        assert!(!program_exists_in("notify.sh", None));
        // A directory is not a program.
        assert!(!program_exists_in(dir.path().to_str().unwrap(), None));
    }

    #[test]
    fn enabled_with_binary_runs_the_override() {
        let dir = tempfile::tempdir().unwrap();
        let (script, log) = logging_script(dir.path());
        let cfg = Config::default();
        assert!(cfg.notify);
        notify_with(
            &cfg,
            Kind::Question,
            "Martin",
            "src/auth.rs",
            Some(script.to_str().unwrap()),
            |_| true,
        );
        notify_with(
            &cfg,
            Kind::Answer,
            "Martin",
            "src/auth.rs",
            Some(script.to_str().unwrap()),
            |_| true,
        );
        let body = wait_for_log(&log, 2);
        let mut lines: Vec<&str> = body.lines().collect();
        lines.sort();
        assert_eq!(
            lines,
            [
                "owlpost|Martin asks about src/auth.rs",
                "owlpost|answer from Martin"
            ]
        );
    }

    #[test]
    fn disabled_or_missing_binary_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        let (script, log) = logging_script(dir.path());
        let script = script.to_str().unwrap();
        // notify == false: nothing runs even though a working override is set and found.
        let off = Config {
            notify: false,
            ..Default::default()
        };
        notify_with(
            &off,
            Kind::Question,
            "Martin",
            "src/auth.rs",
            Some(script),
            |_| true,
        );
        notify_with(
            &off,
            Kind::Answer,
            "Martin",
            "src/auth.rs",
            Some(script),
            |_| true,
        );
        // notify == true but the command is absent: skipped, no error, no panic.
        let on = Config::default();
        notify_with(
            &on,
            Kind::Question,
            "Martin",
            "src/auth.rs",
            Some(script),
            |_| false,
        );
        let missing = dir.path().join("does-not-exist.sh");
        notify_with(
            &on,
            Kind::Answer,
            "Martin",
            "src/auth.rs",
            Some(missing.to_str().unwrap()),
            program_exists,
        );
        // And the real lookup on a platform binary that is not on an empty PATH.
        notify_with(&on, Kind::Question, "Martin", "src/auth.rs", None, |p| {
            program_exists_in(p, Some(std::ffi::OsStr::new("")))
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !log.exists(),
            "nothing may be spawned: {:?}",
            std::fs::read_to_string(&log)
        );
    }

    #[test]
    fn spawn_failure_is_swallowed() {
        let dir = tempfile::tempdir().unwrap();
        // Exists but is not executable: spawn fails, notify_with still returns.
        let script = dir.path().join("noexec.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        notify_with(
            &Config::default(),
            Kind::Question,
            "Martin",
            "x",
            Some(script.to_str().unwrap()),
            |_| true,
        );
    }
}
