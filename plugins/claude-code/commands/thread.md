---
description: Show the whole conversation with one person — the peer list, or one timeline
argument-hint: "[<peer>] [--since <when>] [--context <id>]"
allowed-tools: Agent, Bash(owl thread:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

1. No peer in the arguments: run `owl thread` and show its rows unchanged — one row per
   person, newest conversation first. Ask which person to open.
2. With a peer: run `owl thread <peer>` (adding `--since <when>` or `--context <id>` when the
   arguments carry them) and paste its output verbatim. It is already formatted: the tables
   are what the person sent you, the ```text blocks are your own words, and the one-line
   entries are what happened to each request — held, allowed, drafted, edited, sent.
3. `owl thread` never answers anything and never marks a record seen. To act on an open
   question from the timeline, use `/owlpost:inbox`.
