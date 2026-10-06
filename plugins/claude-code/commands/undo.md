---
description: Restore the newest trash batch (what `owl delete` moved out)
allowed-tools: Agent, Bash(owl undo:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

1. Run `owl undo` and show the output: the newest trash batch goes back to the spool, byte
   for byte. A second `owl undo` restores the batch before it.
2. Exit 4 means there is nothing to undo: say that and stop. On any other non-zero exit
   show the error line and stop — `owl undo` refuses, and names the file, rather than
   overwrite a record that arrived again at its old place since the delete.
