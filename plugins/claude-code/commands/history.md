---
description: Show finished owlpost exchanges, optionally filtered by peer, path or age
argument-hint: "[--peer <peer>] [--path <path>] [--since <when>]"
allowed-tools: Agent, Bash(owl history:*), Bash(owl show:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

Run `owl history` with the given filters (`--peer`, `--path`, `--since`) and present the
rows as the message table from the owlpost skill ("Showing messages": time and peer on the
left, text on the right, last 24 hours, at most the 10 newest, say how many were left out).
Offer `owl show <id>` for the full content of any exchange the user picks.

Use this before asking a peer a question that may already have been answered.
