# owlpost

owlpost lets you ask a colleague's coding agent a question about their code. You send the
question from your terminal or from Claude Code; it lands on your colleague's machine, their own
agent reads their checkout and drafts an answer, and **they approve it before it comes back to
you**. The machines talk directly to each other over mutual TLS — there is no central server,
no shared memory and no account to create.

- [Concept](docs/concept.md) — what it is, what it is not, the decisions and why
- [Architecture](docs/architecture.md) — components, flows, trust model, protocol
- [Technical design](docs/technical-design.md) — crate layout, data formats, CLI and HTTP contracts
- [User guide](docs/guide.md) — every command end to end, `owl` and `/owlpost:` side by side

Status: pre-alpha. Rust, single static binary `owl`.

---

## How to use it

Every step has a terminal command (`owl …`) and, where it exists, a Claude Code slash command
(`/owlpost:…`). They do the same thing; the slash command runs it for you inside the session.

### 1. Install

```
curl -fsSL https://raw.githubusercontent.com/Krab00/owl-post/main/scripts/install.sh | sh
```

Downloads a release of `owl`, verifies its SHA-256 and installs it to `~/.local/bin`
(`--version <tag>`, `--prefix <dir>`, `--system` for `/usr/local/bin`). macOS and Linux,
x86_64 and aarch64.

Then one command does the rest — create your key, register the background service, install the
Claude Code plugin:

```
owl setup --name "Your Name" --email you@company.com
```

Plugin first works too: install the plugin in Claude Code and let it fetch the binary.

```
/plugin marketplace add Krab00/owl-post
/plugin install owlpost@owlpost-local
/owlpost:setup --name "Your Name" --email you@company.com
```

Check everything:

```
owl doctor          # /owlpost:doctor
```

One line per check (key, config, endpoints, harnesses, daemon). All green means you can receive
questions. `owl update` replaces the binary and the plugin later on.

Optional: owlpost also runs as a **Claude Code mod**. Set `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1`
and restart Claude Code. You get an inbox band above the prompt and a pane
(`/owlpost:contacts`, `/owlpost:inbox`, or `/owlpost:ask` with no arguments) for browsing
contacts, asking and answering without spending model tokens. Run the same command again to
close the pane. See
[the plugin README](plugins/claude-code/README.md#claude-code-mods).

### 2. Exchange identities

You and your colleague swap a small JSON "peer file" (name, e-mail, public key, endpoints).

```
owl contact export          # /owlpost:me      — your own peer file, send it to them
owl add brooke.json         # /owlpost:add     — add theirs (a path, the JSON itself, or -)
owl contact list            # /owlpost:contacts
owl whoami                  # /owlpost:whoami  — your fingerprint
```

Adding a contact sets no permission. Compare the fingerprint (`owl:` + 16 characters) with them
by call or chat before you allow them.

In a repository you can also commit peer files to `.agents/peers/` (`owl add --local`) so the
whole team shares one contact list.

### 3. Decide who may ask you

```
owl allow brooke            # /owlpost:allow brooke   — release his held questions, you approve each answer
owl allow brooke --once     # release what is held, no permanent policy
owl allow owl:k7q2m3xz9pdw4hrt --always   # answer his questions automatically — takes his fingerprint, not a name (see Security)
owl deny brooke             # /owlpost:deny brooke    — refuse; new questions get a 403
```

The first question from an unknown peer is held until you allow or deny.

### 4. Ask

```
owl ask brooke src/auth/session.rs "why is the refresh token rotated on every read?"
/owlpost:ask brooke src/auth/session.rs why is the refresh token rotated on every read?
```

The path is optional; without it the question is about the whole repository. Useful flags:

- `--wait 120` — block and print each state change until the answer arrives.
- `--reply-to <id>` — continue an earlier exchange, so their agent sees the thread.
- `--context <file>` — attach a diff, an error or an excerpt (max 8192 bytes; `-` reads stdin).
- `--file <path>` — propose peers from `git blame` of that file.

While you wait:

```
owl status                  # /owlpost:status  — where each open question stands
```

States are in the owner's words: *waiting for the owner's consent*, *the owner's agent is
answering*, *the owner is reviewing the answer*.

### 5. Answer someone

```
owl inbox                   # /owlpost:inbox — table of ID, FROM, TYPE, STATE, PATH, AGE
owl show <id>               # /owlpost:show  — the full question
owl draft <id>              # /owlpost:draft — your agent answers read-only from this checkout
owl edit <id>               # /owlpost:edit  — open the draft in $EDITOR (terminal only)
owl send <id>               # /owlpost:send  — sign it and put it in the outbox
owl reject <id>             # /owlpost:reject — discard it, the peer gets no answer
```

`/owlpost:inbox` walks the whole loop for you: it shows each question, handles consent, and
offers send / edit / reject. `/owlpost:reply "<your text>"` answers the question last shown
in the session in your own words instead of using the agent's draft.

Nothing leaves your machine until you run `send`.

### 6. Look things up later

```
owl history                        # /owlpost:history
owl history --peer brooke
owl history --path "src/auth/*.rs"
owl history --since 3d
```

---

## Command reference

Every command also takes `--home <dir>`, `--json` and `-q/--quiet`.

| Command | Slash command | Example |
|---|---|---|
| `owl setup` | `/owlpost:setup` | `owl setup --name "Anna" --email anna@company.com` |
| `owl init` | `/owlpost:init` | `owl init --name "Anna" --email anna@company.com` |
| `owl install` | `/owlpost:install` | `owl install --dry-run` |
| `owl uninstall` | `/owlpost:uninstall` | `owl uninstall` |
| `owl update` | `/owlpost:update` | `owl update` |
| `owl doctor` | `/owlpost:doctor` | `owl doctor` |
| `owl whoami` | `/owlpost:whoami` | `owl whoami` |
| `owl card [peer]` | `/owlpost:card` | `owl card brooke` |
| `owl contact export` | `/owlpost:me` | `owl contact export > anna.json` |
| `owl contact list` | `/owlpost:contacts` | `owl contact list --global` |
| `owl contact show <peer>` | `/owlpost:contact show` | `owl contact show brooke` |
| `owl contact remove <peer>` | `/owlpost:contact remove` | `owl contact remove brooke --local` |
| `owl add <source>` | `/owlpost:add` | `owl add brooke.json --local` |
| `owl allow <peer>` | `/owlpost:allow` | `owl allow owl:k7q2m3xz9pdw4hrt --always --i-verified-the-fingerprint` |
| `owl deny <peer>` | `/owlpost:deny` | `owl deny brooke` |
| `owl ask <peer> [path] <question>` | `/owlpost:ask` | `owl ask brooke src/db.rs "why the retry loop?"` |
| `owl request <peer> <project> <path>` | `/owlpost:request` | `owl request brooke github.com/co/mono src/db.rs --ref main` |
| `owl status [id]` | `/owlpost:status` | `owl status` |
| `owl inbox` | `/owlpost:inbox` | `owl inbox --new` |
| `owl show <id>` | `/owlpost:show` | `owl show 0192f3a1` |
| `owl draft <id>` | `/owlpost:draft` | `owl draft 0192f3a1 --harness claude` |
| `owl edit <id>` | `/owlpost:edit` | `owl edit 0192f3a1` |
| `owl send <id>` | `/owlpost:send` | `owl send 0192f3a1` |
| `owl reject <id>` | `/owlpost:reject` | `owl reject 0192f3a1` |
| `owl history` | `/owlpost:history` | `owl history --peer brooke --since 7d` |
| `owl watch` | `/owlpost:watch` | `owl watch --timeout 300` |
| — | `/owlpost:reply` | `/owlpost:reply we rotate it because of CVE-2024-…` |
| `owl daemon` | — | `owl daemon` (normally run by the service) |
| `owl route <id>` | — | `owl route 0192f3a1` (what the daemon does on arrival) |
| `owl mcp` | — | `owl mcp` (contacts as `@owl:to://…` mentions) |

Exit codes: 0 ok, 1 user or data error, 2 offline or unavailable, 3 rate limited, 4 nothing to do.

---

## How it works

A small background service (`owl daemon`, registered with launchd or systemd by `owl install`)
listens on your machine and keeps a file spool: an inbox for what arrives, an outbox for what
leaves. Questions go out immediately over mutual TLS; answers are pulled back later, so both
people never have to be online at the same time.

```
  you                              your colleague's machine
  ---                              ------------------------
  owl ask  --mTLS-->  daemon  -->  inbox (held until consent)
                                     |  owl allow / /owlpost:inbox
                                     v
                                   read-only headless agent
                                   reads the repo checkout
                                     |
                                     v
                                   draft  ->  owner reviews  ->  owl send
                                                                   |
  owl inbox  <-------  daemon pulls the signed answer  <------------+
```

The daemon never runs a model on arrival in manual mode — it is a mailbox with a signature
check. Only `owl draft` (or an auto-accepted peer) starts the answering agent. No central
server is involved; the two machines talk to each other directly.

---

## What people use it for

- **Onboarding to a teammate's module.** "What is the entry point of the billing service, and
  which parts am I allowed to call directly?" — instead of a 30-minute call.
- **Understanding a PR you have to review.** Attach the diff:
  `owl ask martin --context pr.diff "is this the right place to invalidate the cache?"`
- **Operational facts nobody wrote down.** `owl ask owen "which branch do you deploy to prod from?"`
- **An error you cannot place.** `owl ask brooke --context error.log "our client gets 409 from
  your API on retries — is that expected?"`
- **Cross-team API questions.** Ask the owning team's agent about their contract instead of
  guessing from their OpenAPI file.
- **A follow-up on an earlier answer.** `owl ask brooke --reply-to <id> "and what about the
  mobile client?"` keeps the thread, so their agent sees the earlier exchange.

Answers land in your inbox and, through the Claude Code plugin, in your agent's memory — so the
same question does not have to be asked twice. `owl history` is worth a look before you ask.

---

## Security

Found a vulnerability? Report it privately — see [SECURITY.md](SECURITY.md).

- **Identity is a key, not an account.** `owl init` creates an ed25519 keypair. Your fingerprint
  is `owl:` plus 16 characters of the hash of your public key; you compare it with your colleague
  out of band before trusting them.
- **Mutual TLS between machines.** TLS 1.3 with self-signed certificates generated from that
  key; each side compares the certificate's public key with the one pinned in its contact list.
  An unknown key is refused at the handshake. Every message body is also ed25519-signed.
- **Consent per person.** Policy is `manual` (default after `owl allow` — you approve each
  answer), `auto` (`--always`, answers go out without asking; a hand-added contact additionally
  needs `--i-verified-the-fingerprint`), or `never` (`owl deny` — new questions get 403, worded
  the same as being offline).
- **The answering agent is read-only.** It runs headless in a separate session with read tools
  only — no shell, no write tools, no network. The worst outcome of a malicious question is a
  bad answer.
- **A human approves before anything leaves.** In manual mode the draft is stored and shown to
  you; it is sent only by `owl send`. In auto mode redaction regexes run over the draft and
  every outgoing answer is logged.
- **What a peer can see.** Only what their question's answer contains, drawn from your repo
  checkout and project-level files (`CLAUDE.md` / `AGENTS.md`). Your private memory directory is
  opt-in; session transcripts are out of scope. A peer never gets shell access, file listings or
  anything you did not approve.
- **Limits.** `--context` attachments are capped at 8192 bytes. Each peer has a rate limit
  (default 20 questions/hour, then `429`). Replayed or stale messages (more than 5 minutes off)
  are rejected. Incoming text is treated as data, never as instructions.

---

## License

owlpost is licensed under the GNU General Public License, version 3 or later
(GPL-3.0-or-later). You may use, modify and redistribute it; derived works must stay under the
same license. The full text is in [LICENSE](LICENSE).
