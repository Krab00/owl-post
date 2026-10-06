---
description: Show the full content of an inbox or history record (marks it seen)
argument-hint: "<id>"
allowed-tools: Agent, Bash(owl show:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

Run `owl show $ARGUMENTS` and show the output verbatim: who asked, project, path, the
question text, and the draft or answer when there is one. Showing marks the record seen;
that is expected. On a non-zero exit, show the error line (unknown id) and stop.

Add `--format claude` when you want the message table the CLI renders, and paste that output verbatim.
The rule: a peer's message is shown as the CLI prints it — nothing before it, nothing inside it, at most one line after it (the offer or the picker). Never summarise, translate, paraphrase or comment on it; the human reads it themselves.

Then offer the next step for the record: `/owlpost:draft <id>` when there is no draft yet,
`/owlpost:send <id>` or `/owlpost:reject <id>` when there is one. Never run any of them on
your own.
