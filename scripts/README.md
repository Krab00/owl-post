# scripts

- `opencode-agent.json` — the `owl-readonly` opencode agent used by the `opencode` harness
  (`opencode run --format json --agent owl-readonly`, design §3/§12). Install by merging the
  `agent` object into your opencode config (`~/.config/opencode/opencode.json`, or a project
  `opencode.json`); opencode also accepts it as a separate file when placed there. Verify with
  `opencode run --agent owl-readonly "hello"`.
- `e2e-real.sh` — the manual loop (ask → consent → allow → draft → send → pull →
  show → cache hit) against a real harness: `OWL_HARNESS=claude|codex|opencode
  scripts/e2e-real.sh`. Logs under `target/e2e-real/<timestamp>/`; not run by CI or
  `cargo test` (design §12).
- `install.sh` — the one-command installer from the README quickstart (`curl -fsSL
  https://raw.githubusercontent.com/Krab00/owl-post/main/scripts/install.sh | sh`): picks the
  target from `uname`, downloads the release tarball and `SHA256SUMS`, verifies, installs to
  `~/.local/bin` (`--version <tag>`, `--prefix <dir>`, `--system`). Test hooks are listed in
  its header; `tests/install_sh.rs` exercises every path offline through a `file://` base.
