#!/usr/bin/env bash
# Agent24 + Sin90 live demo driver.
#
# Runs entirely against an isolated HOME (never touches the real
# ~/.agent24). Every step's real stdout/stderr is appended to
# logs/run-demo.log; failures are recorded and the script moves on to the
# next step rather than aborting or faking output.
set -uo pipefail

DEMO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENT24_ROOT="/Users/jason/Dev/auraai/Agent24"
SIN90_ROOT="/Users/jason/Dev/auraai/sin90-design"

# NOTE: HOME must NOT live under the scratchpad path -- agent24d's
# out-of-process module socket is a Unix domain socket, and the scratchpad
# path (.../scratch pad/demo/home-cli/.agent24/run/<pid>/<x>.sock) is well
# over macOS's ~103-byte sockaddr_un limit, so the daemon degrades every
# module with "callback sockets ... would be longer than 103 bytes" (this
# is exactly the failure mode SPEC-ME3-OUT-OF-PROCESS.md's tmp_home() helper
# exists to dodge). HOME_CLI is therefore a short, isolated /tmp path --
# still never the real ~/.agent24 -- while every SCRIPT and ARTIFACT stays
# under scratchpad/demo as requested.
HOME_CLI="/tmp/a24demo-cli"
LOG_DIR="$DEMO_DIR/logs"
PKG_SRC="$DEMO_DIR/sin90-pkg-src"
LOG="$LOG_DIR/run-demo.log"

AGENT24D_BIN="$AGENT24_ROOT/rust/target/debug/agent24d"
AGENT24_CLI_BIN="$AGENT24_ROOT/rust/target/debug/agent24"
SIN90_BIN="$SIN90_ROOT/target/debug/sin90"

mkdir -p "$HOME_CLI" "$LOG_DIR" "$PKG_SRC/bin"
: > "$LOG"

step() { printf '\n==== %s ====\n' "$*" | tee -a "$LOG"; }
note() { printf '%s\n' "$*" | tee -a "$LOG"; }

# Run a command, always logging real output, never aborting the script.
run() {
  printf '\n+ %s\n' "$*" >> "$LOG"
  "$@" >>"$LOG" 2>&1
  local rc=$?
  if [ $rc -ne 0 ]; then
    printf '  (exit %d)\n' "$rc" | tee -a "$LOG"
  fi
  return $rc
}

jqf() { command -v jq >/dev/null 2>&1 && jq "$@" || cat; }

step "0. sanity: required binaries"
for f in "$AGENT24D_BIN" "$AGENT24_CLI_BIN" "$SIN90_BIN"; do
  if [ -x "$f" ]; then
    note "OK  $f"
  else
    note "MISSING $f -- run the cargo build steps first"
  fi
done

step "1. build the sin90 domain-os package directory (isolated, under scratchpad)"
cp "$SIN90_ROOT/domain-os.yml" "$PKG_SRC/domain-os.yml"
cp "$SIN90_BIN" "$PKG_SRC/bin/sin90"
chmod 755 "$PKG_SRC/bin/sin90"
note "package source: $PKG_SRC"
ls -la "$PKG_SRC" "$PKG_SRC/bin" | tee -a "$LOG"

step "2. install the module into the ISOLATED HOME (daemon down at this point)"
HOME="$HOME_CLI" run "$AGENT24_CLI_BIN" os install "$PKG_SRC"

step "3. start agent24d against the isolated HOME"
HOME="$HOME_CLI" run "$AGENT24_CLI_BIN" daemon start

DAEMON_JSON="$HOME_CLI/.agent24/daemon.json"
for i in $(seq 1 30); do
  [ -f "$DAEMON_JSON" ] && break
  sleep 1
done
if [ ! -f "$DAEMON_JSON" ]; then
  note "FATAL: $DAEMON_JSON never appeared -- daemon did not start. See log above."
else
  PORT=$(jqf -r .port < "$DAEMON_JSON")
  TOKEN=$(jqf -r .token < "$DAEMON_JSON")
  PID=$(jqf -r .pid < "$DAEMON_JSON")
  note "daemon: pid=$PID port=$PORT (token redacted, ${#TOKEN} chars)"
fi

BASE="http://127.0.0.1:${PORT:-0}"
AUTH=(-H "Authorization: Bearer ${TOKEN:-}")

curlj() { curl -sS -m 20 "${AUTH[@]}" "$@"; }

step "4. agent24 os list (module manifest + mount state)"
HOME="$HOME_CLI" run "$AGENT24_CLI_BIN" os list
note "--- raw /api/v1/os ---"
curlj "$BASE/api/v1/os" | tee -a "$LOG" | jqf .

step "5. wait for sin90 to reach state=mounted and answer through the proxy"
MOUNTED=0
for i in $(seq 1 60); do
  STATE=$(curlj "$BASE/api/v1/os" 2>/dev/null | jqf -r '.modules[] | select(.name=="sin90") | .state' 2>/dev/null)
  if [ "$STATE" = "mounted" ]; then
    PROXY_STATUS=$(curl -sS -o /tmp/_sin90_today.$$ -w '%{http_code}' -m 10 "${AUTH[@]}" "$BASE/api/v1/sin90/today" 2>/dev/null)
    if [ "$PROXY_STATUS" = "200" ]; then
      MOUNTED=1
      note "sin90 mounted and answering /today after $((i))s"
      break
    fi
  fi
  sleep 1
done
[ "$MOUNTED" = "1" ] || note "WARNING: sin90 never reached mounted+serving within 60s (state last seen: ${STATE:-unknown})"
rm -f /tmp/_sin90_today.$$ 2>/dev/null

step "6. read the sin90 human actor key (written under the isolated HOME)"
ACTOR_FILE=$(find "$HOME_CLI" -name actor-keys.json 2>/dev/null | head -1)
if [ -n "${ACTOR_FILE:-}" ]; then
  note "found: $ACTOR_FILE"
  HUMAN_KEY=$(jqf -r .human < "$ACTOR_FILE")
  note "human actor key length: ${#HUMAN_KEY}"
else
  note "FATAL: no actor-keys.json found under $HOME_CLI -- sin90 module likely never started"
fi
SIN90_AUTH=(-H "x-sin90-actor-key: ${HUMAN_KEY:-}")

step "7. create a Direction"
DIR_RESP=$(curlj "${SIN90_AUTH[@]}" -H 'content-type: application/json' \
  -d '{"title":"演示：健康与专注","target_window":"2026-Q4"}' \
  "$BASE/api/v1/sin90/directions")
echo "$DIR_RESP" | tee -a "$LOG" | jqf .
DIRECTION_ID=$(echo "$DIR_RESP" | jqf -r '.id // empty')
note "direction_id=${DIRECTION_ID:-<none>}"

step "8. create a few Tasks under that Direction"
for t in "写演示脚本" "录制截图" "回顾本周进度"; do
  TRESP=$(curlj "${SIN90_AUTH[@]}" -H 'content-type: application/json' \
    -d "{\"title\":\"$t\",\"direction_id\":${DIRECTION_ID:+\"$DIRECTION_ID\"}${DIRECTION_ID:+}}" \
    "$BASE/api/v1/sin90/tasks")
  echo "$TRESP" | tee -a "$LOG" | jqf -c .
done

step "9. create a Routine (cron) and confirm the kernel schedule row"
ROUTINE_RESP=$(curlj "${SIN90_AUTH[@]}" -H 'content-type: application/json' \
  -d '{"title":"每周3次运动","kind":"exercise","cron":"0 7 * * MON,WED,FRI","target_count":3}' \
  "$BASE/api/v1/sin90/routines")
echo "$ROUTINE_RESP" | tee -a "$LOG" | jqf .
ROUTINE_ID=$(echo "$ROUTINE_RESP" | jqf -r '.id // empty')
if [ -n "${ROUTINE_ID:-}" ]; then
  KEY="routine.$(echo "$ROUTINE_ID" | tr '[:upper:]' '[:lower:]')"
  note "routine_id=$ROUTINE_ID  kernel schedule key=$KEY"
  for i in $(seq 1 20); do
    ROW=$(curlj "$BASE/api/v1/schedules" 2>/dev/null | jqf -c ".schedules[]? // .[]? | select(.name==\"$KEY\")" 2>/dev/null)
    [ -n "$ROW" ] && break
    sleep 1
  done
  note "kernel schedule row: ${ROW:-<not found after 20s>}"
else
  note "no routine id -- routine creation likely failed, see response above"
fi

step "10. trigger run_now on that schedule (kernel endpoint) to show fired delivery"
SCHED_ID=$(echo "${ROW:-}" | jqf -r '.id // empty')
if [ -n "${SCHED_ID:-}" ]; then
  RN_STATUS=$(curl -sS -o /tmp/_run_now.$$ -w '%{http_code}' -m 10 "${AUTH[@]}" -X POST "$BASE/api/v1/schedules/$SCHED_ID/run_now")
  note "POST /api/v1/schedules/$SCHED_ID/run_now -> $RN_STATUS"
  cat /tmp/_run_now.$$ 2>/dev/null | tee -a "$LOG"
  rm -f /tmp/_run_now.$$ 2>/dev/null
  sleep 3
  note "sin90 events for this routine after run_now:"
  curlj "${SIN90_AUTH[@]}" "$BASE/api/v1/sin90/events?entity=routine&entity_id=$ROUTINE_ID" | tee -a "$LOG" | jqf .
else
  note "no kernel schedule id available -- skipping run_now"
fi

step "11. capture a couple of inbox items, then trigger AI classify (routes to local oMLX)"
curlj "${SIN90_AUTH[@]}" -H 'content-type: application/json' -d '{"text":"整理一下本周的运动记录"}' "$BASE/api/v1/sin90/capture" | tee -a "$LOG" | jqf -c .
curlj "${SIN90_AUTH[@]}" -H 'content-type: application/json' -d '{"text":"准备下周的演示材料"}' "$BASE/api/v1/sin90/capture" | tee -a "$LOG" | jqf -c .

CLS_RESP=$(curlj "${SIN90_AUTH[@]}" -H 'content-type: application/json' -d '{}' "$BASE/api/v1/sin90/ai/classify")
echo "$CLS_RESP" | tee -a "$LOG" | jqf .
RUN_ID=$(echo "$CLS_RESP" | jqf -r '.run_id // empty')
if [ -n "${RUN_ID:-}" ]; then
  note "polling run $RUN_ID (this calls oMLX at 127.0.0.1:8088, Qwen3-8B-4bit) ..."
  for i in $(seq 1 60); do
    RUN_JSON=$(curlj "${SIN90_AUTH[@]}" "$BASE/api/v1/sin90/ai/runs/$RUN_ID")
    ST=$(echo "$RUN_JSON" | jqf -r '.state // empty')
    [ "$ST" = "done" ] || [ "$ST" = "failed" ] && break
    sleep 2
  done
  note "final run state after $((i*2))s: ${ST:-unknown}"
  echo "$RUN_JSON" | tee -a "$LOG" | jqf .
  note "resulting proposals:"
  curlj "${SIN90_AUTH[@]}" "$BASE/api/v1/sin90/proposals" | tee -a "$LOG" | jqf .
else
  note "no run_id -- classify trigger failed, see response above"
fi

step "12. per-module usage"
curlj "$BASE/api/v1/usage?module=sin90" | tee -a "$LOG" | jqf .

step "DONE"
note "daemon pid=${PID:-?} port=${PORT:-?} HOME=$HOME_CLI"
note "state file: $DAEMON_JSON"
note "full log: $LOG"
note "to stop: $DEMO_DIR/stop-demo.sh"
