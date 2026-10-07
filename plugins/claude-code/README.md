# owlpost plugin for Claude Code

The Claude Code harness adapter for owlpost (`docs/concept.md` "Harness adapters",
`docs/architecture.md` §3.3 and §3.6). It ships:

| Piece | Path | What it does |
|---|---|---|
| Hooks | `hooks/hooks.json`, `hooks/owl-count.sh` | On `SessionStart`, `UserPromptSubmit` and `PostToolUse` runs `owl inbox --count --format claude` (5 s timeout) and injects the unseen-question counter line; `SessionStart` also registers this session's private wake directory `$OWLPOST_HOME/sessions/<session_id>/wake` as a `watchPaths` entry, and the `FileChanged` hook (`asyncRewake`) wakes the idle session — exit 2 with the instruction line and the record's metadata (never the peer's text) — when the daemon routes a record to it (one session per record); `SessionEnd` removes the directory. Silent no-op when `owl` is missing or fails. |
| Skill | `skills/owlpost/SKILL.md` | When to ask a peer, how to run `owl ask`, the mentioned contact, how to react to the counter, the answer loop, the memory rule. |
| MCP server | `owl mcp`, registered at user scope by `owl setup` / `owl update` | Serves every contact as an `@owl:to://…` resource in the `@` typeahead (stdio); see "Mention a contact" below. |
| Commands | `commands/<name>.md`, one per `owl` subcommand (all except `daemon`) | `/owlpost:<name>` runs `owl <name>` with the arguments and offers the next step; see the table below. |
| Mod (optional) | `hooks/owlpost.tsx`, listed under `modules` in `hooks/hooks.json` | Loaded only when Claude Code mods (function hooks) are on; see "Claude Code mods" below. |
| Manifests | `.claude-plugin/plugin.json`, `.claude-plugin/marketplace.json` | Plugin metadata; a one-plugin marketplace so the directory can be added from a local path. |

## Commands

Every `owl` subcommand except `daemon` (a service: `install`, `uninstall` and `doctor`
cover it) and `mcp` (started by Claude Code, not by hand) is one `commands/<name>.md`. Each is a thin wrapper: run the command with the
arguments, show the output, offer the natural next step. `send`, `allow`, `deny`, `reject`
and `uninstall` ask for one explicit confirmation with `AskUserQuestion` before running.
`allowed-tools` in each file is limited to its own `owl <sub>:*` pattern (plus the `owl`
patterns of the next steps a file runs itself); `tests/plugin.rs` pins the rule.

| Command | File | Runs |
|---|---|---|
| `/owlpost:init [--name <name>] [--email <email>]` | `commands/init.md` | `owl init` — create home, key, config |
| `/owlpost:whoami` | `commands/whoami.md` | `owl whoami` — identity summary |
| `/owlpost:me` | `commands/me.md` | `owl contact export` — own peer file to hand to a colleague |
| `/owlpost:card [peer]` | `commands/card.md` | `owl card` — own card, or fetch a peer's |
| `/owlpost:presence` | `commands/presence.md` | `owl presence` — per-contact online/last seen plus the daemon's last pull summary |
| `/owlpost:ping <peer>` | `commands/ping.md` | `owl ping` — probe one peer right now |
| `/owlpost:contacts` | `commands/contacts.md` | `owl contact list --json` — the book as one table, then the hint to mention a contact with `@owl:to://…` |
| `/owlpost:contact <show\|export\|remove> ...` | `commands/contact.md` | `owl contact show|export|remove` |
| `/owlpost:add <peer json\|file> [--local]` | `commands/add.md` | `owl add` — add a peer file |
| `/owlpost:allow <peer> [--once\|--always]` | `commands/allow.md` | `owl allow` — release held questions, set policy (confirms first) |
| `/owlpost:deny <peer>` | `commands/deny.md` | `owl deny` — policy never (confirms first) |
| `/owlpost:ask <peer> [path] <question>` | `commands/ask.md` | `owl ask` — send a question at once; the command is the approval, no confirmation step; `--reply-to <id>` continues a thread, `--context <path>` attaches a snippet |
| `/owlpost:request <peer> <project> <path>` | `commands/request.md` | `owl request` — ask for one file at a ref, or one memory entry (`--memory <key>`); always held for the owner's consent |
| `/owlpost:call <peer> <tool> --input <file>` | `commands/call.md` | `owl call` — ask a peer to run one tool of their registry; nothing runs there until a human allows it and runs `owl draft` |
| `/owlpost:status [id]` | `commands/status.md` | `owl status` — where every open question stands, in the peer's words (`waiting for the owner's consent`, …) |
| `/owlpost:inbox` | `commands/inbox.md` | `owl inbox --json` — walk the records: question verbatim, consent picker (allow once/always, deny), draft / draft & send / reject / skip picker, verbatim draft, send/edit/reject picker. With mods on, the same command opens the pane's Chats instead |
| `/owlpost:show <id>` | `commands/show.md` | `owl show` — full record, offer draft/send/reject |
| `/owlpost:reply [text]` | `commands/reply.md` | `owl draft` on the question last shown in this session (else the newest pending one): `text` is stored as the human's own answer (`--text`), no text drafts through the Agent tool (`owl draft --prompt` → subagent → `owl draft --agent --text`); the draft is shown verbatim, then Send / Save as draft / Edit / Reject |
| `/owlpost:draft <id> [--harness <name>] [--send]` | `commands/draft.md` | `owl draft --prompt` → the Agent tool (sonnet, read-only) → `owl draft --agent --text`; `--harness` runs a headless harness instead; show the draft; `--send` then runs `owl send` at once |
| `/owlpost:edit <id>` | `commands/edit.md` | `owl edit` opens `$EDITOR`, which cannot run in a session; explains the alternatives |
| `/owlpost:send <id>` | `commands/send.md` | `owl send` — sign and move to outbox (confirms first) |
| `/owlpost:reject <id>` | `commands/reject.md` | `owl reject` — discard a record (confirms first) |
| `/owlpost:thread [<peer>]` | `commands/thread.md` | `owl thread` — one person's whole conversation |
| `/owlpost:archive <peer> [--context <id>]` | `commands/archive.md` | `owl archive` — hide a chat (or one thread) from `owl thread` |
| `/owlpost:unarchive <peer> [--context <id>]` | `commands/unarchive.md` | `owl unarchive` — bring an archived chat (or thread) back |
| `/owlpost:delete <peer> [--context <id>]` | `commands/delete.md` | `owl delete` — move a chat (or one thread) to the trash (confirms first); `/owlpost:undo` restores it |
| `/owlpost:undo` | `commands/undo.md` | `owl undo` — restore the newest trash batch |
| `/owlpost:history [--peer] [--path] [--since]` | `commands/history.md` | `owl history` — finished exchanges |
| `/owlpost:panel` | `commands/panel.md` | with mods on, opens the panel (or closes it while it is open); without mods it only says that the panel needs them |
| `/owlpost:watch [on\|off\|status]` | `commands/watch.md` | event-driven inbox watch, nothing to arm: the `SessionStart` hook registers this session's wake directory (`$OWLPOST_HOME/sessions/<session_id>/wake`) as a watch path and the `FileChanged` hook wakes the session with the record's metadata when the daemon routes a record to it; `off` stores `{"watch": false}` in `$OWLPOST_HOME/plugin.json` (the hook stops waking at once, new sessions do not watch), `on` restores it from the next session start, `status` reports the stored default |
| `/owlpost:setup [--name] [--email] [--plugin-source] [--dry-run]` | `commands/setup.md` | `owl setup` — init, daemon, plugin in one go |
| `/owlpost:install [--dry-run]` | `commands/install.md` | `owl install` — register the daemon service |
| `/owlpost:uninstall` | `commands/uninstall.md` | `owl uninstall` — remove the service (confirms first) |
| `/owlpost:stop` | `commands/stop.md` | stop owlpost without uninstalling: the mod closes the panel (with mods on), `{"watch": false}` in `plugin.json` as `/owlpost:watch off`, then `owl stop` — stops the daemon service and keeps its unit |
| `/owlpost:start` | `commands/start.md` | the mirror of `/owlpost:stop`: `{"watch": true}` as `/owlpost:watch on`, then `owl start` |
| `/owlpost:doctor` | `commands/doctor.md` | `owl doctor` — check the setup, offer the fix per failure |
| `/owlpost:harness <list\|scan\|add\|edit\|remove\|use> ...` | `commands/harness.md` | `owl harness` — the harness table of config.json |
| `/owlpost:project add [path] [--name <key>] \| list \| remove <name>` | `commands/project.md` | `owl project` — the projects table of config.json (which checkout answers for which project) |
| `/owlpost:update [--source <dir>]` | `commands/update.md` | `owl update` — replaces the running binary in place, daemon, plugin |

## Mention a contact

Type `@owl:` and the start of the contact's name (or e-mail, or domain) in the prompt, for
example `@owl:al`: the `owl` MCP server lists every contact of the merged book as a
resource `to://<name-slug>.<email>`, so the typeahead ranks `owl:to://alex-…` near the
top; `@owl:` alone lists the whole book. Pick with the arrows and type the question, for
example `@owl:to://ana-kowalska.ana@acme.com src/auth/session.rs why does this retry?`; the
skill runs `owl ask --peer <fingerprint>` at once, the mention is the approval. (`@al`
without the `owl:` prefix never beats files and connectors: that ranking is Claude Code's.)

The server is registered in Claude Code at user scope by `owl setup` and `owl update`
(idempotent: `mcp server owl already registered` when it is there), or by hand with
`claude mcp add --scope user owl -- owl mcp`. It runs `owl mcp` over stdio, so it needs
`owl` on `PATH` like the hooks; it is resources-only and adds no tools to the model
context. `owl doctor` reports it as the `mcp` check (`warn` with the `claude mcp add` line
when it is missing). The plugin bundles no server of its own: one bundled through a plugin
is named `plugin:owlpost:owl`, and that prefix sinks the contacts in the typeahead ranking.

## Claude Code mods

The plugin also works as a Claude Code mod (function hooks, early access). Turn mods on by
setting `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1`, either in the shell that starts Claude Code or
under `env` in `~/.claude/settings.json`, and then restart the session. Without the variable,
the plain plugin above keeps working unchanged.

With mods on, `hooks/owlpost.tsx` adds:

- a segment in the line under the prompt with the unseen count (`owlpost 0` included); a
  click on it opens the pane. Your own status line and `settings.json` are not touched;
- a band above the prompt, only while something waits (`1 question waiting`), with one
  **Open panel** button (it replaces the counter line the classic hook injects).
  `{"band": false}` in `$OWLPOST_HOME/plugin.json` turns it off;
- live watch off by default: the `FileChanged` wake is armed only when `plugin.json` has
  `{"watch": true}` (`/owlpost:watch on`), so a new message shows in the band instead of
  starting a model turn. Without mods the default stays on;
- a pane opened by `/owlpost:panel`, `/owlpost:contacts`, `/owlpost:inbox` or `/owlpost:ask`
  with no arguments (`/owlpost:panel` and `/owlpost:inbox` open Chats, `/owlpost:contacts`
  Contacts, `/owlpost:ask` New). It runs `owl` directly, with no model turn and no tokens.

The pane has five tabs — `1` Chats, `2` Contacts, `3` New, `4` Card, `5` Settings — a list of
keys on `h`, and `q` closes it. Every screen ends with a one-line note (the last action's
result) and the key hints. Colours are Claude Code theme keys, so they follow your theme.

- **Chats**: one row per person; a row opens that person's threads (grouped by thread id), a
  thread opens its messages, and **Send** replies in it (`u` takes the text from the prompt
  box, `p` adds a path, `f` a context file). A thread with a request waiting for you opens the
  incoming-request screen: Allow once / Allow always / Deny (each naming the fingerprint),
  Draft (a question goes to `/owlpost:draft`), Own answer, Edit, Send, Reject. A tool-call
  request gets **Run (owl draft)** instead of Draft: it runs the tool the peer named on this
  machine, with no model turn, and the note line reports the exit code.
- **Contacts**: the contact book; the selected contact opens its chat or its card, sets its
  policy (auto / manual / never) or is removed after a confirmation; `n` adds a peer file.
- **New**: ask a question, request a file or call a tool, as a new thread or continuing one.
- **Card**: your card, **Copy peer file** for a colleague, and a colleague's card by name.
- **Settings**: Live watch and the band (stored in `plugin.json`), doctor, install and
  uninstall the daemon (uninstall asks first and has no key), update.

Short ids on screen are the last six hex characters of the full id. To close the pane, run
the same command again or press `q`. Esc only returns focus to the prompt.

## Requirements

- `owl` on `PATH` (`cargo install --path .` from the repo root, or the release binary).
  The hook prints nothing when `owl` is not found, so the plugin loads without it but the
  counter stays silent.
- An initialised home: `owl init`, then contacts via `owl add <peer-file>` (global book;
  `--local` for the repo's `.agents/peers/`) and `owl allow`.
- The `owl daemon` running as a service so questions and answers actually flow:
  `owl install` registers the launchd/systemd unit (`owl install --dry-run` shows what it
  would write). Check with `owl doctor`.

## Install

In one go (also creates the identity, installs the daemon and registers the `owl` MCP
server): `owl setup`, or
`owl setup --plugin-source /absolute/path/to/owlpost/plugins/claude-code` from a checkout.

Plugin first: install the plugin with the two `/plugin` commands below, then `/owlpost:setup`
runs the release installer (`scripts/install.sh`) when `owl` is missing and `owl setup` after
it; until then the `SessionStart` hook prints one line saying the binary is not installed.

By hand from a local checkout:

```
/plugin marketplace add /absolute/path/to/owlpost/plugins/claude-code
/plugin install owlpost@owlpost-local
```

From the repository (`.claude-plugin/marketplace.json` at the repo root points at this directory):

```
/plugin marketplace add Krab00/owl-post
/plugin install owlpost@owlpost-local
```

`owlpost-local` is the marketplace name in `.claude-plugin/marketplace.json`; the plugin
`source` there is `./`, so the plugin directory is its own marketplace. Restart the session
(or `/plugin` → reload) after installing so the hooks register.

The plugin version in `.claude-plugin/plugin.json` is copied by hand from `Cargo.toml`;
`tests/plugin.rs` fails when they drift.

## Manual smoke run

Automated tests cover the hook script (`cargo test --test plugin`). Whether Claude Code
actually shows the injected line has to be observed once by hand:

1. Build and put `owl` on `PATH`: `cargo install --path .` (or symlink `target/debug/owl`).
2. Use a throwaway home so the real inbox is untouched, and seed it with the fixture the
   integration tests use (it writes key, config, one contact "Martin" and two unseen
   questions from Martin, then prints the expected counter line):

   ```
   export OWLPOST_HOME=$(mktemp -d)
   cargo test --test plugin seed_home_for_smoke -- --ignored --nocapture
   owl inbox --count --format claude     # must print one JSON line
   ```

3. Keep `OWLPOST_HOME` exported in the shell that starts Claude Code; the hook inherits it.
4. Install the plugin from the local path (commands above) in a Claude Code session started
   with the same `OWLPOST_HOME` exported.
5. Start a new session (`claude`) in any directory and send any prompt. The `SessionStart`
   and `UserPromptSubmit` hooks fire; the counter line
   `owlpost: 2 new questions (Martin 2). Say "show owlpost inbox" or run `owl inbox`.`
   must appear in the injected context (visible in the transcript with `Ctrl+O`, or ask
   Claude "what did the owlpost hook inject?").
6. Say "show owlpost inbox"; Claude should run `owl inbox` and list the two rows.
