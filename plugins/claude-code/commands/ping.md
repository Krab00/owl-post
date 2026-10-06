---
description: Probe one peer right now (exit 2 when the peer is offline)
argument-hint: "<peer>"
allowed-tools: Agent, Bash(owl ping:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

1. Run `owl ping $ARGUMENTS` and show the output: `<peer> is online`, or `<peer> is
   offline: <reason>`.
2. Exit 2 means the peer is offline — that is the answer, not a failure of the command:
   report the reason it printed. On any other non-zero exit, show the error line and stop.
