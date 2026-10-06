---
description: Check key, config, endpoints, harnesses and daemon; explain any failure and offer the fix
allowed-tools: Agent, Bash(owl doctor:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Run `owl doctor` and show its output verbatim. Every line is one check; a failed check names
what is missing.

For each failure offer the fix: no home or key → `/owlpost:init`; daemon not running →
`/owlpost:install`; a stale binary → `/owlpost:update`; no contacts → `/owlpost:add`; an
unreachable endpoint or a missing harness → the config change the line names. Do not run
any fix on your own.
