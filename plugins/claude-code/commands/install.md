---
description: Install the owl daemon as a launchd/systemd service (--dry-run prints the unit)
argument-hint: "[--dry-run]"
allowed-tools: Agent, Bash(owl install:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

Run `owl install $ARGUMENTS` and show its output. `--dry-run` prints the unit that would be
written without installing anything. On a non-zero exit, show the error line and stop.

Afterwards offer `/owlpost:doctor` to check that the daemon is running.
