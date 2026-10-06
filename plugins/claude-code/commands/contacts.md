---
description: List the contact book as a table and show how to ask a contact by mentioning it (@owl:to://…)
allowed-tools: Agent, Bash(owl contact:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

No arguments. This command only lists; a contact is asked by mentioning it in the prompt.

1. Run `owl contact list --json`. It prints a JSON array with one object per contact of the
   merged book (global `$OWLPOST_HOME/contacts/` plus this repository's `.agents/peers/`):
   `name`, `fingerprint`, `source` (`global` or `local`), `emails`, `endpoints` and, only
   when a policy is set, `policy` with `mode` (`manual`, `auto` or `never`).
   If the array is empty, print one line: "No contacts yet — run /owlpost:add with a colleague's peer file." and stop.
2. Print the book as ONE table, sorted by name, with the columns name, e-mails, fingerprint,
   source and policy; a contact without `policy` shows `-` in the policy column.
3. End with exactly this hint, on one line:
   Type @owl: and the start of the name, pick the contact, then type the question.
   The picked contact is inserted as a mention of the shape `@owl:to://<name-slug>.<email>`
   (the `owl` MCP server's resource URI, for example `@owl:to://ana-kowalska.ana@acme.com`).
