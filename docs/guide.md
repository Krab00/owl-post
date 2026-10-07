# User guide

End to end, every command, in the order you will need them.

Running example: **Anna** asks **Brooke** a question about his repository. Anna is the asker,
Brooke is the responder. Both do steps 1 and 2 on their own machine.

Each step gives the terminal command and the Claude Code slash command. Both do the same thing;
the slash command adds a confirmation step and shows the output for you.

---

## 1. Install

### 1.1 Get the binary

```
curl -fsSL https://raw.githubusercontent.com/Krab00/owl-post/main/scripts/install.sh | sh
```

Downloads a release of `owl`, verifies its SHA-256, installs it to `~/.local/bin`.
Flags: `--version <tag>`, `--prefix <dir>`, `--system` (installs to `/usr/local/bin`), `--help`.

You should see the install line and then `owl --version` works.

### 1.2 Everything else in one command

```
owl setup --name "Anna Nolan" --email anna@company.com
/owlpost:setup --name "Anna Nolan" --email anna@company.com
```

Runs 1.3–1.5 for you: `owl init` (skipped when a key exists), `owl install`, then
`claude plugin marketplace add Krab00/owl-post` and `claude plugin install owlpost@owlpost-local`.
`--plugin-source <dir>` installs the plugin from a local checkout (`<repo>/plugins/claude-code`);
`--dry-run` lists the steps. Without `claude` on `PATH` the plugin step is skipped.
You should see your fingerprint, `installed`/`started` for the daemon, and
`set up; run \`owl doctor\``. Then skip to 1.6.

Plugin first, binary second also works: after 1.5 alone, every session starts with
`owlpost: the owl binary is not installed; run /owlpost:setup ...`, and `/owlpost:setup`
runs the installer from 1.1 before `owl setup`. The plugin steps inside `owl setup` are no-ops
for a plugin that is already installed.

### 1.3 Create your identity

```
owl init --name "Anna Nolan" --email anna@company.com
/owlpost:init --name "Anna Nolan" --email anna@company.com
```

Creates the key and config under `~/.config/owlpost`. Without `--name` / `--email` it asks
for each in turn on the terminal (`/owlpost:init` and `/owlpost:setup` ask in the session).
You should see one line: your fingerprint, `owl:` plus 16 characters.
If a key already exists, the command refuses and changes nothing.

### 1.4 Install the daemon

```
owl install
/owlpost:install
```

Registers `owl daemon` as a launchd (macOS) or systemd user (Linux) service, so questions and
answers flow while you are away from the terminal.
You should see `installed <path>` and `started <service>`; when the unit file is already
up to date it is left as is and you see `unchanged <path>` instead of `installed <path>`.
`owl install --dry-run` prints the unit file without installing anything.

### 1.5 Install the Claude Code plugin

```
claude plugin marketplace add Krab00/owl-post
claude plugin install owlpost@owlpost-local --scope user
```

Or from a local checkout: `claude plugin marketplace add <repo>/plugins/claude-code`.
Restart the session afterwards so the hooks register.

Optional: run it as a Claude Code mod. Set `CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1` (in the shell
or under `env` in `~/.claude/settings.json`) and restart. You get an inbox band above the prompt
and a panel with five tabs: Chats, Contacts, New, Card and Settings. `/owlpost:panel`,
`/owlpost:contacts`, `/owlpost:inbox` or `/owlpost:ask` with no arguments opens it, and
running the same command again closes it (so does `q`).
Details: `plugins/claude-code/README.md`, "Claude Code mods".

### 1.6 Check the setup

```
owl doctor
/owlpost:doctor
```

One line per check: `ok|warn|fail  <name>: <detail>`, for key, config, endpoints, harnesses,
daemon, iroh and the last pull. Exits 1 if any check fails.
You should see `ok` on the key, config and daemon lines.

---

## 2. Identity and contacts

### 2.1 See who you are

```
owl whoami
/owlpost:whoami
```

Prints fingerprint, pubkey, name and endpoints.

### 2.2 Hand your peer file to a colleague

```
owl contact export
/owlpost:me
```

Prints your peer file as JSON: `name`, `emails`, `pubkey`, `endpoints`.
Anna sends this to Brooke; Brooke sends his to Anna.
For a team repository you save it as a file instead:
`owl contact export > .agents/peers/anna.json` and open a PR.

### 2.3 Add the colleague

```
owl add brooke.json
/owlpost:add brooke.json
```

Adds a peer file to your global book (`$OWLPOST_HOME/contacts/`). The argument is a path, the
JSON itself, or `-` to read stdin. `--local` writes to the repo's `.agents/peers/` instead.
You should see `added Brooke owl:xxxxxxxx (global)`.
Adding sets no policy. Confirm the fingerprint with Brooke by call or chat before allowing him.

### 2.4 List and inspect contacts

```
owl contact list
owl contact show brooke
/owlpost:contacts
/owlpost:contact show brooke
```

`list` prints NAME, FINGERPRINT, SOURCE (global or local) and POLICY; `--global` or `--local`
restricts the scope. `show` prints one contact as JSON.
`/owlpost:contacts` shows a picker you navigate with the arrow keys.
With Claude Code mods on (§1.5) it opens the owlpost pane instead; run it again to close it.

### 2.5 Remove a contact

```
owl contact remove brooke
/owlpost:contact remove brooke
```

Deletes the contact file from the global book; `--local` removes it from `.agents/peers/`.
You should see `removed Brooke (global)`.

### 2.6 Cards

```
owl card
owl card brooke
/owlpost:card
/owlpost:card brooke
```

Own card with no argument (the running daemon's copy, which also lists the iroh interface),
the peer's card with one (fetched from the first reachable endpoint). The card is an A2A 1.0
agent card: who this is, how to authenticate (`owl-mtls`), the one skill (`ask-about-repo`)
and the three owlpost extensions (identity, repository question, human gate).

### 2.7 Who is online

```
owl presence
owl ping anna
```

`owl presence` reads the daemon's status file: one row per contact — `online`, `offline` or
`-` when the daemon never probed that peer — plus when the last pull ran, how many asks are
open and how many peers were probed. The daemon probes every contact of the book on each pull
loop (not only those a question is waiting on), so `-` shows only until the first loop has
run, or for a contact added since. With no status file at all it exits 4 (is the daemon
running? see `owl install`).

`owl ping` probes one peer right now and prints `Anna is online` or
`Anna is offline: <reason>`; it exits 2 when the peer is offline (like `owl card`), so a
script can gate on it. Both commands take `--json`.

---

## 3. Ask, receive, consent, answer

### 3.1 Anna asks

```
owl ask brooke src/auth/session.rs "why is the refresh token rotated on every read?"
/owlpost:ask brooke src/auth/session.rs why is the refresh token rotated on every read?
```

Sends the question to Brooke's machine. The path is optional; without it the question is about
the whole repository.
You should see `accepted <id> — waiting for the owner's consent` (or `— the owner's agent is
answering` once Brooke has allowed you), or the answer text right away when it came from the
cache or Brooke has you on auto.

Other flags:
- `--wait <secs>` — block and poll for the answer; every change on Brooke's side prints one
  line `<HH:MM> <state>` (`waiting for the owner's consent`, `the owner's agent is
  answering`, `the owner is reviewing the answer`); `declined by Brooke: the owner declined`
  ends the wait with exit code 2; exit code 4 on timeout.
- `--reply-to <id>` — continue an earlier exchange with Brooke (the id of a question you sent
  or of an answer you received): his agent then sees the earlier questions and answers of
  that thread. An unknown id is `no exchange <id>`, another peer's is `<id> was asked to
  <name>, not <peer>`.
- `--context <file>` — attach a snippet (a diff, an error, a file excerpt; `-` reads stdin;
  at most 8192 bytes, else `context is <n> bytes, max 8192`) so Brooke's agent answers the
  question actually being asked. Shown under the question in `owl show`.
- `--file <path>` — propose peers from `git blame` of that file; the path is also the question's path.
- `--peer <peer>` — name the peer instead of picking one.
- `--project <id>` — override the detected project id.
- `--no-cache` — skip the local answer cache (a question with `--context` or `--reply-to`
  skips it anyway).

Failures: `offline` (no endpoint reachable, exit 2), `unavailable` (policy never, exit 2),
`rate limited` (exit 3).

### 3.1a Where does it stand?

```
owl status
owl status <id>
/owlpost:status
```

One row per open question: `ID  PEER  PATH  STATE  SINCE`, the STATE in Brooke's words
(`waiting for the owner's consent`, `the owner's agent is answering`, `the owner is reviewing
the answer`), `offline` when his machine cannot be reached right now. A question Brooke
declined moves to history as `declined`. `--json` prints the raw task objects. Exit code 4
with `no open questions` when nothing is open.

### 3.2 Brooke sees the question

```
owl inbox
/owlpost:inbox
```

Table of ID, FROM, TYPE, STATE, PATH, AGE. Listing marks records seen.
A held question shows a line under the table, with the standing of the signing key in it —
a name is a label, the key is the identity:
`Anna wants to ask your agent about <project> — known key: contact "Anna" — added by hand (global book; fingerprint not verified through a PR) — owl allow <fp> [--once|--always] / owl deny <fp>`.
The message table (`owl show <id> --format claude`) carries the same standing as its first
body row, directly after the rule row:
`| 🔑 known key: contact "Anna" — added by hand (global book; fingerprint not verified through a PR) |`.
`owl inbox --new` lists only unseen records; `owl inbox --count` prints the unseen count without
marking anything.

### 3.3 Read one record

```
owl show <id>
/owlpost:show <id>
```

Prints id, from, type, state, received, project, path and the full question (plus the draft when
there is one). Marks the record seen. `owl show all` prints every inbox record.

### 3.4 Watch for new mail

```
owl watch
/owlpost:watch on
```

`owl watch` blocks until an unseen record arrives, prints its id and exits; `--id <id>` waits for
one record, `--timeout <secs>` gives up with exit code 4.
`/owlpost:watch on|off|status` is the session version: a background poll that prints one line
when the inbox count changes. `off` stops it, `status` reports it.

### 3.5 Consent

```
owl allow anna
/owlpost:allow anna
```

Releases Anna's held questions and sets policy `manual` (you approve each answer).
You should see `allowed Anna (owl:xxxxxxxx): policy manual, released 1 held question`.

```
owl allow anna --once
owl allow owl:k7q2m3xz9pdw4hrt --always
owl deny anna
/owlpost:allow anna --once
/owlpost:allow owl:k7q2m3xz9pdw4hrt --always
/owlpost:deny anna
```

- `--once` releases the held questions and writes no policy; the next question is held again.
- `--always` sets policy `auto`: future questions are answered without asking. It takes the
  fingerprint and nothing else — a name prefix or an e-mail exits 2 with
  `owl allow --always needs the fingerprint, not a name: verify it out-of-band and pass owl:… (this peer: <fp>)`
  and changes nothing. A hand-added (global) contact also needs
  `--i-verified-the-fingerprint`, which you pass only after checking the fingerprint out of
  band. Manual and `--once` still take a name or an e-mail.
- `owl deny` sets policy `never`: held questions are denied, new ones get 403.
  You should see `denied Anna (owl:xxxxxxxx): policy never, 1 held question moved to done`.

### 3.6 Draft the answer

```
owl draft <id>
/owlpost:draft <id>
```

`owl draft <id>` runs the responder harness read-only against this checkout and stores the
draft on the record. You should see the draft text, then `harness: ...`, `redactions: N`,
`state: drafted (<id>)`. `--harness <name>` picks another configured harness. Exit 1 means the
draft was stored but needs a look (timeout or extraction failure).

The checkout comes from the `projects` table of `config.json`: the key is the project the
asker's `owl ask` detected (the origin remote as `host/org/repo`, else the directory name),
the value the local path. `owl project add [path] [--name <key>]` maps a checkout (default:
the current directory, key detected the same way), `owl project list` shows the table
(`missing` marks a checkout that is gone) and `owl project remove <name>` drops a key; the
running daemon picks the change up without a restart. A plain question about a project that
is not in the table is still accepted and answered outside any checkout; a question about a
file, a file request or a tool call needs the project mapped.

`/owlpost:draft <id>` answers in the session instead: `owl draft <id> --prompt` prints the
responder prompt, the plugin hands it to the Agent tool (a read-only subagent on a cheaper
model), and `owl draft <id> --agent --text <reply>` stores the answer, redacted, as
`harness: agent`. `/owlpost:draft <id> --harness <name>` uses the headless harness as above.

### 3.7 Edit the draft

```
owl edit <id>
/owlpost:edit <id>
```

Opens the draft in `$EDITOR` and stores the result; you should see the new text and
`draft updated (<id>)`. `$EDITOR` cannot run inside a Claude Code session, so
`/owlpost:edit` tells you to run it in a terminal instead.

### 3.8 Send it

```
owl send <id>
/owlpost:send <id>
```

Signs the draft, moves it to the outbox and the question to `done/`.
You should see `sent <answer id> (reply to <id>, to <fingerprint>)`.

### 3.9 Or reject it

```
owl reject <id>
/owlpost:reject <id>
```

Discards the record; Anna gets no answer. You should see `rejected <id>`.

### 3.10 Anna reads the answer

The answer lands in Anna's inbox as a record of type `answer`:

```
owl inbox
owl show <id>
/owlpost:inbox
/owlpost:show <id>
```

### 3.11 The whole conversation with one person

The inbox is a pile of separate records; a thread is one person. `owl thread` lists everyone
who has ever written, newest conversation first, with how many of their questions are still
unseen and how many are still open:

```
owl thread
/owlpost:thread
```

Naming a person prints that conversation in one chronological order, in both directions: what
they asked you, what you asked them, and what happened to every request on the way — held for
consent, allowed, drafted by which harness, edited, sent, rejected. Their messages come as the
familiar table; your own words come in a plain text block.

```
owl thread Anna
owl thread Anna --since 7d
/owlpost:thread Anna
```

`owl thread` only reads. It never answers anything, never marks a record seen and never
changes a policy — use `/owlpost:inbox` to act on an open question you find there.

### 3.11a Archive, delete, undo

A conversation that is over does not have to stay on the list. `owl archive` hides one
person's chat from `owl thread` — with `--context`, only one thread of it — and `owl thread
--archived` shows exactly what you hid. Nothing moves; unarchiving brings it back:

```
owl archive Anna
owl archive Anna --context <id>
owl unarchive Anna
owl thread --archived
```

The marks live in `$OWLPOST_HOME/archive.json`; every change first keeps the previous file as
`archive.json.bak`. If `archive.json` is ever damaged, the commands fall back to that backup
with a warning — or to nothing archived when there is no usable backup.

`owl delete` goes further: the records move out of the spool into `$OWLPOST_HOME/trash/`, one
stamped batch per call. The daemon removes a batch once it is older than 30 days; until then
`owl undo` puts the newest batch back, byte for byte, and a second `owl undo` the one before
it:

```
owl delete Anna
owl delete Anna --context <id>
owl undo
```

If a record arrived again at its old place since the delete, `owl undo` refuses and names the
file rather than overwrite it.

### 3.12 Ask for a file

Sometimes you do not want an answer about a file — you want the file. `owl request` asks a
colleague for one concrete path at one ref of a project they own, or for one entry of the
memory store they configured:

```
owl request Brooke github.com/company/monorepo src/auth/session.rs
owl request Brooke github.com/company/monorepo src/auth/session.rs --ref v2.1.0
owl request Brooke --memory decisions/2026-08-refresh-token.md
/owlpost:request Brooke github.com/company/monorepo src/auth/session.rs
```

Three things are always true, and they are the point of the feature:

- **Brooke approves every single one by hand.** Even if he set your policy to `auto`, a
  content request is held for his consent. There is no setting that sends a file out
  unattended.
- **Only what he opted into is reachable.** A path inside a checkout he listed in
  `projects`, or a key inside his `memory_root` — anything else is refused by his daemon
  before it reaches his inbox at all.
- **Nothing runs on his machine when the request arrives.** His daemon spools it; the file is
  read only when he runs `owl draft`, and his redaction patterns run over the content exactly
  as over an answer.

On his side the request shows up in `owl inbox` like a question, `owl show <id>` prints what
you asked for, `owl draft <id>` reads the file (no agent, no model) and prints how many bytes
and how many redactions it came to, and `owl send <id>` is the moment it leaves.

You get it back in your inbox:

```
owl show <reply id>
```

which prints the content and one line telling you the digest matched. Files over 256 KiB
arrive cut, and the line says so — the digest is still of the whole file, so you can tell.
Nothing is written into your checkout: what you do with the content is yours to decide.

### 3.13 Let a colleague run one of your tools

Sometimes the answer is not in a file at all: it is what a command prints on the machine that
has the checkout. `owl call` asks a colleague to run one tool **they** configured:

```
owl call Brooke test --input ./in.json
owl call Brooke build --input - --project github.com/company/monorepo
/owlpost:call Brooke test --input ./in.json
```

The input file is a JSON object; it reaches the tool on its standard input and never as a
command-line argument. Four things are always true:

- **Only tools Brooke listed exist.** The registry lives in his `responder.tools` and is
  empty until he adds one by hand. A name he did not configure comes back as `unknown tool`,
  and so does every name when he configured none — on purpose, so nobody can map his tools
  by guessing.
- **Nothing runs when the request arrives.** His daemon spools it and stops. Even `owl allow`
  runs nothing.
- **He starts the run himself.** `owl draft <id>` is the command that executes the tool, and
  he types it. It prints `ran test in 4.1s — exit 0, 3120 bytes, 1 redaction` and the output,
  which his redaction patterns have already been over.
- **He decides whether you see it.** `owl send <id>` is the moment the output leaves.

You get it back in your inbox, exit code and all:

```
owl show <reply id>
```

A non-zero exit is an answer, not an error: a failing build is exactly the thing you asked
about.

---

## 4. History

```
owl history
/owlpost:history
```

Finished exchanges: ID, PEER, TYPE, STATE, PATH, RECEIVED.

Filters:

```
owl history --peer brooke
owl history --path "src/auth/*.rs"
owl history --since 3d
```

- `--peer` matches the fingerprint exactly or the contact name, case-insensitive.
- `--path` is a glob with `*` and `?` only.
- `--since` takes RFC3339 (`2026-09-01T10:00:00Z`) or `<N>d`, `<N>h`, `<N>m`.

Full content of one exchange:

```
owl show <id>
```

Check history before asking a question that may already be answered.

---

## 5. Maintenance

### 5.1 Update

```
owl update
/owlpost:update
```

Replaces the `owl` binary, restarts the daemon (re-registering it only when its service
unit changed) and reinstalls the Claude Code plugin.
You should see `updated; restart your Claude Code session to load the plugin`.
`--source <dir>` builds from a local checkout instead of downloading a release;
`--dry-run` prints the commands it would run.

### 5.2 Uninstall the service

```
owl uninstall
/owlpost:uninstall
```

Stops and removes the daemon service; questions and answers stop flowing until you run
`owl install` again. You should see `stopped <service>` and `removed <path>`.

### 5.3 Harnesses

The harnesses are the agent CLIs that draft answers for you; the table lives in
`config.json` and `responder.harness` names the one that drafts.

```
owl harness list          # one row per harness: NAME, DRAFTS, FOUND, COMMAND
owl harness scan          # add the known harnesses found on PATH that the config lacks
owl harness add <name> [--answer-path <p>] -- <cmd>...
owl harness edit <name> -- <cmd>...
owl harness remove <name>
owl harness use <name>    # the harness that drafts
```

The command is everything after `--`; `{prompt}` inside it is where the question goes.
`--answer-path` (default `raw`) says where the answer text sits in the harness output.
Every command accepts `--json`.

---

## Global flags

Every command accepts:

- `--home <dir>` — use another owlpost home (overrides `$OWLPOST_HOME`).
- `--json` — machine-readable output.
- `-q`, `--quiet` — suppress non-essential output.

Exit codes: 0 ok, 1 user or data error, 2 offline or unavailable, 3 rate limited,
4 nothing to do (a `watch` or `--wait` timeout).
