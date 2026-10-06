#!/bin/sh
# e2e-watch-wake.sh — proof on the real Claude Code harness that the owlpost plugin wakes an
# idle session when a record lands in the inbox (design §12; per-session
# routing): the SessionStart hook registers the session's private wake directory
# $OWLPOST_HOME/sessions/<session_id>/wake as a watch path, `owl route <id>` writes the
# record's message table there (what the daemon does on arrival) and the FileChanged hook
# (asyncRewake) exits 2 with that block, which Claude Code turns into a new model turn.
#
#   scripts/e2e-watch-wake.sh                      # installed plugin, the `owl` on PATH
#   E2E_PLUGIN_DIR=plugins/claude-code PATH=$PWD/target/release:$PATH scripts/e2e-watch-wake.sh
#
#   E2E_PLUGIN_DIR   plugin directory to load with `--plugin-dir` (default: none — the plugin
#                    installed at user scope is what runs)
#   E2E_OUT          directory that keeps the stream file as evidence (default: a temp dir,
#                    printed at the end)
#
# One `claude -p --input-format stream-json --output-format stream-json --verbose
# --include-hook-events --permission-mode bypassPermissions` session runs in a throwaway
# OWLPOST_HOME with its stdin held open (a FIFO). The script sends exactly one user message
# (`Reply with exactly the word: ready`), waits for the first `"type":"result"` event, and only
# then drops one unseen question record into $OWLPOST_HOME/spool/inbox/ and runs
# `owl route <id>`. Within 60 s the stream must carry a `system` `hook_response` event with
# `"hook_event":"FileChanged"`, `"exit_code":2` and the wake's `type: question` metadata line (the question text
# never), followed by a second `"type":"assistant"` event — no second user message is
# ever sent. Prints `PASS` or `FAIL <reason>`, exits
# non-zero on FAIL (2 = precondition); the whole `claude` run is bounded by `timeout 180`.
# Cost: one `claude -p` call.
set -u

fail_pre() {
    echo "e2e-watch-wake: $1" >&2
    exit 2
}

command -v claude >/dev/null 2>&1 || fail_pre "claude not found on PATH"
command -v owl >/dev/null 2>&1 || fail_pre "owl not found on PATH"
command -v mkfifo >/dev/null 2>&1 || fail_pre "mkfifo not found on PATH"

if [ -n "${E2E_PLUGIN_DIR:-}" ]; then
    [ -d "$E2E_PLUGIN_DIR" ] || fail_pre "E2E_PLUGIN_DIR=$E2E_PLUGIN_DIR is not a directory"
    E2E_PLUGIN_DIR=$(cd "$E2E_PLUGIN_DIR" && pwd)
fi
OUT=${E2E_OUT:-$(mktemp -d)}
mkdir -p "$OUT" || fail_pre "cannot create E2E_OUT=$OUT"
WORK=$(mktemp -d)
HOME_DIR=$WORK/home
STREAM=$OUT/stream.jsonl
STDERR=$OUT/claude.stderr
FIFO=$WORK/stdin

echo "owl: $(command -v owl) ($(owl --version 2>/dev/null))"
echo "claude: $(command -v claude) ($(claude --version 2>/dev/null | head -n 1))"
echo "plugin: ${E2E_PLUGIN_DIR:-installed at user scope}"
echo "out: $OUT"

mkdir -p "$HOME_DIR"
OWLPOST_HOME=$HOME_DIR owl init --name e2e-watch >"$OUT/init.log" 2>&1 || fail_pre "owl init failed, see $OUT/init.log"
mkdir -p "$HOME_DIR/spool/inbox" || fail_pre "cannot create the spool inbox"
mkfifo "$FIFO" || fail_pre "cannot create the stdin FIFO"

# The run: `claude -p` in the background, reading its stream-json input from the FIFO, its
# output kept as evidence. Stdin must stay open or claude exits after the first result.
if [ -n "${E2E_PLUGIN_DIR:-}" ]; then
    (
        cd "$WORK" && OWLPOST_HOME=$HOME_DIR timeout 180 claude -p \
            --input-format stream-json --output-format stream-json --verbose \
            --include-hook-events --permission-mode bypassPermissions \
            --plugin-dir "$E2E_PLUGIN_DIR" <"$FIFO" >"$STREAM" 2>"$STDERR"
    ) &
else
    (
        cd "$WORK" && OWLPOST_HOME=$HOME_DIR timeout 180 claude -p \
            --input-format stream-json --output-format stream-json --verbose \
            --include-hook-events --permission-mode bypassPermissions \
            <"$FIFO" >"$STREAM" 2>"$STDERR"
    ) &
fi
runner=$!
# Hold the write end open for the whole run (opening blocks until claude opens the read end).
exec 3>"$FIFO"

pid_alive() {
    kill -0 "$1" 2>/dev/null
}

finish() {
    exec 3>&-
    kill "$runner" 2>/dev/null
    wait "$runner" 2>/dev/null
    rm -rf "$WORK"
    echo "stream kept under $OUT"
}

# Exactly one user message; nothing else is ever sent.
printf '%s\n' '{"type":"user","message":{"role":"user","content":"Reply with exactly the word: ready"}}' >&3

# Wait up to 120 s for the first result event.
waited=0
result_line=""
while [ $waited -lt 120 ]; do
    result_line=$(grep -n -F '"type":"result"' "$STREAM" 2>/dev/null | head -n 1 | cut -d: -f1)
    [ -n "$result_line" ] && break
    if ! pid_alive "$runner"; then
        break
    fi
    sleep 1
    waited=$((waited + 1))
done
if [ -z "$result_line" ]; then
    finish
    echo "FAIL no result event within ${waited}s (see $STDERR)"
    exit 1
fi

# Only now: one unseen question record, built like tests/inbox.rs builds them (`raw` is the
# payload JSON, `sig` is never checked by the counter), renamed into spool/inbox/ so the
# watcher sees a single `add`.
id="e2e-wake-$(date +%s)"
ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
raw=$(printf '{"v":1,"id":"%s","type":"question","from":"owl:e2e-peer","to":"owl:e2e-me","ts":"%s","in_reply_to":null,"body":{"project":"github.com/e2e/repo","path":"src/lib.rs","question":"does the watch wake?"}}' "$id" "$ts")
escaped=$(printf '%s' "$raw" | sed 's/"/\\"/g')
printf '{"raw":"%s","sig":"e2e","state":"pending","seen":false,"received_at":"%s","draft":null,"meta":{}}\n' "$escaped" "$ts" >"$WORK/$id.json"
mv "$WORK/$id.json" "$HOME_DIR/spool/inbox/$id.json"
# Route it (what the daemon does when a record is born): the session is the only live one,
# so its wake directory gets the message table.
OWLPOST_HOME=$HOME_DIR owl route "$id" >"$OUT/route.log" 2>&1 || {
    finish
    echo "FAIL owl route $id failed, see $OUT/route.log"
    exit 1
}
dropped_at=$(date +%s)

# Within 60 s: the FileChanged hook_response (exit 2, the message table) and then an assistant event.
reason=""
hook_line=""
assistant_after=""
waited=0
while [ $waited -lt 60 ]; do
    after=$(tail -n "+$((result_line + 1))" "$STREAM")
    hook_line=$(printf '%s\n' "$after" | grep -n -F '"hook_response"' | grep -F '"hook_event":"FileChanged"' | grep -F '"exit_code":2' | grep -F 'type: question' | head -n 1 | cut -d: -f1)
    if [ -n "$hook_line" ]; then
        assistant_after=$(printf '%s\n' "$after" | tail -n "+$((hook_line + 1))" | grep -c -F '"type":"assistant"')
        [ "$assistant_after" -gt 0 ] && break
    fi
    if ! pid_alive "$runner"; then
        break
    fi
    sleep 1
    waited=$((waited + 1))
done
woke_after=$(( $(date +%s) - dropped_at ))
if [ -z "$hook_line" ]; then
    reason="no FileChanged hook_response with exit_code 2 and the metadata wake within ${waited}s of the drop"
elif [ "${assistant_after:-0}" -eq 0 ]; then
    reason="FileChanged hook_response seen but no assistant event followed within ${waited}s"
else
    hook_json=$(tail -n "+$((result_line + 1))" "$STREAM" | sed -n "${hook_line}p")
    case $hook_json in
    *'does the watch wake?'*) reason="hook_response carries the question text: $hook_json" ;;
    *) ;;
    esac
fi
finish
if [ -z "$reason" ]; then
    echo "PASS (woke ${woke_after}s after the record was added; one user message sent)"
    exit 0
fi
echo "FAIL $reason"
exit 1
