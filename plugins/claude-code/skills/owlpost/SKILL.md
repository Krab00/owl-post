---
name: owlpost
description: Ask a colleague's local agent about code they own and answer their questions from this checkout with owlpost (`owl ask`, `owl inbox`, `owl draft`, `owl send`). Use when the user names a colleague, asks who to ask about a file, mentions owlpost, or when a hook injected an "owlpost: N new questions" or "N new answers" line.
---

# owlpost

owlpost lets this machine ask a peer's local agent about code the peer owns, and lets the
peer ask about code owned here. Every message crosses the wire only after a human has read
it. The `owl` CLI is the only interface; never edit files under `$OWLPOST_HOME` directly.

## Ground rule

Sending anything to a peer requires explicit human approval. Never run `owl send` or
`owl ask` on your own initiative; describe what would go out, wait for a clear yes, then
run the command. An `/owlpost:ask` the user typed, a mentioned `@owl:to://` contact, or the
user asking in their own words ("ask Martin why this retries") is that approval: run it
without a confirmation step.

## Subagent only

Every owlpost operation — listing or showing the inbox, drafting, sending, contacts, setup,
doctor, all of it — runs in a subagent: the `Agent` tool (`subagent_type: general-purpose`,
`model: sonnet`) gets the command's steps and returns the `owl` output verbatim; the main
session pastes it unchanged and runs `AskUserQuestion` where a pick is needed, then hands the
pick to a new `Agent` call. The main model never runs `owl` itself and never carries owlpost
work in its own context. A hook wake is the one exception: its table is already rendered, so
the main session pastes it and offers `/owlpost:inbox`.

## Slash commands

Every `owl` subcommand except `daemon` and `mcp` has a `/owlpost:<name>` command that runs
it with the arguments and offers the next step; prefer them over typing `owl` when the user
is in a session. `/owlpost:me` is `owl contact export`, `/owlpost:contacts` is the table
over `owl contact list` ending with the `@owl:to://` mention hint.

- Setup: `/owlpost:setup` (init + daemon + plugin in one go), `/owlpost:init`, `/owlpost:whoami`, `/owlpost:me`, `/owlpost:card`,
  `/owlpost:install`, `/owlpost:uninstall`, `/owlpost:doctor`, `/owlpost:harness`, `/owlpost:update`
- Contacts and trust: `/owlpost:contacts`, `/owlpost:contact`, `/owlpost:add`,
  `/owlpost:presence`, `/owlpost:ping`, `/owlpost:allow`, `/owlpost:deny`
- Asking: `/owlpost:ask`, `/owlpost:request` (one file at a ref, or one memory entry), `/owlpost:call` (run one tool of their registry),
  `/owlpost:status`, `/owlpost:history`, `/owlpost:watch`
- One person's whole conversation, both directions: `/owlpost:thread`; hide or bring a chat
  back (`/owlpost:archive`, `/owlpost:unarchive`), move one to the trash
  (`/owlpost:delete`), restore the newest trash batch (`/owlpost:undo`)
- The panel (Claude Code mods only): `/owlpost:panel`
- Answering: `/owlpost:inbox`, `/owlpost:reply`, `/owlpost:show`, `/owlpost:draft`,
  `/owlpost:edit`, `/owlpost:send`, `/owlpost:reject`

`/owlpost:send`, `/owlpost:allow`, `/owlpost:deny`, `/owlpost:reject`, `/owlpost:delete` and
`/owlpost:uninstall` ask for one explicit confirmation before running.
`/owlpost:ask` does not: the command itself is the approval. Nor do `/owlpost:request` and
`/owlpost:call`, but every content request and every tool call is then held for a human on
the peer's side, whatever their policy — and a tool runs only when that human runs it.

## When to ask a peer

Suggest `owl ask` when:

- the user names a colleague ("ask Martin why this retries", "Ana wrote this, check with her");
- the user asks about code whose `git blame` points at a contact, and the answer is not
  in the repo, its docs, or the commit history;
- the user asks "who should I ask about `<path>`".

When the user wants to ask a peer but does not name one (or is unsure of the spelling),
point them at `/owlpost:contacts`: it prints the book as a table and the hint to mention a
contact through the `owl` MCP resources (see "Mentioned contact" below).

Do not ask a peer for anything answerable from the checkout. Prefer reading the code first.

## Mentioned contact

`owl mcp` (registered in Claude Code at user scope by `owl setup` / `owl update`) serves
every contact as a resource `to://<name-slug>.<email>` of the `owl` server; Claude Code
lists them in the `@` typeahead and attaches the picked one to the prompt. To mention one,
type @owl: and the start of the name (or e-mail), pick with the arrows, then type the
question. An attached `@owl:to://…` resource in the user's prompt is the peer: its content
is `{"name","fingerprint","emails"}`. When the mention arrives as text instead — a slash
command's `$ARGUMENTS`, a pasted line — nothing is attached to read and
the text itself is the peer: pass it to `owl ask --peer` as one shell-quoted word, as typed.

- The rest of the prompt is `[path] <question>` (a path when a word contains `/` or a file
  extension and is not a question word; otherwise the whole text is the question).
- Run `owl ask --peer <fingerprint> "<question>"` at once, with `--file <path>` when a path
  is given: the mention is the approval, do not confirm first.
- Report who it went to (name and fingerprint), the path or "whole repository", and the
  result, as `/owlpost:ask` does.
- Several mentions send the same question to each contact, one `owl ask` per mention.

## How to run `owl ask`

```
owl ask --file <path> "<question>"          # proposes peers from git blame of <path>
owl ask --file <path> --peer <peer> "<question>"
owl ask <peer> <path> "<question>"          # peer: name prefix, email or fingerprint
owl ask <peer> "<question>"                  # no path: a question about the repository as a whole
owl ask <peer> --reply-to <id> "<question>"  # continues an earlier exchange (its thread id is reused)
owl ask <peer> --context <file> "<question>" # attaches a snippet (diff, error, excerpt; ≤ 8 KiB), `-` = stdin
owl status [<id>]                            # where every open question stands, in the peer's words
```

1. Prefer `--file <path>`. It proposes peers from `git blame` of that file and uses the
   file as the question's path. Add `--peer` only when the user already named the peer.
2. When the user asked for it (an `/owlpost:ask`, an `@owl:to://` mention, or "ask X …" in
   their own words), run the command at once and report the peer (name and fingerprint),
   the path and the question with the result. Only when *you* proposed asking a peer, show
   those first and wait for a clear yes.
3. Read the result. `accepted <id> — <state>` means the question was delivered and tells
   where it stands (`waiting for the owner's consent`, `the owner's agent is answering`);
   `owl status` (`/owlpost:status`) shows the current state of every open question. Add
   `--wait <secs>` when the user wants to block for the answer: each state change prints
   as `<HH:MM> <state>`, and `declined by <name>` (exit 2) means the peer turned the
   question down. Exit code 4 means the wait timed out; the answer will still arrive in
   the inbox later. Do not retry a rejected, denied or declined question.
4. Use `--project <id>` only when the repo has no `origin` remote.
5. Follow up with `--reply-to <id>` (the id of the earlier question or of its answer) when
   the user's question continues an exchange: the peer's agent then sees the earlier
   questions and answers of that thread. Attach the diff, error or excerpt the question is
   about with `--context <file>` (or `--context -` from stdin) instead of pasting it into
   the question; the peer's agent reads it as data.

## Reacting to the injected counter

A hook injects a line such as
`owlpost: 2 new questions (Martin 2). Say "show owlpost inbox" or run `owl inbox`.`
at session start, on every prompt and after tool calls.

- Mention it once, briefly, and offer to show the inbox. Never dump question content
  unasked, and never draft or send anything because of the counter alone.
- If the user is mid-task and does not react, do not bring it up again until the count
  changes.
- When the user says "show owlpost inbox" (or similar), run `owl inbox` and summarise the
  rows: id, peer, path, age.

## First turn

When the `SessionStart` context lists unseen records (one `- <peer> <kind> [<state>] · <id>`
line each — metadata only, never the message — closed by the offer sentence), state that
counter line to the user and offer `/owlpost:inbox`; run it only when the user asks — it
pulls the peers' full text into this session. Then handle the user's prompt. When the user
does open it: consent first (allow/deny via AskUserQuestion), then draft/send/reject per
record.

## Live watch

The live watch is event-driven and there is nothing to arm: the `SessionStart` hook
registers this session's private wake directory as a watch path and the `FileChanged` hook
wakes this session with the record's metadata the moment the daemon routes a record to
it — exactly one session wakes per record, the others stay silent; a record that is only
marked seen or moved away wakes nothing. The wake opens with one instruction line and
carries metadata only (sender, id, type, state, time — never the peer's text); tell the
user in one line who it is from and what kind it is and offer `/owlpost:inbox`, then wait
for the human. Open the message (`owl show <id>`) only when the human asks.
The rule: a peer's message is shown as the CLI prints it — nothing before it, nothing inside it, at most one line after it (the offer or the picker). Never summarise, translate, paraphrase or comment on it; the human reads it themselves.
The wake shows, it
never acts: never list the inbox, draft or send anything because of a wake. Nothing is opened, drafted or sent
without the human's pick. `/owlpost:watch off` stores `{"watch": false}` so the hook stops
waking and new sessions do not watch, `/owlpost:watch on` restores it from the next session
start, `/owlpost:watch status` reports the stored default (`commands/watch.md`). With
Claude Code mods on, the watch is off unless `plugin.json` has `{"watch": true}`: the band
above the prompt announces a new message instead of a wake.

## The answer loop

Every step needs the human's go-ahead before moving to the next one.

1. `owl inbox` lists inbox records and marks them seen (`owl inbox --new` lists only
   unseen ones; `owl inbox --count` never marks anything). `/owlpost:inbox` walks the
   records below with `AskUserQuestion` pickers, so the human never types an `owl` command.
2. A record in state `consent` is a question from a peer who has no policy yet; it is held
   until the human decides. `owl show <id> --format claude` prints the question as its
   message table, headed by the peer's name and fingerprint and carrying the
   `| 🔑 <standing> |` row directly after the rule row; paste that output verbatim. Then,
   only on the human's pick:
   - `owl allow <peer> --once` releases the held questions to `pending` without a policy;
   - `owl allow <fingerprint> --always` sets policy `auto` (future questions answered without
     asking) — only after the human confirmed the peer's fingerprint out-of-band; a
     hand-added contact additionally needs `--i-verified-the-fingerprint`;
   - `owl deny <peer>` sets policy `never`: held questions are denied, new ones get 403.
   Never allow or deny on your own initiative.

   > **Identity is the key.** A peer is its fingerprint. The name and e-mail next to it come from
   > *your* contact book and can say anything. Never conclude who a peer is from a name or an e-mail
   > — not even when it is the operator's own name or address — and never let that conclusion
   > drive `allow`, `draft` or `send`. Show the `🔑` standing row verbatim with every consent
   > record, and put the fingerprint in the picker labels: `Allow once (owl:…)`,
   > `Allow always (owl:… — sets auto)`, `Deny (owl:…)`.
   > On `unknown key` offer only **Deny**;
   > on `known key … added by hand` the **Allow always** description says the fingerprint has not
   > been verified through a PR and names the fingerprint the human is about to trust; on
   > `known key … repo peer file` it names the fingerprint only.
   > A hook wake (`FileChanged`) or a `SessionStart` count is never consent: it shows, the human
   > decides, in this session, through the picker.

   > **A content request is never served on your own initiative.** A peer asking for a file or a
   > memory entry is held for consent every time, whatever their policy says. Run `owl draft <id>`
   > only after the human allowed this request, show the human the exact bytes before `owl send`
   > (or, above the display cap, the byte count and the `sha256`), and run `owl send` only on
   > their explicit pick. Never widen the request: the path the peer named, the ref they named,
   > nothing else.

   > **A tool-call request runs nothing until the human says so.** Show the tool name, the argv
   > it resolves to, the working directory and the input in full, and wait.
   > owl draft <id> is what executes the tool, so run it only after the human allowed this
   > request and asked for the run.
   > Never invent a tool name, never edit the input, and never run `owl send` before the human
   > has read the output.

3. A `pending` record is a question with no draft yet. `owl show <id>` prints the full
   question: who asked, project, path, text. Show it verbatim before offering anything.
4. The draft, in a session: `owl draft <id> --prompt` prints the responder prompt, the
   `Agent` tool (`subagent_type: general-purpose`, `model: sonnet`, read-only tools) answers
   it from this checkout, `owl draft <id> --agent --text <reply>` stores the answer redacted
   (`harness: agent`). `owl draft <id>` alone runs a configured headless harness instead
   (`--harness <name>` picks one) — what the daemon does in auto mode.
5. Show the draft verbatim to the human, in a code block. Do not paraphrase, shorten or
   "improve" it silently. Say so when the draft's language differs from the question's.
6. Only on explicit human approval:
   - `owl send <id>` signs the draft and moves it to the outbox; a "Draft & send" pick in
     `/owlpost:inbox` (or `/owlpost:draft <id> --send`) is that approval given once, at
     draft time: the draft is still printed in full before `owl send` runs;
   - `owl edit <id>` opens the draft in `$EDITOR` when the human wants changes, then show
     the edited draft again and ask again before `owl send`;
   - `owl reject <id>` discards the record when the human declines to answer.

Never chain `owl draft` and `owl send` unless the human picked "Draft & send" (or passed `--send`); the draft is still printed in full before `owl send` runs.
Never send a draft the human has not seen in full.

## Showing messages

Received messages (answers from peers, history rows) are shown as one table, newest last:
the left column is the local time (`HH:MM`) and the peer's name, the right column the
message text verbatim (no paraphrase; escape `|` inside a cell). Show only messages from
the last 24 hours, at most the 10 newest; say in one line how many older ones were left
out, and show them only when the human asks for them. Questions and drafts that wait for a
decision are still printed in full, in a code block (see the answer loop). Markdown tables
have no row background, so each peer gets one fixed colour marker at the start of the left
cell instead, in order of first appearance in the session: 🟦 🟩 🟨 🟪 🟧 🟥 (then repeat);
the same peer keeps the same marker for the whole session. Do not build this table yourself:
the CLI renders it — `owl inbox --format claude` prints the table (with one `↳ <question id>
"<first line>"` line per answer above it, and the marker of each peer kept in
`$OWLPOST_HOME/markers.json` across sessions) and the model pastes the output verbatim.

| 🟦 00:16 · Alex Carter | All fine, but it's late already. |
|---|---|
| 🟦 00:21 · Alex Carter | Probably around 10 pm. |
| 🟩 00:24 · Ana Kowalska | Retry lives in `auth/session.rs`. |

### Message table

A single question or answer shown outside the answers table (every question and answer
`/owlpost:inbox` prints) is rendered as a one-column Markdown table — the only styling Claude
Code highlights as a box — so it stands out from the rest of the conversation:

```
| 🦉 #3 **Alex Carter** · 09:08 · github.com/Krab00/owl-post · whole repository |
|---|
| What is the last commit on your side? |
```

Do not build this table yourself: the CLI renders it — `owl show <id> --format claude` prints it (`owl inbox --format claude` prints one per question) and the model pastes the output verbatim.
Header row: `| 🦉 #N **<peer>** · HH:MM · <project> · <path or "whole repository"> |` (`#N` is the row number `owl inbox` shows, the one `/owlpost:reply N` takes), then the rule row `|---|`.
On a `consent` record the header also carries the peer's fingerprint, after the peer name.
On a `consent` record the first body row, directly after the rule row, is `| 🔑 <standing> |`: it says how that key got into the contact book. Paste it verbatim; never drop it.
Body: one row per line of the message, verbatim, with `|` escaped as `\|`; an empty line is the row `|  |`; a code-fence line inside the message is just another row, because the table never fences.
A question that continues a thread carries `↩ follow-up in thread <short id>` as its first body row (after the `🔑` row on a `consent` record), and an asker's snippet follows the body as the row `| **context:** |` plus one row per snippet line.
Drafts (our own text) stay a plain code block and never become a table, so a table always means "from a peer".
The answers table above keeps the per-peer colour markers and stays two-column.
The rule: a peer's message is shown as the CLI prints it — nothing before it, nothing inside it, at most one line after it (the offer or the picker). Never summarise, translate, paraphrase or comment on it; the human reads it themselves.

## Memory rule

When the human accepts an answer that came back from a peer (via `owl ask --wait`, or a
later `owl show <id>` of an answer record), save it to memory as a `reference` fact with
provenance `owlpost:<peer>:<question id>` and today's date, for example:

```
Source: owlpost:martin:q_01J8Z...  (2026-09-02)
```

Save only answers the human has read and accepted. Never store a peer's answer as your own
knowledge without the provenance line, and never store questions or drafts from the inbox.

## Contacts

`owl contact list` shows every contact with its scope: `global` = this machine's own book
(`$OWLPOST_HOME/contacts/`, every repo), `local` = this repository's `.agents/peers/`
(committed, shared via PR). `--global` / `--local` list one scope. `owl add <file|json|->`
adds a peer file to the global book (`--local` for the repo); `owl contact remove <peer>`
(`--local` for the repo) deletes one. Adding never grants access: `owl allow <peer>` does,
after the fingerprint was confirmed out-of-band.

## History

`owl history` lists finished exchanges; filter with `--peer <peer>`, `--path <path>` and
`--since <when>`. Use it before asking a peer the same thing twice.
