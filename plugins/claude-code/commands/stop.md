---
description: Stop owlpost on this machine without uninstalling — close the panel, switch the live watch off, stop the daemon service
allowed-tools: Agent, Read, Write, Bash(owl stop:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

No arguments. With mods on (`CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1`), `hooks/owlpost.tsx`
has already closed the panel when it was open; without mods there is no panel to close.

1. Live watch off, as `/owlpost:watch off` does: `Read` `plugin.json` in the home
   (`${OWLPOST_HOME:-$HOME/.config/owlpost}`; absent or not a JSON object: start from `{}`),
   set `watch` to `false` and keep every other key as it is, then `Write` the object back.
2. Run `owl stop` (it keeps the service unit; `owl uninstall` removes it). On a non-zero
   exit, show the error line and stop.
3. Say one line: `owlpost stopped: watch off, <owl stop's line>`.

Mention that `/owlpost:start` brings it back.
