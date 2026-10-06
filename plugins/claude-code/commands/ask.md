---
description: Ask a peer's agent a question about their code, optionally about one path (usage: /owlpost:ask <peer> [path] <question>)
argument-hint: "<peer> [path] <question...>"
allowed-tools: Agent, Bash(owl ask:*), Bash(owl contact:*), Bash(git blame:*)
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS"

Parse them as `<peer> [path] [--reply-to <id>] [--context <path>] <question...>`: the first
word is the peer; if the second word looks like a file path (contains `/` or a file
extension, and is not a question word) it is the path and the question is everything after
it, otherwise the question is everything after the peer and there is no path. A question
about the repository as a whole ("how long is your README?", "which branch do you deploy
from?") needs no path — do not ask for one. Ask the user only when the peer or the question
itself is missing. Two flags may sit anywhere after the peer and are passed through as they
are: `--reply-to <id>` continues an earlier exchange with that peer (the id of a question
you sent or an answer you received; the peer's agent sees the thread), `--context <path>`
attaches that file (a diff, an error, an excerpt; at most 8192 bytes) as the question's
context so the peer's agent answers the question actually being asked.

1. Resolve the peer with `owl contact show <peer>`. The peer may be an `@owl:to://` mention
   the user picked from the `@` typeahead, which arrives here as plain text: hand it to
   `owl contact show` and `owl ask` verbatim, as one shell-quoted word (the `@` and the `:`
   are part of the argument), never rewritten into a name, e-mail or fingerprint.
2. Do not ask for confirmation: the command is the approval. Run at once. With a path, run
   `owl ask --file <path> --peer <peer> "<question>"`; without one, run
   `owl ask <peer> "<question>"`; append `--reply-to <id>` and `--context <path>` when given.
3. Report in one line who the question went to (name and fingerprint), the path (or
   "whole repository") and the result. `accepted <id> — <state>` tells where the question
   stands on the peer's side (`waiting for the owner's consent`, `the owner's agent is
   answering`); `/owlpost:status` shows it again later. Offer to wait for the answer with
   `owl ask ... --wait <secs>` only if the user wants to block (the wait prints each state
   change, and `declined by <name>` when the peer turns the question down); otherwise tell
   them the answer will show up in the inbox counter.
4. When an answer arrives — through `--wait` or through a wake — show it with
   `owl show <id> --format claude` and paste that output verbatim, then offer the next step
   in one line. The rule: a peer's message is shown as the CLI prints it — nothing before it, nothing inside it, at most one line after it (the offer or the picker). Never summarise, translate, paraphrase or comment on it; the human reads it themselves.
5. When the user accepts the answer, follow the memory rule from the owlpost
   skill: save it with provenance `owlpost:<peer>:<question id>` and today's date.
