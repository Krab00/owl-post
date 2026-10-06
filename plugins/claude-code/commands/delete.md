---
description: Move a chat (or one thread of it with --context) to the trash; undoable with `owl undo` within 30 days
argument-hint: "<peer> [--context <id>]"
allowed-tools: Agent, Bash(owl delete:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

Deleting moves the records out of the spool into the trash, so it needs one confirmation.
It is undoable: `owl undo` restores the newest batch. The daemon removes a trash batch once
it is older than 30 days, so undo works within 30 days of the delete.

1. Tell the user which chat `owl delete` moves to the trash — with `--context <id>`, only
   the one thread — and that `/owlpost:undo` brings the newest deleted batch back within
   30 days.
2. Ask for explicit confirmation with AskUserQuestion before running. Do not run anything
   until the user says yes.
3. Run `owl delete $ARGUMENTS` and show the output. On a non-zero exit, show the error line
   and stop.
