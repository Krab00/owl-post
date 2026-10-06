---
description: Sign a draft answer and move it to the outbox
argument-hint: "<id>"
allowed-tools: Agent, Bash(owl send:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

Sending crosses the wire, so it needs one confirmation with the draft in view.

1. Show the draft that would go out verbatim, in a code block (from the last
   `/owlpost:show` or `/owlpost:draft` output; if it was not shown in this session, ask the
   user to run `/owlpost:show <id>` first). Never send a draft the user has not seen in full.
2. Ask for explicit confirmation with AskUserQuestion before running. Do not run anything
   until the user says yes.
3. Run `owl send $ARGUMENTS` and show the output. On a non-zero exit, show the error line
   (unknown id, no draft, or a failed move) and stop; the record is untouched and the
   command can be retried.
