#!/bin/bash
# e2e-real.sh — the manual loop against a REAL harness on this machine (design §12).
#
#   OWL_HARNESS=claude|codex|opencode [OWL_TRANSPORT=https|iroh] scripts/e2e-real.sh
#
# Two temp homes (Ana asks, Bea answers), two `owl daemon --foreground` processes on port 0,
# a throwaway git repo as the project, the real harness named by OWL_HARNESS drafting Bea's
# answer. Every step is printed; daemon and command logs land under target/e2e-real/<stamp>/.
# Never run by CI or `cargo test` — it needs a real, authenticated harness and a network-free
# but live machine. Exit 2 = precondition failure (guards below), 1 = a step failed.
#
#   OWL_TRANSPORT  https (default): Bea's peer file carries her host:port, as before.
#                  iroh: both peer files have empty `endpoints`; the daemons keep the default
#                  `relay_urls` (n0's public relays), so the loop runs over iroh — needs
#                  internet. Both daemons' `owl doctor` iroh lines are printed for the record.
#   OWL_BIN        owl binary (default: target/debug/owl, built with `cargo build` if missing)
#   OWL_E2E_KEEP   set to keep the temp homes and repo after the run
set -euo pipefail

usage_fail() {
  echo "e2e-real: $1" >&2
  echo "usage: OWL_HARNESS=claude|codex|opencode [OWL_TRANSPORT=https|iroh] scripts/e2e-real.sh" >&2
  exit 2
}

# ---- guards (before anything is touched) -------------------------------------------------
case "${OWL_HARNESS:-}" in
  "") usage_fail "OWL_HARNESS is unset; set it to the harness to drive (claude|codex|opencode)" ;;
  claude | codex | opencode) ;;
  *) usage_fail "OWL_HARNESS=${OWL_HARNESS} is not one of claude|codex|opencode" ;;
esac
TRANSPORT=${OWL_TRANSPORT:-https}
case "$TRANSPORT" in
  https | iroh) ;;
  *) usage_fail "OWL_TRANSPORT=${TRANSPORT} is not one of https|iroh" ;;
esac
if ! command -v "$OWL_HARNESS" >/dev/null 2>&1; then
  usage_fail "harness binary '${OWL_HARNESS}' not found on PATH (OWL_HARNESS=${OWL_HARNESS})"
fi
for tool in git mktemp; do
  command -v "$tool" >/dev/null 2>&1 || usage_fail "'$tool' not found on PATH"
done

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OWL=${OWL_BIN:-$ROOT/target/debug/owl}
if [ ! -x "$OWL" ]; then
  echo "==> building owl ($OWL missing)"
  (cd "$ROOT" && cargo build -q)
fi
[ -x "$OWL" ] || usage_fail "owl binary not found at $OWL (set OWL_BIN)"

STAMP=$(date +%Y%m%d-%H%M%S)
LOGDIR=$ROOT/target/e2e-real/$STAMP
mkdir -p "$LOGDIR"
WORK=$(mktemp -d)
A=$WORK/a
B=$WORK/b
REPO=$WORK/repo
PROJECT=e2e-real
FILE=src/greeting.rs
QUESTION="What does greet() return and in which file is it defined?"
PIDS=()

cleanup() {
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
  if [ -z "${OWL_E2E_KEEP:-}" ]; then
    rm -rf "$WORK"
  else
    echo "==> kept $WORK"
  fi
}
trap cleanup EXIT

step() { echo; echo "==> $*"; }
run() {
  # run <who> <home> <args…>: print the command, run it from the repo, tee the log.
  local who=$1 home=$2
  shift 2
  echo "[$who] owl $*"
  (cd "$REPO" && OWLPOST_HOME= "$OWL" --home "$home" "$@") 2>&1 | tee -a "$LOGDIR/$who-commands.log"
  return "${PIPESTATUS[0]}"
}
wait_for() {
  # wait_for <secs> <what> <cmd…>: poll until the command succeeds.
  local secs=$1 what=$2
  shift 2
  local i=0
  until "$@"; do
    i=$((i + 1))
    [ "$i" -lt $((secs * 10)) ] || { echo "e2e-real: timed out waiting for $what" >&2; exit 1; }
    sleep 0.1
  done
}
write_config() {
  # write_config <home> <name> <email> <extra-json-fields>
  cat >"$1/config.json" <<EOF
{
  "name": "$2",
  "emails": ["$3"],
  "listen": "127.0.0.1:0",
  "endpoints": [${5:-}],
  "pull_interval_secs": 1,
  "notify": false,
  "responder": { "enabled": true, "harness": "$OWL_HARNESS" },
  "projects": { "$PROJECT": "$REPO" }$4
}
EOF
}

# ---- throwaway project -------------------------------------------------------------------
step "throwaway repo at $REPO"
mkdir -p "$REPO/src" "$REPO/.agents/peers"
cat >"$REPO/$FILE" <<'EOF'
/// Greets whoever asks.
pub fn greet() -> &'static str {
    "hello from owlpost"
}
EOF
echo "# e2e-real fixture" >"$REPO/README.md"
git -C "$REPO" init -q
git -C "$REPO" -c user.email=dev@example.org -c user.name=Dev add .
git -C "$REPO" -c user.email=dev@example.org -c user.name=Dev commit -q -m fixture

# ---- identities and peer files -----------------------------------------------------------
step "owl init for Ana ($A) and Bea ($B)"
mkdir -p "$A" "$B"
run ana "$A" init --name Ana --email ana@e2e.local
run bea "$B" init --name Bea --email bea@e2e.local
write_config "$A" Ana ana@e2e.local ""
write_config "$B" Bea bea@e2e.local ""
echo "[ana] owl contact export > .agents/peers/ana.json"
(cd "$REPO" && OWLPOST_HOME= "$OWL" --home "$A" contact export) >"$REPO/.agents/peers/ana.json"

step "start Bea's daemon (pinning Ana's key from .agents/peers)"
(cd "$REPO" && OWLPOST_HOME= RUST_LOG=info "$OWL" --home "$B" daemon --foreground) >"$LOGDIR/bea-daemon.log" 2>&1 &
PIDS+=($!)
wait_for 10 "Bea's daemon.addr" test -s "$B/daemon.addr"
B_ADDR=$(tr -d '[:space:]' <"$B/daemon.addr")
echo "Bea listens on $B_ADDR"
if [ "$TRANSPORT" = https ]; then
  write_config "$B" Bea bea@e2e.local "" "\"$B_ADDR\""
else
  echo "OWL_TRANSPORT=iroh: Bea's peer file gets no endpoints; Ana dials her key over iroh"
fi
echo "[bea] owl contact export > .agents/peers/bea.json"
(cd "$REPO" && OWLPOST_HOME= "$OWL" --home "$B" contact export) >"$REPO/.agents/peers/bea.json"
git -C "$REPO" -c user.email=dev@example.org -c user.name=Dev add .agents
git -C "$REPO" -c user.email=dev@example.org -c user.name=Dev commit -q -m "peers"

step "start Ana's daemon (pull every 1 s)"
(cd "$REPO" && OWLPOST_HOME= RUST_LOG=info "$OWL" --home "$A" daemon --foreground) >"$LOGDIR/ana-daemon.log" 2>&1 &
PIDS+=($!)
wait_for 10 "Ana's daemon.addr" test -s "$A/daemon.addr"

if [ "$TRANSPORT" = iroh ]; then
  step "iroh: wait for both daemons to reach a relay (doctor's iroh line)"
  iroh_ok() { (cd "$REPO" && OWLPOST_HOME= "$OWL" --home "$1" doctor 2>/dev/null | grep -q '^ok   iroh: '); }
  wait_for 30 "Bea's relay connection" iroh_ok "$B"
  wait_for 30 "Ana's relay connection" iroh_ok "$A"
  run bea "$B" doctor | grep ' iroh: ' || true
  run ana "$A" doctor | grep ' iroh: ' || true
fi

# ---- the manual loop ---------------------------------------------------------------------
step "Ana asks Bea"
ASK_OUT=$(run ana "$A" ask Bea "$FILE" "$QUESTION" --project "$PROJECT")
printf '%s\n' "$ASK_OUT"
QID=${ASK_OUT##*accepted }
QID=${QID%%[[:space:]]*}
[ -n "$QID" ] || { echo "e2e-real: no 'accepted <id>' in: $ASK_OUT" >&2; exit 1; }
echo "question id $QID"

step "Bea holds it for consent, then allows once"
wait_for 10 "the question in Bea's inbox" test -f "$B/spool/inbox/$QID.json"
run bea "$B" inbox
run bea "$B" allow Ana --once
run bea "$B" inbox

step "Bea drafts with the real '$OWL_HARNESS' harness (this is the slow step)"
run bea "$B" draft "$QID"

step "Bea sends"
SEND_OUT=$(run bea "$B" send "$QID")
printf '%s\n' "$SEND_OUT"
AID=${SEND_OUT##*sent }
AID=${AID%%[[:space:]]*}
[ -n "$AID" ] || { echo "e2e-real: no 'sent <id>' in: $SEND_OUT" >&2; exit 1; }

step "Ana's daemon pulls the answer"
wait_for 20 "the answer in Ana's inbox" test -f "$A/spool/inbox/$AID.json"
run ana "$A" inbox --count --format claude
run ana "$A" show "$AID"

step "Bea stopped: the same question is answered from Ana's cache"
kill "${PIDS[0]}" 2>/dev/null || true
PIDS[0]=""
sleep 0.5
run ana "$A" ask Bea "$FILE" "$QUESTION" --project "$PROJECT"

echo
echo "==> done — logs under $LOGDIR"
