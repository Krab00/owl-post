#!/usr/bin/env bash
# Test fixture: the tool a tool-call request names in the tests. Never a real build
# or test command.
#
# It echoes its stdin, appends one `start` line to the log naming the argv, the environment,
# the stdin and the working directory, and — after any requested sleep — one `done` line, so
# a timeout test can tell a run that started from one that finished. Every knob arrives as an
# **argument**, not as an environment variable: `owl draft` clears the environment and passes
# only `OWLPOST_TOOL`, `OWLPOST_TOOL_NAME`, `OWLPOST_PEER` and `PATH`, which is exactly the
# rule the `env=` field of the log lets a test pin.
#
#   fake-tool.sh FAKE_TOOL_LOG=<path> [FAKE_TOOL_EXIT=<n>] [FAKE_TOOL_SLEEP=<secs>]
#                [FAKE_TOOL_STDERR=<text>] [FAKE_TOOL_SECRET=1] [FAKE_TOOL_BIG=<bytes>]
#                [FAKE_TOOL_RAW=<text>] [FAKE_TOOL_NO_STDOUT=1]
#
# `FAKE_TOOL_RAW` writes <text> to stdout with **no** trailing newline and `FAKE_TOOL_NO_STDOUT`
# writes nothing at all — the two stdout shapes `echo` can never produce, and the ones the
# stderr join's newline branch turns on.
set -u
export LC_ALL=C

log=""
exit_code=0
sleep_for=""
stderr_text=""
secret=0
big=0
raw=""
no_stdout=0
for a in "$@"; do
  case "$a" in
    FAKE_TOOL_LOG=*)    log="${a#*=}" ;;
    FAKE_TOOL_EXIT=*)   exit_code="${a#*=}" ;;
    FAKE_TOOL_SLEEP=*)  sleep_for="${a#*=}" ;;
    FAKE_TOOL_STDERR=*) stderr_text="${a#*=}" ;;
    FAKE_TOOL_SECRET=*) secret="${a#*=}" ;;
    FAKE_TOOL_BIG=*)    big="${a#*=}" ;;
    FAKE_TOOL_RAW=*)    raw="${a#*=}" ;;
    FAKE_TOOL_NO_STDOUT=*) no_stdout="${a#*=}" ;;
  esac
done

input=$(cat)

if [ -n "$log" ]; then
  printf 'start\targv=%s\tenv=%s\tstdin=%s\tcwd=%s\n' \
    "$*" "$(env | sort | tr '\n' '|')" "$input" "$PWD" >> "$log"
fi

if [ "$no_stdout" != 0 ]; then
  : # stdout stays genuinely empty
elif [ -n "$raw" ]; then
  printf '%s' "$raw"
elif [ "$big" != 0 ]; then
  # A one-byte prefix in front of two-byte characters and nothing else on stdout, so the
  # cap's byte offset lands inside a character and a byte slice would panic or split it.
  s="ł"
  while [ ${#s} -lt "$big" ]; do s="$s$s"; done
  printf 'a%s\n' "$s"
else
  printf '%s\n' "$input"
fi
if [ "$secret" != 0 ]; then
  printf 'api_key: hunter2\n'
fi
if [ -n "$stderr_text" ]; then
  printf '%s\n' "$stderr_text" >&2
fi
if [ -n "$sleep_for" ]; then
  sleep "$sleep_for"
fi
if [ -n "$log" ]; then
  printf 'done\n' >> "$log"
fi
exit "$exit_code"
