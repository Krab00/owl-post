//! `scripts/install.sh` installs a working `owl` from a release layout and fails
//! closed on every guard. Nothing here touches the network: a tarball of the test binary plus
//! a `SHA256SUMS` are written to a temp dir and served through `OWL_INSTALL_BASE_URL=file://`,
//! which `curl` reads like any other URL. Each guard has its own row with the message unique
//! to it (a shared epilogue would let `contains` match every exit path).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const INSTALL_SH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/install.sh");
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Same OS/arch mapping the script uses, for the host this test runs on.
fn host_target() -> String {
    let os = match std::env::consts::OS {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        other => panic!("tests run on linux or macos only, got {other}"),
    };
    format!("{}-{}", std::env::consts::ARCH, os)
}

/// A release directory holding `owl-<VERSION>-<host target>.tar.gz` and `SHA256SUMS`, built
/// from the binary under test.
struct Release {
    dir: tempfile::TempDir,
    asset: String,
}

impl Release {
    fn build() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let asset = format!("owl-{VERSION}-{}.tar.gz", host_target());
        let bin_dir = Path::new(env!("CARGO_BIN_EXE_owl")).parent().unwrap();
        let tar = Command::new("tar")
            .args(["-C"])
            .arg(bin_dir)
            .arg("-czf")
            .arg(dir.path().join(&asset))
            .arg("owl")
            .output()
            .expect("tar is required to build the fixture tarball");
        assert!(tar.status.success(), "{}", stderr(&tar));
        let sum = sha256(&dir.path().join(&asset));
        fs::write(dir.path().join("SHA256SUMS"), format!("{sum}  {asset}\n")).unwrap();
        Self { dir, asset }
    }

    fn base_url(&self) -> String {
        format!("file://{}", self.dir.path().display())
    }
}

fn sha256(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(path).unwrap();
    format!("{:x}", Sha256::digest(bytes))
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Runs `sh scripts/install.sh <args>` with the hook variables cleared, then `env` applied.
fn install(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new("sh");
    c.arg(INSTALL_SH).args(args).stdin(Stdio::null());
    for k in [
        "OWL_INSTALL_BASE_URL",
        "OWL_INSTALL_FAKE_SUM",
        "OWL_INSTALL_OS",
        "OWL_INSTALL_ARCH",
        "OWL_INSTALL_CURL",
    ] {
        c.env_remove(k);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn prefix() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("bin");
    (d, p)
}

fn owl_version(prefix: &Path) -> String {
    let out = Command::new(prefix.join("owl"))
        .arg("--version")
        .env_remove("OWLPOST_HOME")
        .output()
        .expect("installed owl runs");
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out).trim().to_string()
}

#[test]
fn script_is_executable_posix_sh() {
    let meta = fs::metadata(INSTALL_SH).expect("scripts/install.sh exists");
    assert!(meta.is_file());
    assert_ne!(meta.permissions().mode() & 0o111, 0, "must be executable");
    let src = fs::read_to_string(INSTALL_SH).unwrap();
    assert!(
        src.starts_with("#!/bin/sh\n"),
        "shebang: {:?}",
        src.lines().next()
    );
    assert!(
        src.contains("\nset -eu\n"),
        "must fail on errors and unset vars"
    );
    // Every hook the tests rely on is documented in the header.
    for hook in [
        "OWL_INSTALL_BASE_URL",
        "OWL_INSTALL_FAKE_SUM",
        "OWL_INSTALL_OS",
        "OWL_INSTALL_ARCH",
        "OWL_INSTALL_CURL",
    ] {
        let header: String = src.lines().take_while(|l| l.starts_with('#')).collect();
        assert!(header.contains(hook), "header must document {hook}");
    }
    // The real download base is the GitHub releases URL of this repository.
    assert!(src.contains("https://github.com/${REPO}/releases/download/${tag}"));
    assert!(src.contains("REPO=\"Krab00/owl-post\""));
}

// ---------------------------------------------------------------- install works

#[test]
fn installs_the_tagged_version_into_prefix() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let tag = format!("v{VERSION}");
    let out = install(
        &["--version", &tag, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));

    // The binary is there, executable, and reports the tag's version.
    let bin = pfx.join("owl");
    assert_ne!(
        fs::metadata(&bin).unwrap().permissions().mode() & 0o111,
        0,
        "installed owl must be executable"
    );
    assert_eq!(owl_version(&pfx), format!("owl {VERSION}"));
    // No temp file left next to it.
    let names: Vec<_> = fs::read_dir(&pfx)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, vec!["owl".to_string()]);

    // Stdout, whole: what was fetched, where it landed, the PATH note (the temp prefix is
    // never on PATH), the three next steps in order.
    assert_eq!(
        stdout(&out),
        format!(
            "downloading {base}/{asset}\n\
             installed owl {VERSION} to {pfx}/owl\n\
             note: {pfx} is not on your PATH; add it to your shell profile\n\
             next steps:\n\
             \x20 owl init              # create your key and config, prints your fingerprint\n\
             \x20 owl install           # register the owl daemon as a user service\n\
             \x20 owl contact export    # your peer file, to be committed under .agents/peers/\n",
            base = rel.base_url(),
            asset = rel.asset,
            pfx = pfx.display()
        )
    );
}

#[test]
fn version_without_v_prefix_and_bare_tag_both_install() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(owl_version(&pfx), format!("owl {VERSION}"));
}

#[test]
fn without_version_reads_the_asset_name_from_sha256sums() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let out = install(
        &["--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains(&format!("installed owl {VERSION} to")),
        "{}",
        stdout(&out)
    );
    assert_eq!(owl_version(&pfx), format!("owl {VERSION}"));
}

#[test]
fn reinstall_overwrites_an_existing_binary() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    fs::create_dir_all(&pfx).unwrap();
    fs::write(pfx.join("owl"), "#!/bin/sh\necho stale\n").unwrap();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(owl_version(&pfx), format!("owl {VERSION}"));
}

#[test]
fn prefix_on_path_gets_no_path_note() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    fs::create_dir_all(&pfx).unwrap();
    let path = format!(
        "{}:{}",
        pfx.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url()), ("PATH", &path)],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        !stdout(&out).contains("is not on your PATH"),
        "{}",
        stdout(&out)
    );
    // And the twin: a prefix outside PATH gets the note.
    let (_d2, pfx2) = prefix();
    let out = install(
        &["--version", VERSION, "--prefix", pfx2.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains(&format!(
            "note: {} is not on your PATH; add it to your shell profile",
            pfx2.display()
        )),
        "{}",
        stdout(&out)
    );
}

// ---------------------------------------------------------------- checksum mismatch

#[test]
fn fake_sum_fails_with_checksum_mismatch_and_installs_nothing() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[
            ("OWL_INSTALL_BASE_URL", &rel.base_url()),
            ("OWL_INSTALL_FAKE_SUM", "1"),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    let real = sha256(&rel.dir.path().join(&rel.asset));
    assert_eq!(
        err.trim(),
        format!(
            "owl install: checksum mismatch for {}: expected {}, got {real}",
            rel.asset,
            "0".repeat(64)
        )
    );
    assert!(!pfx.join("owl").exists(), "nothing may be installed");
    let leftovers: Vec<_> = fs::read_dir(&pfx).unwrap().collect();
    assert!(
        leftovers.is_empty(),
        "prefix must stay empty: {leftovers:?}"
    );
}

#[test]
fn a_tampered_tarball_fails_the_real_checksum() {
    // Not the hook: the SHA256SUMS is genuine and the tarball is modified after signing.
    let rel = Release::build();
    let path = rel.dir.path().join(&rel.asset);
    let mut bytes = fs::read(&path).unwrap();
    bytes.push(0);
    fs::write(&path, &bytes).unwrap();
    let (_d, pfx) = prefix();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).starts_with(&format!(
            "owl install: checksum mismatch for {}: expected ",
            rel.asset
        )),
        "{}",
        stderr(&out)
    );
    assert!(!pfx.join("owl").exists());
}

#[test]
fn fake_sum_unset_or_not_one_does_not_corrupt() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[
            ("OWL_INSTALL_BASE_URL", &rel.base_url()),
            ("OWL_INSTALL_FAKE_SUM", "0"),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(owl_version(&pfx), format!("owl {VERSION}"));
}

// ---------------------------------------------------------------- guards, one row each

#[test]
fn unsupported_platform_fails_before_anything_is_fetched() {
    let (_d, pfx) = prefix();
    for (os, arch, shown) in [
        ("Linux", "riscv64", "Linux/riscv64"),
        ("FreeBSD", "x86_64", "FreeBSD/x86_64"),
        ("Darwin", "i386", "Darwin/i386"),
    ] {
        let out = install(
            &["--prefix", pfx.to_str().unwrap()],
            &[
                ("OWL_INSTALL_OS", os),
                ("OWL_INSTALL_ARCH", arch),
                ("OWL_INSTALL_BASE_URL", "file:///nonexistent"),
            ],
        );
        assert_eq!(out.status.code(), Some(2), "{shown}: {}", stderr(&out));
        assert_eq!(
            stderr(&out).trim(),
            format!(
                "owl install: unsupported platform {shown} (supported: macOS and Linux on x86_64 or aarch64)"
            )
        );
        assert!(stdout(&out).is_empty());
    }
    assert!(
        !pfx.exists(),
        "prefix is created only for a supported platform"
    );
}

#[test]
fn every_supported_pair_maps_to_its_triple() {
    // The positive twin of the platform guard: each pair resolves to the asset name for its
    // triple (observed through the "not listed" message, so no build per target is needed).
    let rel = Release::build();
    let (_d, pfx) = prefix();
    for (os, arch, triple) in [
        ("Darwin", "arm64", "aarch64-apple-darwin"),
        ("Darwin", "aarch64", "aarch64-apple-darwin"),
        ("Darwin", "x86_64", "x86_64-apple-darwin"),
        ("Linux", "x86_64", "x86_64-unknown-linux-gnu"),
        ("Linux", "amd64", "x86_64-unknown-linux-gnu"),
        ("Linux", "aarch64", "aarch64-unknown-linux-gnu"),
        ("Linux", "arm64", "aarch64-unknown-linux-gnu"),
    ] {
        let out = install(
            &["--version", "v9.9.9", "--prefix", pfx.to_str().unwrap()],
            &[
                ("OWL_INSTALL_OS", os),
                ("OWL_INSTALL_ARCH", arch),
                ("OWL_INSTALL_BASE_URL", &rel.base_url()),
            ],
        );
        assert_eq!(out.status.code(), Some(1), "{os}/{arch}: {}", stderr(&out));
        assert_eq!(
            stderr(&out).trim(),
            format!("owl install: owl-9.9.9-{triple}.tar.gz is not listed in SHA256SUMS"),
            "{os}/{arch}"
        );
    }
}

#[test]
fn missing_curl_fails_with_its_own_message() {
    let (_d, pfx) = prefix();
    let out = install(
        &["--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_CURL", "owl-test-no-such-curl")],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        "owl install: curl is required but was not found on PATH"
    );
    assert!(stdout(&out).is_empty());
}

#[test]
fn download_failure_names_the_url() {
    let (_d, pfx) = prefix();
    let missing = tempfile::tempdir().unwrap();
    let base = format!("file://{}/no-such-release", missing.path().display());
    let out = install(
        &["--version", "v1.2.3", "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &base)],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        format!("owl install: download failed for {base}/SHA256SUMS")
    );
    assert!(!pfx.join("owl").exists());

    // Second arm: SHA256SUMS lists the asset but the tarball itself is gone.
    let rel = Release::build();
    fs::remove_file(rel.dir.path().join(&rel.asset)).unwrap();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        format!(
            "owl install: download failed for {}/{}",
            rel.base_url(),
            rel.asset
        )
    );
}

#[test]
fn version_not_listed_in_sha256sums_fails() {
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let out = install(
        &["--version", "v9.9.9", "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        format!(
            "owl install: owl-9.9.9-{}.tar.gz is not listed in SHA256SUMS",
            host_target()
        )
    );
}

#[test]
fn latest_without_an_asset_for_this_target_fails() {
    let rel = Release::build();
    fs::write(
        rel.dir.path().join("SHA256SUMS"),
        format!(
            "{}  owl-{VERSION}-riscv64gc-unknown-linux-gnu.tar.gz\n",
            "a".repeat(64)
        ),
    )
    .unwrap();
    let (_d, pfx) = prefix();
    let out = install(
        &["--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        format!(
            "owl install: no asset for {} listed in {}/SHA256SUMS",
            host_target(),
            rel.base_url()
        )
    );
}

#[test]
fn unwritable_prefix_fails_before_download() {
    let d = tempfile::tempdir().unwrap();
    let ro = d.path().join("ro");
    fs::create_dir(&ro).unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(ro.join("probe"), b"").is_ok() {
        // Running as root: the read-only bit does not bind, the guard cannot be induced.
        eprintln!("skipping: prefix is writable despite 0555 (root?)");
        return;
    }
    let out = install(
        &["--version", VERSION, "--prefix", ro.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", "file:///nonexistent")],
    );
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        format!(
            "owl install: prefix {} is not writable (use --prefix <dir> you own, or sudo for --system)",
            ro.display()
        )
    );
    assert!(stdout(&out).is_empty(), "nothing was downloaded");

    // Sibling arm: the prefix cannot be created because its parent is a file.
    let file = d.path().join("file");
    fs::write(&file, b"").unwrap();
    let under_file = file.join("bin");
    let out = install(
        &[
            "--version",
            VERSION,
            "--prefix",
            under_file.to_str().unwrap(),
        ],
        &[("OWL_INSTALL_BASE_URL", "file:///nonexistent")],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).starts_with(&format!(
            "owl install: prefix {} is not writable",
            under_file.display()
        )),
        "{}",
        stderr(&out)
    );
}

#[test]
fn usage_errors_are_distinct_and_exit_2() {
    let cases: [(&[&str], &str); 4] = [
        (
            &["--version"],
            "owl install: --version needs a value (e.g. --version v0.1.0)",
        ),
        (
            &["--prefix"],
            "owl install: --prefix needs a value (e.g. --prefix ~/.local/bin)",
        ),
        (
            &["--bogus"],
            "owl install: unknown argument '--bogus' (see --help)",
        ),
        (
            &["--version", "v1", "extra"],
            "owl install: unknown argument 'extra' (see --help)",
        ),
    ];
    for (args, msg) in cases {
        let out = install(args, &[]);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert_eq!(stderr(&out).trim(), msg, "{args:?}");
        assert!(stdout(&out).is_empty(), "{args:?}");
    }
}

const USAGE: &str = "owl install: download a release of owl, verify its SHA-256 and install it

usage: install.sh [--version <tag>] [--prefix <dir> | --system]
   or: curl -fsSL https://raw.githubusercontent.com/Krab00/owl-post/main/scripts/install.sh | sh

  --version <tag>   release tag to install, e.g. v0.1.0 (default: the latest release)
  --prefix <dir>    directory that receives owl (default: ~/.local/bin)
  --system          shorthand for --prefix /usr/local/bin
  -h, --help        print this help
";

#[test]
fn help_prints_the_usage_text_and_exits_0() {
    for flag in ["--help", "-h"] {
        let out = install(&[flag], &[]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        assert_eq!(stdout(&out), USAGE, "{flag}");
        assert!(stderr(&out).is_empty());
    }
}

#[test]
fn system_flag_targets_usr_local_bin() {
    // The prefix guard runs before any download, so the expected message depends only on
    // whether /usr/local/bin is writable here; decide that with the script's own predicate
    // and assert the one exact message. The fake curl keeps github.com out of it.
    let d = tempfile::tempdir().unwrap();
    let (curl, log) = fake_curl(d.path());
    let writable = Command::new("/bin/sh")
        .args(["-c", "[ -d /usr/local/bin ] && [ -w /usr/local/bin ]"])
        .status()
        .unwrap()
        .success();
    let out = install(
        &["--system", "--version", "v1.2.3"],
        &[("OWL_INSTALL_CURL", curl.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let expected = if writable {
        "owl install: download failed for https://github.com/Krab00/owl-post/releases/download/v1.2.3/SHA256SUMS"
    } else {
        "owl install: prefix /usr/local/bin is not writable (use --prefix <dir> you own, or sudo for --system)"
    };
    assert_eq!(stderr(&out).trim(), expected, "writable={writable}");
    assert_eq!(
        log.exists(),
        writable,
        "curl runs only past the prefix guard"
    );
}

// ---------------------------------------------------------------- real URLs, no network

/// A fake `curl` that records its arguments and fails, so the URL the script would fetch
/// from GitHub is observable without any network access.
fn fake_curl(dir: &Path) -> (PathBuf, PathBuf) {
    let log = dir.join("curl.log");
    let bin = dir.join("curl");
    fs::write(
        &bin,
        format!("#!/bin/sh\necho \"$@\" >> {}\nexit 22\n", log.display()),
    )
    .unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    (bin, log)
}

#[test]
fn default_base_url_is_the_github_release_for_the_normalised_tag() {
    let d = tempfile::tempdir().unwrap();
    let (curl, log) = fake_curl(d.path());
    let (_p, pfx) = prefix();
    let cases = [
        (
            vec!["--version", "0.1.0"],
            "https://github.com/Krab00/owl-post/releases/download/v0.1.0/SHA256SUMS",
        ),
        (
            vec!["--version", "v0.1.0"],
            "https://github.com/Krab00/owl-post/releases/download/v0.1.0/SHA256SUMS",
        ),
        (
            vec![],
            "https://github.com/Krab00/owl-post/releases/latest/download/SHA256SUMS",
        ),
    ];
    for (args, url) in cases {
        let _ = fs::remove_file(&log);
        let mut full = args.clone();
        full.extend(["--prefix", pfx.to_str().unwrap()]);
        let out = install(&full, &[("OWL_INSTALL_CURL", curl.to_str().unwrap())]);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
        assert_eq!(
            stderr(&out).trim(),
            format!("owl install: download failed for {url}"),
            "{args:?}"
        );
        let logged = fs::read_to_string(&log).unwrap();
        // Exactly one curl call: -fsSL --retry 2 -o <tmp>/SHA256SUMS <url>
        let argv: Vec<&str> = logged.trim().split(' ').collect();
        assert_eq!(argv.len(), 6, "{args:?}: curl got {logged}");
        assert_eq!(&argv[..4], ["-fsSL", "--retry", "2", "-o"], "{logged}");
        assert!(argv[4].ends_with("/SHA256SUMS"), "{logged}");
        assert_eq!(argv[5], url, "{logged}");
    }
}

#[test]
fn installed_binary_is_made_executable_even_if_the_tarball_entry_is_not() {
    // A release tarball normally carries the mode bit; the installer must not depend on it.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_owl"), src.join("owl")).unwrap();
    fs::set_permissions(src.join("owl"), fs::Permissions::from_mode(0o644)).unwrap();
    let asset = format!("owl-{VERSION}-{}.tar.gz", host_target());
    let tar = Command::new("tar")
        .arg("-C")
        .arg(&src)
        .arg("-czf")
        .arg(dir.path().join(&asset))
        .arg("owl")
        .output()
        .unwrap();
    assert!(tar.status.success(), "{}", stderr(&tar));
    let sum = sha256(&dir.path().join(&asset));
    fs::write(dir.path().join("SHA256SUMS"), format!("{sum}  {asset}\n")).unwrap();

    let (_p, pfx) = prefix();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[(
            "OWL_INSTALL_BASE_URL",
            &format!("file://{}", dir.path().display()),
        )],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let mode = fs::metadata(pfx.join("owl")).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111, "mode {mode:o}");
    assert_eq!(owl_version(&pfx), format!("owl {VERSION}"));
}

// ---------------------------------------------------------------- more guards and input forms

#[test]
fn directory_at_prefix_owl_fails_before_download_and_leaves_nothing() {
    // No release fixture on purpose: the base URL resolves to nothing and curl is the
    // logging fake, so the only way to get the directory message is for the guard to run
    // before the first fetch (SHA256SUMS included). A guard moved past that fetch would
    // fail on the download instead and leave a curl log behind.
    let d = tempfile::tempdir().unwrap();
    let (curl, log) = fake_curl(d.path());
    let (_p, pfx) = prefix();
    fs::create_dir_all(pfx.join("owl")).unwrap();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[
            ("OWL_INSTALL_BASE_URL", "file:///nonexistent"),
            ("OWL_INSTALL_CURL", curl.to_str().unwrap()),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        format!(
            "owl install: could not write {}/owl: is a directory",
            pfx.display()
        )
    );
    assert!(stdout(&out).is_empty(), "no download: {}", stdout(&out));
    assert!(!log.exists(), "curl ran before the directory guard");
    // The directory is untouched and no temp file exists anywhere under the prefix.
    let mut stack = vec![pfx.clone()];
    let mut seen = Vec::new();
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            }
            seen.push(e.path());
        }
    }
    assert_eq!(seen, vec![pfx.join("owl")], "{seen:?}");
    assert!(
        !seen.iter().any(|p| p
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("owl.tmp.")),
        "{seen:?}"
    );
}

/// The README's form: the script arrives on stdin (`curl … | sh -s -- <args>`), so `$0` is
/// `sh` and nothing may depend on the script's own path.
fn install_piped(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new("/bin/sh");
    c.arg("-s").arg("--").args(args);
    c.stdin(Stdio::from(fs::File::open(INSTALL_SH).unwrap()));
    for k in [
        "OWL_INSTALL_BASE_URL",
        "OWL_INSTALL_FAKE_SUM",
        "OWL_INSTALL_CURL",
    ] {
        c.env_remove(k);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

#[test]
fn piped_form_prints_help_and_installs() {
    for flag in ["-h", "--help"] {
        let out = install_piped(&[flag], &[]);
        assert_eq!(out.status.code(), Some(0), "{flag}: {}", stderr(&out));
        assert_eq!(stdout(&out), USAGE, "{flag}");
        assert!(stderr(&out).is_empty(), "{flag}: {}", stderr(&out));
    }
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let out = install_piped(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &rel.base_url())],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
    assert_eq!(owl_version(&pfx), format!("owl {VERSION}"));
    // And a guard through the pipe keeps its exit code and message.
    let out = install_piped(&["--bogus"], &[]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        stderr(&out).trim(),
        "owl install: unknown argument '--bogus' (see --help)"
    );
}

/// Runs the script with `PATH` set to a directory holding only the named tools (symlinks to
/// the real binaries), the platform pinned so `uname` is not needed, and curl by absolute
/// path so only the guard under test can fire.
fn install_with_tools(tools: &[&str]) -> Output {
    let d = tempfile::tempdir().unwrap();
    for t in tools {
        let real = String::from_utf8(
            Command::new("/bin/sh")
                .args(["-c", &format!("command -v {t}")])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        std::os::unix::fs::symlink(real.trim(), d.path().join(t)).unwrap();
    }
    let curl = String::from_utf8(
        Command::new("/bin/sh")
            .args(["-c", "command -v curl"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let mut c = Command::new("/bin/sh");
    c.arg(INSTALL_SH)
        .args(["--prefix", d.path().join("bin").to_str().unwrap()])
        .env("PATH", d.path())
        .env("OWL_INSTALL_OS", "Linux")
        .env("OWL_INSTALL_ARCH", "x86_64")
        .env("OWL_INSTALL_CURL", curl.trim())
        .env("OWL_INSTALL_BASE_URL", "file:///nonexistent")
        .stdin(Stdio::null());
    c.output().unwrap()
}

#[test]
fn missing_tar_fails_with_its_own_message() {
    let out = install_with_tools(&[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        "owl install: tar is required but was not found on PATH"
    );
    assert!(stdout(&out).is_empty());
}

#[test]
fn missing_sha256_tool_fails_with_its_own_message() {
    let out = install_with_tools(&["tar"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        "owl install: neither sha256sum nor shasum is available to verify the download"
    );
    assert!(stdout(&out).is_empty());
    // Positive twin: either tool alone is enough to get past the guard (to the prefix step,
    // which then fails on the dead base URL, proving the guard was passed).
    for tool in ["sha256sum", "shasum"] {
        let has = Command::new("/bin/sh")
            .args(["-c", &format!("command -v {tool}")])
            .output()
            .unwrap()
            .status
            .success();
        if !has {
            continue;
        }
        // mkdir/mktemp/rm are what the prefix and download steps themselves need.
        let out = install_with_tools(&["tar", tool, "mkdir", "mktemp", "rm"]);
        assert_eq!(out.status.code(), Some(1), "{tool}: {}", stderr(&out));
        assert_eq!(
            stderr(&out).trim(),
            "owl install: download failed for file:///nonexistent/SHA256SUMS",
            "{tool}"
        );
    }
}

/// A release dir whose tarball bytes are `bytes` and whose SHA256SUMS matches them exactly,
/// so the checksum passes and only the extraction can fail.
fn release_with_bytes(bytes: &[u8]) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let asset = format!("owl-{VERSION}-{}.tar.gz", host_target());
    fs::write(dir.path().join(&asset), bytes).unwrap();
    let sum = sha256(&dir.path().join(&asset));
    fs::write(dir.path().join("SHA256SUMS"), format!("{sum}  {asset}\n")).unwrap();
    (dir, asset)
}

#[test]
fn unextractable_tarball_fails_after_a_matching_checksum() {
    // Arm 1: not a gzip stream at all.
    let (dir, asset) = release_with_bytes(b"this is not a tarball\n");
    let (_d, pfx) = prefix();
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[(
            "OWL_INSTALL_BASE_URL",
            &format!("file://{}", dir.path().display()),
        )],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        format!("owl install: could not extract owl from {asset}")
    );
    assert!(!pfx.join("owl").exists());
    assert_eq!(fs::read_dir(&pfx).unwrap().count(), 0, "prefix stays empty");

    // Arm 2: a valid tarball that holds no `owl` entry.
    let src = tempfile::tempdir().unwrap();
    fs::write(src.path().join("README"), b"no binary here").unwrap();
    let tgz = src.path().join("x.tar.gz");
    let tar = Command::new("tar")
        .arg("-C")
        .arg(src.path())
        .arg("-czf")
        .arg(&tgz)
        .arg("README")
        .output()
        .unwrap();
    assert!(tar.status.success());
    let (dir, asset) = release_with_bytes(&fs::read(&tgz).unwrap());
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[(
            "OWL_INSTALL_BASE_URL",
            &format!("file://{}", dir.path().display()),
        )],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        format!("owl install: could not extract owl from {asset}")
    );
    assert!(!pfx.join("owl").exists());
}

#[test]
fn trailing_slash_on_base_url_is_stripped() {
    // file:// arm: the fetched URL has a single slash and the install succeeds.
    let rel = Release::build();
    let (_d, pfx) = prefix();
    let base = format!("{}/", rel.base_url());
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[("OWL_INSTALL_BASE_URL", &base)],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).lines().next().unwrap(),
        format!("downloading {}/{}", rel.base_url(), rel.asset)
    );
    assert_eq!(owl_version(&pfx), format!("owl {VERSION}"));

    // http arm through the fake curl: the URL curl receives has no `//`.
    let d = tempfile::tempdir().unwrap();
    let (curl, log) = fake_curl(d.path());
    let out = install(
        &["--version", VERSION, "--prefix", pfx.to_str().unwrap()],
        &[
            ("OWL_INSTALL_BASE_URL", "https://mirror.example/owl/"),
            ("OWL_INSTALL_CURL", curl.to_str().unwrap()),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stderr(&out).trim(),
        "owl install: download failed for https://mirror.example/owl/SHA256SUMS"
    );
    let logged = fs::read_to_string(&log).unwrap();
    assert!(
        logged
            .trim()
            .ends_with(" https://mirror.example/owl/SHA256SUMS"),
        "{logged}"
    );
}

// ---------------------------------------------------------------- drift guard vs release.yml

#[test]
fn install_sh_targets_and_asset_name_match_release_yml() {
    let yml = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/release.yml"
    ))
    .unwrap();
    // The workflow's matrix targets, in order, and its asset name pattern.
    let mut wf_targets: Vec<String> = yml
        .lines()
        .filter_map(|l| l.trim().strip_prefix("- target: "))
        .map(str::to_string)
        .collect();
    assert_eq!(wf_targets.len(), 4, "{wf_targets:?}");
    assert!(
        yml.contains(r#"asset="owl-${VERSION}-${TARGET}.tar.gz""#),
        "release.yml asset name pattern"
    );
    assert!(yml.contains("sha256sum owl-*.tar.gz > SHA256SUMS"));

    // The script's case table: every `target="…"` literal, and its asset construction.
    let sh = fs::read_to_string(INSTALL_SH).unwrap();
    let mut sh_targets: Vec<String> = sh
        .lines()
        .filter_map(|l| {
            let i = l.find("target=\"")? + "target=\"".len();
            let rest = &l[i..];
            Some(rest[..rest.find('"')?].to_string())
        })
        .collect();
    sh_targets.sort();
    sh_targets.dedup();
    wf_targets.sort();
    assert_eq!(sh_targets, wf_targets);
    assert!(
        sh.contains(r#"asset="owl-${version}-${target}.tar.gz""#),
        "install.sh builds the same asset name"
    );

    // Behavioural twin: for each workflow target, a SHA256SUMS written with the workflow's
    // naming makes the script request exactly that tarball (present in the sums, absent on
    // disk, so the download-failed message names it).
    let dir = tempfile::tempdir().unwrap();
    let sums: String = wf_targets
        .iter()
        .map(|t| format!("{}  owl-9.9.9-{t}.tar.gz\n", "b".repeat(64)))
        .collect();
    fs::write(dir.path().join("SHA256SUMS"), sums).unwrap();
    let base = format!("file://{}", dir.path().display());
    let (_d, pfx) = prefix();
    for (os, arch, target) in [
        ("Darwin", "arm64", "aarch64-apple-darwin"),
        ("Darwin", "x86_64", "x86_64-apple-darwin"),
        ("Linux", "x86_64", "x86_64-unknown-linux-gnu"),
        ("Linux", "aarch64", "aarch64-unknown-linux-gnu"),
    ] {
        assert!(wf_targets.contains(&target.to_string()), "{target}");
        let out = install(
            &["--version", "9.9.9", "--prefix", pfx.to_str().unwrap()],
            &[
                ("OWL_INSTALL_OS", os),
                ("OWL_INSTALL_ARCH", arch),
                ("OWL_INSTALL_BASE_URL", &base),
            ],
        );
        assert_eq!(out.status.code(), Some(1), "{target}: {}", stderr(&out));
        assert_eq!(
            stderr(&out).trim(),
            format!("owl install: download failed for {base}/owl-9.9.9-{target}.tar.gz"),
            "{target}"
        );
    }
}
