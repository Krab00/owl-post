---
description: Show this machine's owlpost identity (peer file) to send to a colleague
allowed-tools: Agent, Bash(owl contact export:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Run `owl contact export` and show the JSON in a code block. Tell the user to send it to the
colleague, who saves it as a file and runs `owl add <file>`, then confirms the fingerprint
out-of-band before `owl allow`.
