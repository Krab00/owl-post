---
description: Print this machine's card, or fetch and print a peer's card
argument-hint: "[peer]"
allowed-tools: Agent, Bash(owl card:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

Run `owl card $ARGUMENTS`: with no argument it prints this machine's own card, with a peer
(name prefix, email or fingerprint) it fetches and prints that peer's card. Show the output
in a code block. If the peer cannot be reached, show the error line and offer
`/owlpost:doctor`.
