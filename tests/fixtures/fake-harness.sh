#!/usr/bin/env sh
# Deterministic "LLM" harness for tests (design §10, §12). Never calls a real model.
#
#   FAKE_HARNESS_LOG   append pid, argv, cwd, env marker and stdin here (prompt assertions)
#   FAKE_SLEEP         sleep this many seconds before answering (timeout tests)
#   FAKE_PRINT_THEN_SLEEP  print the answer first, then sleep this many seconds (partial-output timeout)
#   FAKE_STDERR        print this text to stderr (error-message tests)
#   FAKE_OUTPUT_FILE   print this file instead of the canned answer (extraction tests)
#   FAKE_EXIT          exit with this code after printing (failure tests)
#
# The canned answer is plain text (not JSON) and contains exactly one fake secret line,
# `api_key = sk-test-123456`, so redaction can be asserted.
if [ -n "$FAKE_HARNESS_LOG" ]; then
  {
    echo "pid: $$"
    echo "argv: $*"
    echo "pwd: $PWD"
    echo "OWLPOST_RESPONDER: ${OWLPOST_RESPONDER:-unset}"
    echo "stdin:"
    if [ ! -t 0 ]; then cat; fi
    echo "end"
  } >>"$FAKE_HARNESS_LOG"
fi
if [ -n "$FAKE_STDERR" ]; then
  echo "$FAKE_STDERR" >&2
fi
if [ -n "$FAKE_SLEEP" ]; then
  sleep "$FAKE_SLEEP"
fi
if [ -n "$FAKE_OUTPUT_FILE" ]; then
  cat "$FAKE_OUTPUT_FILE"
else
  cat <<'ANSWER'
The retry policy is defined in src/client.rs (see commit 0123abc).
It reads the credentials file, whose sample line is:
api_key = sk-test-123456
Nothing else in the repository overrides it.
ANSWER
fi
if [ -n "$FAKE_PRINT_THEN_SLEEP" ]; then
  sleep "$FAKE_PRINT_THEN_SLEEP"
fi
exit "${FAKE_EXIT:-0}"
