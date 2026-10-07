---
description: Start owlpost again after /owlpost:stop — switch the live watch on, start the daemon service
allowed-tools: Agent, Read, Write, Bash(owl start:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

No arguments.

1. Live watch on, as `/owlpost:watch on` does: `Read` `plugin.json` in the home
   (`${OWLPOST_HOME:-$HOME/.config/owlpost}`; absent or not a JSON object: start from `{}`),
   set `watch` to `true` and keep every other key as it is, then `Write` the object back.
2. Run `owl start`. On a non-zero exit, show the error line and stop (no service installed:
   offer `/owlpost:install`).
3. Say one line: `owlpost started: watch on, <owl start's line>`.
