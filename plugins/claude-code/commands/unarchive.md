---
description: Bring an archived chat (or one thread of it) back to `owl thread`
argument-hint: "<peer> [--context <id>]"
allowed-tools: Agent, Bash(owl unarchive:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

1. Run `owl unarchive $ARGUMENTS` and show the output. The chat — with `--context <id>`,
   only the one thread — reappears in `owl thread`.
2. On a non-zero exit, show the error line and stop.
