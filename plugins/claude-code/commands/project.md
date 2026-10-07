---
description: Projects table of config.json by subcommand — add a checkout, list or remove
argument-hint: "add [path] [--name <key>] | list | remove <name>"
allowed-tools: Agent, Bash(owl project:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

1. Run `owl project $ARGUMENTS` and show the output verbatim. The first word is the
   subcommand: `add [path] [--name <key>]` maps a checkout (default: the current directory)
   under the key peers ask about (default: what `owl ask` sends from there — the origin
   remote as host/org/repo, else the directory name) and prints `added <name> → <path>` or
   `updated <name> → <path>`, `list` prints one row per project (NAME, PATH, `missing` when
   the checkout is gone) and `remove <name>` unmaps a project. The running daemon picks the
   change up without a restart.
2. On a non-zero exit, show the error line (it names the bad path or the unknown project)
   and stop.
