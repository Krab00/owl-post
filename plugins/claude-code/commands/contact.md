---
description: Contact book by subcommand — show <peer>, export (own peer file) or remove <peer> [--local] (use /owlpost:contacts to pick from a list)
argument-hint: "<show|export|remove> [peer] [--local]"
allowed-tools: Agent, Bash(owl contact:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

Run `owl contact $ARGUMENTS`. The first word is the subcommand: `show <peer>` prints one
contact as JSON, `export` prints this machine's peer file, `remove <peer> [--local]` deletes
a contact's file from the global book (or the repo's `.agents/peers/` with `--local`).
For a list to pick from with the arrow keys use `/owlpost:contacts` instead; it wraps
`owl contact list`. Show the output in a code block. On a non-zero exit, show the error line
(it names the unknown peer or the ambiguous prefix) and stop. After `show`, offer
`/owlpost:ask <peer> [path] <question>`.
