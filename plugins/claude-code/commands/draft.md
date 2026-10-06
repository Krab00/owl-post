---
description: Draft an answer to a question — the Agent tool answers read-only from this checkout (a configured headless harness with --harness) and the draft is stored on the record
argument-hint: "<id> [--harness <name>] [--send]"
allowed-tools: Agent, Bash(owl draft:*), Bash(owl send:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

Draft the answer to record `<id>`:

1. Without `--harness`: run `owl draft <id> --prompt` — it prints the responder prompt for the
   record and changes nothing. Call the `Agent` tool with `subagent_type: general-purpose`,
   `model: sonnet` and that prompt followed by one line: "Use only Read, Grep and Glob; never
   edit or write files, never run commands; reply with the answer text only." Store the
   subagent's reply byte for byte, through a quoted heredoc so no shell character is
   interpreted:

   ```
   owl draft <id> --agent --text "$(cat <<'OWL_ANSWER'
   <the subagent's reply>
   OWL_ANSWER
   )"
   ```

   `--agent` stores it as `harness: agent`, redacted like a harness answer.
   With `--harness <name>`: run `owl draft <id> --harness <name>` instead (a configured
   headless harness against this checkout, what the daemon runs in auto mode) and show its
   output.
2. Show the stored draft verbatim, in a code block; do not paraphrase, shorten or "improve"
   it. On a non-zero `owl draft` exit show the error line and stop.

Offer `/owlpost:send <id>` to sign and send it, `/owlpost:edit <id>` to change it, or
`/owlpost:reject <id>` to discard the record.

`/owlpost:draft <id> --send` does draft, print, send in one go: `--send` is stripped from the
arguments before `owl draft` runs (owl has no such flag), the draft is printed verbatim as
above, then `owl send <id>` runs at once. On a non-zero `owl draft` exit show the error line
and stop (nothing is sent); on a non-zero `owl send` exit show the error line, the record
stays `drafted`.

Never chain `owl draft` and `owl send` unless the human picked "Draft & send" (or passed `--send`); the draft is still printed in full before `owl send` runs.
