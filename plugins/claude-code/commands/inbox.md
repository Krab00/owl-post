---
description: Walk the owlpost inbox — list it, show each question verbatim, handle consent (allow/deny), take a typed command or the human's own answer for a question, and pick send/edit/reject with AskUserQuestion
allowed-tools: Agent, Bash(owl inbox:*), Bash(owl show:*), Bash(owl allow:*), Bash(owl deny:*), Bash(owl draft:*), Bash(owl edit:*), Bash(owl send:*), Bash(owl reject:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Walk the inbox record by record. The human never types an `owl` command: a `pending` question
takes a typed reply (a command word or the human's own answer, step 3), every other choice is an
`AskUserQuestion` picker, and every command below runs on the reply or pick that names it.

Two rules hold for the whole flow:

- The question text and every draft are printed verbatim in a code block before any picker.
  Never paraphrase, shorten or "improve" them; never offer a choice about a text the human
  has not seen in full.
- `owl send` runs only on an explicit "Send" pick or a typed "draft & send". `owl allow --always`
  and `owl deny` run only on their pick. Never send, allow or deny on your own initiative.

The CLI renders every message; you paste. Never:

- never build the message table yourself
- never your own icons, markers or columns
- never paraphrase
- a table always means "from a peer"

The rule: a peer's message is shown as the CLI prints it — nothing before it, nothing inside it, at most one line after it (the offer or the picker). Never summarise, translate, paraphrase or comment on it; the human reads it themselves.

## 1. List

Run `owl inbox --json`. Each row carries `id`, `from` (fingerprint), `from_name`, `state`,
`project`, `path` (`null` = the whole repository), `type`, `age` and `summary` (the first line
of the message, cut to 60 chars). Listing marks the rows seen; that is expected. If the array
is empty, say "owlpost inbox is empty" and stop.

Otherwise run `owl inbox` and paste its output verbatim in a plain code block — always, even
for a single record — so the human sees the whole inbox before the first record. When you name
a record anywhere after that (a question, a picker header), name it by its `summary`, never by
its id.

Handle the records by state, in this order: `consent`, then `pending`, then `drafted`,
then `answer` records. After each record continue with the next one; after the last one
stop.

## 2. `consent` records (held until the human allows the peer)

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

For every record in state `consent`:

1. Run `owl show <id> --format claude` and paste its output verbatim: the message table
   of the question, headed by the peer name, the fingerprint, the project and the path
   (or "whole repository"), with the `| 🔑 <standing> |` row directly after the rule row.
2. `AskUserQuestion` with four options:
   - **Allow once (owl:…)** — `owl allow <fingerprint> --once`: releases this peer's held questions
     without setting a policy; the next question is held again.
   - **Allow always (owl:… — sets auto)** — `owl allow <fingerprint> --always`: sets policy `auto`, so future
     questions from this peer are answered without asking. The command takes the fingerprint,
     never a name. The human confirms the peer's
     fingerprint out-of-band first (a call, a chat message, `owl whoami` on the peer's
     machine); a hand-added contact additionally needs `--i-verified-the-fingerprint`, which
     you pass only when the human says they did verify it. Say all of this in the option
     description.
   - **Deny (owl:…)** — `owl deny <fingerprint>`: policy `never`; held questions are denied and new
     ones get 403.
   - **Skip** — leave the record held and move on.
3. Run the picked command and show its output. On a non-zero exit show the error line and
   stop. After "Allow once" or "Allow always" the record is now `pending`: continue with it
   in step 3 right away, without listing again.

## 3. `pending` records (question with no draft yet)

Take the `pending` records in list order. For each one:
run `owl show <id> --format claude` and paste its output verbatim (the question's table with
peer name, project and path). No picker here: end the turn with this one line, in the human's language, and wait for their reply:

`Type draft · draft & send · reject · skip — or write your own answer.`

The reply is a command word or the human's own answer:

- **draft** — the draft steps of `commands/draft.md`: `owl draft <id> --prompt`, the
  `Agent` tool (`subagent_type: general-purpose`, `model: sonnet`, read-only: Read, Grep,
  Glob only, answer text only) on that prompt, then `owl draft <id> --agent --text …` with
  the subagent's reply through a quoted heredoc. Then continue with the record in step 4.
- **draft & send** — do the **draft** steps, print the draft verbatim in a code block (as
  step 4 does), then run `owl send <id>` at once, without a second picker. The typed command is the
  human's explicit approval to send whatever the subagent produced (typing draft reads it first). On a non-zero `owl draft` exit
  show the error line and stop (nothing is sent); on a non-zero `owl send` exit show the
  error line, the record stays `drafted`.
- **reject** — run `owl reject <id>`; the peer gets no answer.
- **skip** — leave it pending and move on.
- **Anything else is the human's own answer** — store it byte for byte as the draft, passing
  it through a quoted heredoc so no shell character is interpreted:

  ```
  owl draft <id> --text "$(cat <<'OWL_ANSWER'
  <the human's reply>
  OWL_ANSWER
  )"
  ```

  Then continue with the
  record in step 4 (its picker offers Send, Save as draft, Edit and Reject). Never send it
  without that Send pick.

## 4. `drafted` records (draft stored, not sent)

1. Run `owl show <id> --format claude` (unless its output is already in view from step 3)
   and paste its output verbatim: the question block,
   then `draft:` with the draft in a plain code block and `harness: <name>`.
   When the CLI prints a `note:` line (the draft is in a different language than the
   question), recommend Edit before the picker.
2. `AskUserQuestion` with **Send / Save as draft / Edit / Reject**:
   - **Send** — run `owl send <id>`: signs the draft and moves it to the outbox. This is the
     only pick in this picker that runs `owl send`.
   - **Save as draft** — nothing runs; the draft stays stored (`drafted`) and the flow moves on.
   - **Edit** — `owl edit <id>` opens `$EDITOR`, which cannot run inside a session (see
     `commands/edit.md`): tell the human to run `owl edit <id>` in a terminal and say when
     they are done. Then run `owl show <id> --format claude` again, paste its output
     verbatim, and ask again with the same Send / Save as draft / Edit / Reject picker. Never send a draft
     the human has not seen after editing.
   - **Reject** — run `owl reject <id>`; the draft is discarded and the peer gets no answer.
3. Show the command's output. On a non-zero exit show the error line and stop; the record
   is untouched and the flow can be repeated.

For the history of one person — everything they asked, everything you asked them, and what
happened to each request — use `/owlpost:thread <peer>` instead of walking the inbox.

Never chain `owl draft` and `owl send` unless the human picked "Draft & send" (or passed `--send`); the draft is still printed in full before `owl send` runs.

## 5. `answer` records (a peer answered a question asked from here)

Run `owl inbox --format claude` and paste its output verbatim. The output is the
"Showing messages" table of the answers (last 24 hours, at most the 10 newest, newest last,
one `↳` line per answer naming its question, one line under it when older ones were left
out); the answers table keeps the per-peer colour markers and stays two-column. Then apply the memory
rule from the owlpost skill: only when the human accepts an answer, save it as a `reference`
fact with provenance `owlpost:<peer>:<question id>` and today's date.
