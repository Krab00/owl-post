---
description: Ask a peer to run one tool of their registry with a JSON input (usage: /owlpost:call <peer> <tool> --input <file>)
argument-hint: "<peer> <tool> --input <file|-> [--project <id>] [--reply-to <id>]"
allowed-tools: Bash(owl call:*)
---

Arguments: "$ARGUMENTS"

Parse them as `owl call <peer> <tool> --input <file|-> [--project <id>] [--reply-to <id>]`.
Ask the user only when the peer, the tool name or the input is missing.
Never invent a tool name and never edit the input: the peer's registry decides what exists,
and the input is the user's words, not yours.

1. The tool must be one the peer has configured. An unknown name comes back as
   `400 unknown tool`, deliberately the same answer as "no registry at all" — that is not a
   hint to guess again with another name. Ask the user which tool their colleague offers.
2. The input is a JSON **object** in a file (or on stdin with `-`); anything else exits 1
   with `input must be a JSON object`. Show the user the object you are about to send.
3. Run `owl call <peer> <tool> --input <file>` (append `--project <id>` and `--reply-to <id>`
   when given). The command is the approval; do not ask for confirmation.
4. Report in one line who it went to and what was asked for. The answer is
   `accepted <id> — waiting for the owner's consent`: nothing runs on their machine until a
   human there allows this request and runs the tool by hand, so there is no `--wait`.
   `/owlpost:status` shows where it stands later.
5. When the reply arrives, show it with `owl show <id>` and paste that output verbatim,
   including the `exit <code> · <duration> · <n> bytes` line. Never summarise the output and
   never present a non-zero exit as a failure of the request: a failing build is an answer.
