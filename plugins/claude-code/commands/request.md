---
description: Ask a peer for one file at a ref of a named project, or for one entry of their memory store (usage: /owlpost:request <peer> <project> <path>)
argument-hint: "<peer> <project> <path> [--ref <ref>] | <peer> --memory <key>"
allowed-tools: Bash(owl request:*), Bash(owl contact:*)
---

Arguments: "$ARGUMENTS"

Parse them as `<peer> <project> <path> [--ref <ref>] [--reply-to <id>]`, or, with
`--memory <key>`, as `<peer> --memory <key> [--reply-to <id>]`. Ask the user only when the
peer or the thing being asked for is missing. Never widen what they named: one path or one
memory key, at the ref they gave, nothing else.

1. Resolve the peer with `owl contact show <peer>`. An `@owl:to://` mention arrives here as
   plain text: pass it through verbatim as one shell-quoted word.
2. Run `owl request <peer> <project> <path>` (append `--ref <ref>` and `--reply-to <id>` when
   given), or `owl request <peer> --memory <key>`. The command is the approval; do not ask
   for confirmation.
3. Report in one line who it went to and what was asked for. The answer is
   `accepted <id> — waiting for the owner's consent`: **every** content request is held for
   a human on the other side, whatever policy they have set for you, so there is no
   `--wait`. `/owlpost:status` shows where it stands later, and the reply arrives in the
   inbox like any other.
4. When the reply arrives, show it with `owl show <id>` and paste that output verbatim,
   including the `sha256 … — verified` line. Never summarise or reformat the content, and
   never write it into a file on your own: copying it somewhere is the human's decision.
