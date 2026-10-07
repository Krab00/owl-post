#!/bin/sh
# Refuses content that must never enter this public repo: real e-mail addresses, real home
# paths and private-key blocks. Fixtures use the allowed domains and names below.
# Usage: scripts/check-private.sh          (staged diff; the pre-commit hook)
#        scripts/check-private.sh --all    (whole tree; CI)
set -u
pat='[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}|/(Users|home)/[A-Za-z0-9_-]+|BEGIN [A-Z ]*PRIVATE KEY'
# Fixture domains and names. Add here, never a real one.
allow='(git|user)@(github\.com|gitlab\.com|bitbucket\.org)|@(x\.io|x\.org|company\.com|acme\.com|e2e\.local|[a-z]+\.example|example\.(com|org|net)|users\.noreply\.github\.com|anthropic\.com)\b|/(Users|home)/(alex|bea|me|other|trash|you)\b|PRIVATE KEY-----\\?n?abc'
if [ "${1:-}" = "--all" ]; then
  hits=$(git grep -nIE "$pat" -- . ':!Cargo.lock' ':!scripts/check-private.sh' | grep -viE "$allow")
else
  hits=$(git diff --cached -U0 -- . ':!scripts/check-private.sh' | grep -E '^\+[^+]' | grep -nE "$pat" | grep -viE "$allow")
fi
[ -z "$hits" ] && exit 0
echo "private data must not enter the repo (fixtures: see the allow list in scripts/check-private.sh):" >&2
echo "$hits" >&2
exit 1
