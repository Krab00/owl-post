---
description: Uninstall the owl daemon service
allowed-tools: Agent, Bash(owl uninstall:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Uninstalling stops the daemon, so questions and answers stop flowing until it is installed
again; it needs one confirmation.

1. Tell the user that.
2. Ask for explicit confirmation with AskUserQuestion before running. Do not run anything
   until the user says yes.
3. Run `owl uninstall` and show its output. On a non-zero exit, show the error line and
   stop.
4. Mention that `/owlpost:install` puts the service back.
