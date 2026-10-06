#!/bin/sh
# owlpost hook: print the Claude Code injection line for unseen questions, or nothing —
# and, on FileChanged, wake the session when the daemon routed a record to it.
#
# Registered for SessionStart, UserPromptSubmit, PostToolUse, FileChanged and SessionEnd
# (hooks.json). `owl inbox --count --format claude` prints exactly one JSON line when there
# are unseen questions and nothing at all when there are none, and never marks anything seen.
# Arguments are passed through (`--hook-event <name>`), and so is stdin: Claude Code's hook
# input JSON, whose `session_id` names this session. On SessionStart owl creates this
# session's private wake directory ($OWLPOST_HOME/sessions/<session_id>/wake) and the line
# also carries it as `watchPaths` unless $OWLPOST_HOME/plugin.json has `{"watch": false}`, so
# Claude Code runs the FileChanged hook when the daemon (or `owl route`) writes a wake file
# there — for exactly one session per record. On FileChanged owl exits 2 with that file's
# content on stderr — the exit code Claude Code's asyncRewake hook turns into a new model
# turn — and the script lets exactly that through; every other outcome is a silent exit 0.
# On SessionEnd owl removes the session's directory and prints nothing (the plain path
# below). The script is a strict no-op (empty stdout, exit 0) when `owl` is not on PATH or
# fails for any reason, e.g. an uninitialised home, so a broken install can never block a
# Claude session — except that on SessionStart a missing `owl` prints one hint line, so a
# plugin installed before the binary (plugin-first install) tells the user what to run.
if ! command -v owl >/dev/null 2>&1; then
    case " $* " in
    *" SessionStart "*) echo "owlpost: the owl binary is not installed; run /owlpost:setup --name \"Your Name\" --email you@company.com to install it" ;;
    esac
    exit 0
fi
case " $* " in
*" FileChanged "*)
    err=$(owl inbox --count --format claude "$@" 2>&1 >/dev/null)
    [ $? -eq 2 ] || exit 0
    printf '%s\n' "$err" >&2
    exit 2
    ;;
esac
owl inbox --count --format claude "$@" 2>/dev/null || exit 0
