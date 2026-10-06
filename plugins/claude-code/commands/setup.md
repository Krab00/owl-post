---
description: First install in one go — owl init (if needed), the daemon service and this plugin (--dry-run lists the steps)
argument-hint: "[--name <name>] [--email <email>] [--plugin-source <dir|repo>] [--dry-run]"
allowed-tools: Agent, Bash(owl setup:*), Bash(owl doctor:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

If `--name` or `--email` is missing from the arguments, ask the user for it first, one
question at a time (name, then email), and pass both as flags (`owl setup` prompts for them
itself only on a terminal, never from here).

Run `owl setup $ARGUMENTS` and show its output. If the shell reports that `owl` is not found
(the plugin was installed before the binary), install the binary and run `owl setup` again;
it lands in `~/.local/bin`, so make sure that directory is on PATH:

```
curl -fsSL https://raw.githubusercontent.com/Krab00/owl-post/main/scripts/install.sh | sh
```

`owl setup` runs `owl init` unless a key exists, `owl install` for the daemon, then
`claude plugin marketplace add` and `claude plugin install` for this plugin (both no-ops when
the plugin is already installed), and registers the `owl` MCP server at user scope
(`claude mcp add`) unless it is already there. `--dry-run` lists the steps without running
them. On any other non-zero exit, show the error line and stop.

Afterwards run `owl doctor`, show the result, and tell the user to restart the Claude Code
session so the plugin loads.
