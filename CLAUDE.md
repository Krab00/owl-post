# CLAUDE.md

owlpost (`owl`): a CLI + local daemon that lets one developer's coding agent ask another's
agent about their code, peer to peer, with the owner's consent on every answer. Rust binary in
`src/`, Claude Code plugin (panel, commands, skill) in `plugins/claude-code/`. Design docs in
`docs/` (`concept.md` → `architecture.md` → `technical-design.md` → `guide.md`).

## Commands

```sh
cargo build
cargo test --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt --check
claude plugin test plugins/claude-code      # panel + command tests
```

CI runs exactly these four cargo commands. Run one cargo build at a time.

## Rules

- Every change goes through a branch and a PR. Never commit to `main`.
- A change users should receive bumps `version` in `Cargo.toml`; merging to `main` with a new
  version publishes the release (`owl update` picks it up).
- Never add private or sensitive data to the repo: no e-mails, real names, fingerprints,
  public or private keys, tokens, home paths (`/Users/...`, `/home/...`), chat or inbox
  content, not in code, tests, fixtures, docs, PR text or commit messages. Use placeholders
  (`alice@example.com`, `/code/app`). `scripts/check-private.sh` refuses such content: it runs
  as the pre-commit hook (`git config core.hooksPath scripts/githooks`, once per clone) and in
  CI. Do not bypass it with `--no-verify`.
- Minimal diffs in the existing style. No speculative abstractions or configurability.
- Non-trivial logic ships with a test. Fixtures must not touch the real home (`OWLPOST_HOME`,
  `XDG_CACHE_HOME` point at a tempdir in tests).
- Security boundaries stay: a peer's request never reads a file or runs anything before the
  owner's consent, a harness never gets a cwd inside the owlpost home, every input from a
  peer is validated at the daemon before it reaches the inbox.
- Panel code follows the approved mock 1:1; CLI output wording is part of the contract
  (the plugin and tests match it verbatim).
