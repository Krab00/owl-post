---
description: Per-contact online/last seen plus the daemon's last pull summary
allowed-tools: Agent, Bash(owl presence:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

1. Run `owl presence` and show its output verbatim: one row per contact — `online`,
   `offline` or `-` when the daemon never probed that peer — plus the daemon's last pull
   summary: when it ran, how many asks are open and how many peers were probed.
2. Exit 4 means there is no daemon status file at all: say so and suggest `/owlpost:install`
   to start the daemon. On any other non-zero exit, show the error line and stop.
