---
description: Harness table of config.json by subcommand — list, scan, add, edit, remove or use
argument-hint: "<list|scan|add|edit|remove|use> [name] [-- <cmd>...]"
allowed-tools: Agent, Bash(owl harness:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

1. Run `owl harness $ARGUMENTS` and show the output verbatim. The first word is the
   subcommand: `list` prints one row per configured harness (NAME, DRAFTS, FOUND, COMMAND),
   `scan` adds the known harnesses found on PATH that the config lacks,
   `add <name> [--answer-path <p>] -- <cmd>...` registers a harness (everything after `--`
   is the command and `{prompt}` inside it is where the question goes; `--answer-path`,
   default `raw`, says where the answer text sits in its output), `edit <name> -- <cmd>...`
   replaces a harness's command, `remove <name>` deletes it and `use <name>` picks the
   harness that drafts answers.
2. On a non-zero exit, show the error line (it names the unknown harness or the bad
   command) and stop.
