---
description: Live inbox watch — switch the event-driven wake on or off for new sessions, or report the stored default
argument-hint: "on|off|status"
allowed-tools: Agent, Read, Write
---

> **Subagent only.** Never run `owl` (or read a file) yourself: call the `Agent` tool
> (`subagent_type: general-purpose`, `model: sonnet`) with the steps below and the arguments;
> the subagent runs every command and returns its output verbatim, which you paste unchanged.
> The only thing that stays with you is the human's pick where a step asks for one; the
> picked option goes to a new `Agent` call that runs it. The main model never carries
> owlpost work.

Arguments: "$ARGUMENTS" — one of `on`, `off`, `status`; no argument means `status`.

The live watch is event-driven and needs nothing from you to run. The `SessionStart` hook
registers this session's wake directory (`$OWLPOST_HOME/sessions/<session_id>/wake`) as a
watch path and the `FileChanged` hook wakes the session with the record's metadata when
the daemon routes a record to it — exactly one session wakes per record (nothing happens
when a record is only marked seen or moved away). The stored default lives in `$OWLPOST_HOME/plugin.json`
as `{"watch": true|false}`; resolve the home as `${OWLPOST_HOME:-$HOME/.config/owlpost}`.
When the file is absent the default is on — in a session without Claude Code mods. With mods
on (`CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1`) the default is off: `hooks/owlpost.tsx` drops the
wake path unless the value is `true`, and the band above the prompt announces a new message. The `FileChanged` hook reads the same file on
every event, so `off` takes effect at once; `on` registers the watch path again at the next
session start. This command only reads and writes that file — it never runs `owl`.

## `status` (or no argument)

1. `Read` `plugin.json` in the home; the stored default is `on` (`true`), `off` (`false`),
   or, when the file is absent or has no `watch` key, `on without mods, off with mods`.
2. Say one line: `owlpost watch: default on|off; event-driven (FileChanged), nothing to arm`
   (with the value you read in place of `on|off`).

## `off`

1. `Read` `plugin.json` in the home (absent or not a JSON object: start from `{}`), set
   `watch` to `false` and keep every other key as it is (e.g. `band`), then `Write` the
   object back; with nothing else stored that is `{"watch": false}`.
2. Say: `owlpost watch: off; this session stops waking now, new sessions do not watch`.

## `on`

1. `Read` `plugin.json` in the home (absent or not a JSON object: start from `{}`), set
   `watch` to `true` and keep every other key as it is (e.g. `band`), then `Write` the
   object back; with nothing else stored that is `{"watch": true}`.
2. Say: `owlpost watch: on; the inbox is watched from the next session start`.

## Ground rules

- The watch only counts: the hook runs `owl inbox --count`, which never marks records seen,
  and nothing runs owl show, owl draft or owl send because of a wake. Nothing is opened,
  drafted or sent without the human's pick.
- When a wake lands, tell the user in one line who it is from and what kind it is and
  offer `/owlpost:inbox`; then wait for the human. The wake carries metadata only, never the
  peer's text: open the message only when the human asks. The rule: a peer's message is shown as the CLI prints it — nothing before it, nothing inside it, at most one line after it (the offer or the picker). Never summarise, translate, paraphrase or comment on it; the human reads it themselves.
- Blocking on a single record (`owl watch [--id <id>] [--timeout <secs>]`, exit 4 on
  timeout) belongs in a terminal, not in this command.
