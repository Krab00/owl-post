---
description: Update owl (binary + daemon) and reinstall this plugin
argument-hint: "[--source <repo dir>]"
allowed-tools: Agent, Bash(owl update:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

Run `owl update` with the given arguments (`--source <dir>` builds from a local checkout
instead of downloading the latest release; the new build replaces the `owl` that is actually
running, printed as `installed <path>`; the `owl` MCP server is registered at user scope
when missing). Show its output, then tell the user to restart
the Claude Code session so the reinstalled plugin loads.
