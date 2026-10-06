---
description: Show this machine's owlpost identity summary (name, email, fingerprint, home)
allowed-tools: Agent, Bash(owl whoami:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Run `owl whoami` and show its output verbatim. If it fails because the home is not
initialised, say so and offer `/owlpost:init`. To hand the identity to a colleague as a peer
file, point the user to `/owlpost:me` instead.
