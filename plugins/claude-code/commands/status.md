---
description: Show where every open owlpost question stands (waiting for consent, being answered, under review), or one question by id
argument-hint: "[<id>]"
allowed-tools: Agent, Bash(owl status:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

Run `owl status $ARGUMENTS` and show the table verbatim: `ID  PEER  PATH  STATE  SINCE`,
one row per open question. The STATE column holds the peer's own words — one of
`waiting for the owner's consent`, `the owner's agent is answering` or
`the owner is reviewing the answer` — and `offline` when the peer cannot be reached now.
A question the peer declined is closed by this command (it moves to history as `declined`);
say so in one line.
Exit code 4 means there are no open questions: say that and stop.

Offer `/owlpost:ask <peer> --reply-to <id> <question>` when the user wants to follow up on
one of the rows, and `/owlpost:history` for the finished ones.
