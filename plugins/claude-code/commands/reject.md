---
description: Discard an inbox record (the peer gets no answer)
argument-hint: "<id>"
allowed-tools: Agent, Bash(owl reject:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

Rejecting is final, so it needs one confirmation.

1. Tell the user which record `owl reject <id>` discards and that the peer gets no answer.
2. Ask for explicit confirmation with AskUserQuestion before running. Do not run anything
   until the user says yes.
3. Run `owl reject $ARGUMENTS` and show the output. On a non-zero exit, show the error line
   and stop.
