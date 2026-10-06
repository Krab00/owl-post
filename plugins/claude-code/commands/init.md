---
description: Create the owlpost home, key and config; print the fingerprint
argument-hint: "[--name <name>] [--email <email>]"
allowed-tools: Agent, Bash(owl init:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

Run `owl init $ARGUMENTS` (`--name` and `--email` fill the config; ask for them first when
they were not given) and show its output, including the fingerprint. If the home already
exists, show the error line and stop; nothing is overwritten.

Then offer the next steps: `/owlpost:install` for the daemon, `/owlpost:me` to hand the
peer file to a colleague, `/owlpost:add` for a colleague's peer file.
