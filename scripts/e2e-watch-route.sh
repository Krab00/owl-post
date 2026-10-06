#!/bin/sh
# e2e-watch-route.sh — proof on the real Claude Code harness that exactly one session wakes
# per record (design §8, §12): two idle sessions in two directories, one of them the
# configured checkout of the test project; `owl route <id>` writes the record's message table
# into that session's private wake directory ($OWLPOST_HOME/sessions/<session_id>/wake, the
# SessionStart hook registered it as a watch path) and only that session's FileChanged hook
# (asyncRewake) exits 2 and gets a new model turn — the other session stays silent.
#
#   scripts/e2e-watch-route.sh                      # installed plugin, the `owl` on PATH
#   E2E_PLUGIN_DIR=plugins/claude-code PATH=$PWD/target/release:$PATH scripts/e2e-watch-route.sh
#
#   E2E_PLUGIN_DIR   plugin directory to load with `--plugin-dir` (default: none — the plugin
#                    installed at user scope is what runs)
#   E2E_OUT          directory that keeps both stream files as evidence (default: a temp dir,
#                    printed at the end)
#
# Two `claude -p --input-format stream-json --output-format stream-json --verbose
# --include-hook-events --permission-mode bypassPermissions` sessions run in one throwaway
# OWLPOST_HOME with their stdin held open (a FIFO each): session A in $WORK/a, which
# config.json maps as the checkout of the test project github.com/e2e/repo, session B in
# $WORK/b. The script sends each exactly one user message (`Reply with exactly the word:
# ready`), waits for both first `"type":"result"` events, drops one unseen question record
# about github.com/e2e/repo into $OWLPOST_HOME/spool/inbox/ and runs `owl route <id>`. Within
# 60 s stream A must carry a `system` `hook_response` event with `"hook_event":"FileChanged"`,
# `"exit_code":2` and the wake's `type: question` metadata line (the question text never), followed by a second
# `"type":"assistant"` event; stream B must carry no FileChanged hook_response and no second
# assistant event, checked 10 s after A woke. No second user message is ever sent. Prints
# `PASS` or `FAIL <reason>`, exits non-zero on FAIL (2 = precondition); each `claude` run is
# bounded by `timeout 180`. Cost: two `claude -p` calls.
set -u

fail_pre() {
    echo "e2e-watch-route: $1" >&2
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
CWD_A=$WORK/a
CWD_B=$WORK/b
STREAM_A=$OUT/stream-a.jsonl
STREAM_B=$OUT/stream-b.jsonl
STDERR_A=$OUT/claude-a.stderr
STDERR_B=$OUT/claude-b.stderr
FIFO_A=$WORK/stdin-a
FIFO_B=$WORK/stdin-b

echo "owl: $(command -v owl) ($(owl --version 2>/dev/null))"
echo "claude: $(command -v claude) ($(claude --version 2>/dev/null | head -n 1))"
echo "plugin: ${E2E_PLUGIN_DIR:-installed at user scope}"
echo "out: $OUT"

mkdir -p "$HOME_DIR" "$CWD_A" "$CWD_B"
OWLPOST_HOME=$HOME_DIR owl init --name e2e-route >"$OUT/init.log" 2>&1 || fail_pre "owl init failed, see $OUT/init.log"
mkdir -p "$HOME_DIR/spool/inbox" || fail_pre "cannot create the spool inbox"
# Session A's directory is the checkout of the test project (routing rule 1).
sed -i "s|\"projects\": {}|\"projects\": {\"github.com/e2e/repo\": \"$CWD_A\"}|" "$HOME_DIR/config.json"
grep -F "\"github.com/e2e/repo\": \"$CWD_A\"" "$HOME_DIR/config.json" >/dev/null || fail_pre "could not map the project to $CWD_A in config.json"
mkfifo "$FIFO_A" || fail_pre "cannot create the stdin FIFO for A"
mkfifo "$FIFO_B" || fail_pre "cannot create the stdin FIFO for B"

# One `claude -p` per session in the background, reading its stream-json input from its
# FIFO, its output kept as evidence. Stdin must stay open or claude exits after the first
# result.
start_session() {
    if [ -n "${E2E_PLUGIN_DIR:-}" ]; then
        (
            cd "$1" && OWLPOST_HOME=$HOME_DIR timeout 180 claude -p \
                --input-format stream-json --output-format stream-json --verbose \
                --include-hook-events --permission-mode bypassPermissions \
                --plugin-dir "$E2E_PLUGIN_DIR" <"$2" >"$3" 2>"$4"
        ) &
    else
        (
            cd "$1" && OWLPOST_HOME=$HOME_DIR timeout 180 claude -p \
                --input-format stream-json --output-format stream-json --verbose \
                --include-hook-events --permission-mode bypassPermissions \
                <"$2" >"$3" 2>"$4"
        ) &
    fi
}
# Plain calls, `$!` taken right after: inside a `$(...)` substitution the backgrounded
# subshell would inherit the substitution pipe and the main shell would wait for its EOF
# while the subshell waits for the FIFO's write end below — a deadlock.
start_session "$CWD_A" "$FIFO_A" "$STREAM_A" "$STDERR_A"
runner_a=$!
# Hold the write end open for the whole run (opening blocks until claude opens the read end).
exec 3>"$FIFO_A"
start_session "$CWD_B" "$FIFO_B" "$STREAM_B" "$STDERR_B"
runner_b=$!
exec 4>"$FIFO_B"

pid_alive() {
    kill -0 "$1" 2>/dev/null
}

finish() {
    exec 3>&-
    exec 4>&-
    kill "$runner_a" "$runner_b" 2>/dev/null
    wait "$runner_a" "$runner_b" 2>/dev/null
    rm -rf "$WORK"
    echo "streams kept under $OUT"
}

# Exactly one user message per session; nothing else is ever sent.
printf '%s\n' '{"type":"user","message":{"role":"user","content":"Reply with exactly the word: ready"}}' >&3
printf '%s\n' '{"type":"user","message":{"role":"user","content":"Reply with exactly the word: ready"}}' >&4

# Wait up to 120 s for both first result events.
first_result() {
    grep -n -F '"type":"result"' "$1" 2>/dev/null | head -n 1 | cut -d: -f1
}
waited=0
result_a=""
result_b=""
while [ $waited -lt 120 ]; do
    result_a=$(first_result "$STREAM_A")
    result_b=$(first_result "$STREAM_B")
    [ -n "$result_a" ] && [ -n "$result_b" ] && break
    if ! pid_alive "$runner_a" || ! pid_alive "$runner_b"; then
        break
    fi
    sleep 1
    waited=$((waited + 1))
done
if [ -z "$result_a" ] || [ -z "$result_b" ]; then
    finish
    echo "FAIL no result event from both sessions within ${waited}s (A: ${result_a:-none}, B: ${result_b:-none}; see $STDERR_A, $STDERR_B)"
    exit 1
fi

# Only now: one unseen question record about the test project, built like tests/inbox.rs
# builds them (`raw` is the payload JSON, `sig` is never checked), renamed into
# spool/inbox/, then routed — the affine session A must get the wake file.
id="e2e-route-$(date +%s)"
ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
raw=$(printf '{"v":1,"id":"%s","type":"question","from":"owl:e2e-peer","to":"owl:e2e-me","ts":"%s","in_reply_to":null,"body":{"project":"github.com/e2e/repo","path":"src/lib.rs","question":"does the watch wake?"}}' "$id" "$ts")
escaped=$(printf '%s' "$raw" | sed 's/"/\\"/g')
printf '{"raw":"%s","sig":"e2e","state":"pending","seen":false,"received_at":"%s","draft":null,"meta":{}}\n' "$escaped" "$ts" >"$WORK/$id.json"
mv "$WORK/$id.json" "$HOME_DIR/spool/inbox/$id.json"
routed=$(OWLPOST_HOME=$HOME_DIR owl route "$id" 2>"$OUT/route.stderr")
echo "route: $routed"
dropped_at=$(date +%s)
case $routed in
"routed $id -> "*) ;;
*)
    finish
    echo "FAIL owl route did not route the record: $routed (see $OUT/route.stderr)"
    exit 1
    ;;
esac

# The FileChanged hook_response (exit 2, the message table) after `result_line` in `stream`,
# as the line number relative to the tail; empty when absent.
wake_line() {
    tail -n "+$(($2 + 1))" "$1" | grep -n -F '"hook_response"' | grep -F '"hook_event":"FileChanged"' | grep -F '"exit_code":2' | grep -F 'type: question' | head -n 1 | cut -d: -f1
}
# Assistant events after line `$2` (relative to the tail after `$3`) in `stream`.
assistant_after() {
    tail -n "+$(($3 + 1))" "$1" | tail -n "+$(($2 + 1))" | grep -c -F '"type":"assistant"'
}

# Within 60 s: A's FileChanged hook_response and then an assistant event.
reason=""
hook_a=""
assistant_a=0
waited=0
while [ $waited -lt 60 ]; do
    hook_a=$(wake_line "$STREAM_A" "$result_a")
    if [ -n "$hook_a" ]; then
        assistant_a=$(assistant_after "$STREAM_A" "$hook_a" "$result_a")
        [ "$assistant_a" -gt 0 ] && break
    fi
    if ! pid_alive "$runner_a"; then
        break
    fi
    sleep 1
    waited=$((waited + 1))
done
woke_after=$(( $(date +%s) - dropped_at ))
if [ -z "$hook_a" ]; then
    reason="A: no FileChanged hook_response with exit_code 2 and the metadata wake within ${waited}s of the route"
elif [ "${assistant_a:-0}" -eq 0 ]; then
    reason="A: FileChanged hook_response seen but no assistant event followed within ${waited}s"
else
    hook_json=$(tail -n "+$((result_a + 1))" "$STREAM_A" | sed -n "${hook_a}p")
    case $hook_json in
    *'does the watch wake?'*) reason="A: hook_response carries the question text: $hook_json" ;;
    *) ;;
    esac
fi
# B must stay silent: 10 s after A woke, no FileChanged hook_response and no second turn.
if [ -z "$reason" ]; then
    sleep 10
    after_b=$(tail -n "+$((result_b + 1))" "$STREAM_B")
    hooks_b=$(printf '%s\n' "$after_b" | grep -F '"hook_response"' | grep -c -F '"hook_event":"FileChanged"')
    assistant_b=$(printf '%s\n' "$after_b" | grep -c -F '"type":"assistant"')
    if [ "$hooks_b" -gt 0 ]; then
        reason="B: $hooks_b FileChanged hook_response event(s) after its first result (must be 0)"
    elif [ "$assistant_b" -gt 0 ]; then
        reason="B: $assistant_b assistant event(s) after its first result (must be 0)"
    fi
fi
finish
if [ -z "$reason" ]; then
    echo "PASS (A woke ${woke_after}s after the route, B silent; one user message sent to each)"
    exit 0
fi
echo "FAIL $reason"
exit 1
