#!/usr/bin/env sh
# Deterministic "$EDITOR" for tests. Appends one line to the file it is given
# and exits 0; never opens a terminal.
#
#   FAKE_EDITOR_LOG   append the argv here, so the test can prove the editor really ran
if [ -n "$FAKE_EDITOR_LOG" ]; then
  echo "argv: $*" >>"$FAKE_EDITOR_LOG"
fi
printf 'edited by the fake editor\n' >>"$1"
exit 0
