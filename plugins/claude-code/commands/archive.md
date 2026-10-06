---
description: Hide a chat from `owl thread` (one thread of it with --context)
argument-hint: "<peer> [--context <id>]"
allowed-tools: Agent, Bash(owl archive:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

1. Run `owl archive $ARGUMENTS` and show the output. It hides the peer's chat from
   `owl thread` — with `--context <id>`, only the one thread. Nothing moves and nothing is
   deleted.
2. On a non-zero exit, show the error line and stop. Otherwise mention that
   `owl thread --archived` lists what is hidden and `/owlpost:unarchive` brings it back.
