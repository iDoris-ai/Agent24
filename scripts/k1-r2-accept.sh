#!/usr/bin/env bash
# K1 second release (#1280) acceptance on a real machine, through the CLI and
# the daemon's HTTP API only. Cases: docs/agent/K1-R2-ACCEPTANCE.md.
#
#   scripts/k1-r2-accept.sh --old <main bin dir> --new <release bin dir> [--work <dir>]
#
# Each bin dir holds `agent24d` and `agent24` (`cargo build -p agent24d -p agent24-cli`).
# Every daemon runs on its own AGENT24_DATA_DIR under --work (Z1 checks the
# user's ~/.agent24, so stop your own daemon first). No model is needed.
# Needs curl, jq, sqlite3, python3, perl and node.
set -uo pipefail
cd "$(dirname "$0")/.."
REPO=$(pwd)

OLD="" NEW="" WORK=""
while [ $# -gt 0 ]; do
  case "$1" in
    --old) OLD=$2; shift 2 ;;
    --new) NEW=$2; shift 2 ;;
    --work) WORK=$2; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$OLD" ] && [ -n "$NEW" ] || { sed -n 2,10p "$0" >&2; exit 2; }
for b in "$OLD/agent24d" "$OLD/agent24" "$NEW/agent24d" "$NEW/agent24"; do
  [ -x "$b" ] || { echo "missing binary: $b" >&2; exit 2; }
done
for t in curl jq sqlite3 python3 perl node; do
  command -v "$t" >/dev/null || { echo "missing tool: $t" >&2; exit 2; }
done
# Both sides fully resolved, dangling links and not-yet-created tails included,
# before anything is created.
resolve() { python3 -I -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$1"; }
REAL=$(resolve "${HOME:-/nonexistent}/.agent24")
WORK=$(resolve "${WORK:-${TMPDIR:-/tmp}/k1r2-accept.$$}")
case "$WORK/" in "$REAL"/*) echo "--work must not be inside $REAL" >&2; exit 2 ;; esac
mkdir -p "$WORK" || exit 2
[ -s "$REAL/daemon.json" ] && kill -0 "$(jq -r .pid "$REAL/daemon.json")" 2>/dev/null \
  && { echo "a daemon is running on $REAL: stop it first (agent24 daemon stop)" >&2; exit 2; }
# Only what each case sets reaches a daemon or the CLI.
for v in $(env | grep -oE '^(A24|AGENT24)_[A-Z0-9_]+'); do unset "$v"; done

REPORT="$WORK/report.md"
FAILS=0
: > "$WORK/results"
touch "$WORK/.start"
[ -e "$REAL" ] && REAL_BEFORE=present || REAL_BEFORE=absent

# check ID "what" ok|fail "evidence" ["known limitation"]: a failing check
# with a known limitation is KNOWN, not FAIL; once fixed it passes.
check() {
  local mark=PASS
  if [ "$3" != ok ]; then
    if [ -n "${5:-}" ]; then mark=KNOWN; else mark=FAIL; FAILS=$((FAILS + 1)); fi
  fi
  printf '| %s | %s | %s | %s%s |\n' "$1" "$2" "$mark" "$4" "${5:+ — known: $5}" >> "$WORK/results"
  echo "$mark $1 — $2 — $4"
}
is() { [ "$1" = "$2" ] && echo ok || echo fail; }
has() { grep -qF -- "$2" "$1" && echo ok || echo fail; }
lacks() { grep -qF -- "$2" "$1" && echo fail || echo ok; }
limit() { perl -e 'alarm shift; exec @ARGV' "$@"; }

PID="" PORT="" TOKEN="" DATA=""
# start_daemon BIN_DIR DATA_DIR [VAR=value ...]: 0 once ready, 1 if it exited.
start_daemon() {
  local bin=$1; DATA=$2; shift 2
  mkdir -p "$DATA"; rm -f "$DATA/daemon.json"
  env AGENT24_DATA_DIR="$DATA" A24_OS_PACKAGES="$DATA.pk" "$@" \
    "$bin/agent24d" serve > "$DATA.serve.log" 2>&1 &
  PID=$!
  for _ in $(seq 1 60); do
    if [ -s "$DATA/daemon.json" ]; then
      PORT=$(jq -r .port "$DATA/daemon.json"); TOKEN=$(jq -r .token "$DATA/daemon.json"); return 0
    fi
    kill -0 "$PID" 2>/dev/null || { wait "$PID"; PID=""; return 1; }
    sleep 0.5
  done
  stop_daemon; return 1
}
# stop_daemon: SIGTERM, then SIGKILL after 10 s, so no daemon outlives a case
stop_daemon() {
  [ -n "$PID" ] || return 0
  kill -TERM "$PID" 2>/dev/null
  for _ in $(seq 1 20); do kill -0 "$PID" 2>/dev/null || break; sleep 0.5; done
  kill -0 "$PID" 2>/dev/null && { echo "daemon $PID ignored SIGTERM; killing it" >&2; kill -KILL "$PID"; }
  wait "$PID" 2>/dev/null; PID=""
}
trap 'stop_daemon' EXIT

# api METHOD PATH [BODY] -> body in $WORK/r.json, status in $CODE
api() {
  CODE=$(curl -s -m 20 -o "$WORK/r.json" -w '%{http_code}' -X "$1" \
    -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
    ${3:+-d "$3"} "http://127.0.0.1:$PORT$2")
}
# wait_run ID: until the run has ended
wait_run() {
  for _ in $(seq 1 40); do
    api GET "/api/v1/runs/$1"
    case $(jq -r .status "$WORK/r.json") in completed|failed|cancelled) return 0 ;; esac
    sleep 0.5
  done
  return 1
}
peer() { api GET "/api/v1/peer/runs/$1"; jq -r .withheld "$WORK/r.json"; }
NOTICE="this run used sources that are not ordinary, so its content is not shared with connected tools; open it in Agent24 to see it"
# withheld ID: ok when a peer gets the run withheld, with the notice and nothing else
withheld() {
  api GET "/api/v1/peer/runs/$1"
  jq -e --arg n "$NOTICE" '.withheld == true and .notice == $n and .prompt == null and .output == null and .error == null' \
    "$WORK/r.json" > /dev/null && echo ok || echo fail
}
db() { sqlite3 "$DATA/agent24.db" "$1"; }
iso() { python3 -c "import datetime,sys;print((datetime.datetime.now(datetime.timezone.utc)+datetime.timedelta(seconds=int(sys.argv[1]))).strftime('%Y-%m-%dT%H:%M:%SZ'))" "$1"; }
# grant MODULE OP AT: a granted row as an earlier build wrote it (no artifact)
grant() {
  db "INSERT OR REPLACE INTO module_consents (module, op, module_version, scope_fingerprint, source, risk, decision, decided_at, expires_at)
      VALUES ('$1', '$2', '0.1.0', 'fp-accept', 'manual_install', 'high', 'granted', '$3', '$(iso 1728000)')"
}
consent_status() { api GET /api/v1/module-consents; jq -r --arg m "$1" --arg t "$2" '.authorizations[] | select(.module==$m and .tool==$t) | .status' "$WORK/r.json"; }

echo "== 1. an old library, written by main ($OLD)"
start_daemon "$OLD" "$WORK/old" || { echo "main daemon did not start: $WORK/old.serve.log" >&2; exit 1; }
api POST /api/v1/schedules '{"name":"accept user schedule","spec":{"type":"every","secs":86400},"action":{"type":"agent_run","prompt":"USER-SCHEDULE-MARKER"}}'
SCH=$(jq -r .id "$WORK/r.json")
api POST /api/v1/runs '{"prompt":"OLD-RUN-MARKER"}'
OLD_RUN=$(jq -r .id "$WORK/r.json")
wait_run "$OLD_RUN"
stop_daemon
COLS=$(db "SELECT group_concat(name, ',') FROM pragma_table_info('schedules') WHERE name NOT IN ('id', 'name', 'action')")
db "INSERT INTO schedules (id, name, action, $COLS)
    SELECT 'wake_01ACCEPTOLD', 'self-wake', json_set(action, '\$.prompt', 'WAKE-MARKER'), $COLS FROM schedules WHERE id = '$SCH'"
RCOLS=$(db "SELECT group_concat(name, ',') FROM pragma_table_info('runs') WHERE name NOT IN ('id', 'schedule_id')")
db "INSERT INTO runs (id, schedule_id, $RCOLS) SELECT 'run_01ACCEPTWAKEGONE', 'wake_01ACCEPTGONE', $RCOLS FROM runs WHERE id = '$OLD_RUN'"
grant node-ref echo "$(iso -3600)"
grant documents find "$(iso -3600)"
OLD_VERSION=$(db "SELECT max(version) FROM _sqlx_migrations")
echo "old library: migration $OLD_VERSION, schedule $SCH, run $OLD_RUN"
cp -R "$WORK/old" "$WORK/up"

echo "== 2. upgrade with the release ($NEW)"
if start_daemon "$NEW" "$WORK/up"; then UP=ok; else UP=fail; fi
check U1 "release daemon opens the main library" "$UP" "migration $OLD_VERSION before; log $WORK/up.serve.log"
[ "$UP" = ok ] || { cat "$WORK/up.serve.log" >&2; exit 1; }
api GET /api/v1/health
check U2 "health after the upgrade" "$(is "$CODE" 200)" "$(cat "$WORK/r.json")"
APPLIED=$(db "SELECT group_concat(version) FROM _sqlx_migrations WHERE version > $OLD_VERSION AND success = 1")
MEM=$(sqlite3 "$DATA/memory.db" "SELECT max(version) FROM _sqlx_migrations")
check U3 "store migrations 19–26 and memory 18 applied" "$(is "$APPLIED/$MEM" "19,20,21,22,23,24,25,26/18")" "store $APPLIED; memory $MEM"
api GET /api/v1/schedules; jq -r '.schedules[]?.id // .[]?.id' "$WORK/r.json" > "$WORK/ids"
api GET "/api/v1/runs/$OLD_RUN"
check U4 "old schedules and runs are still there" "$( (grep -qx "$SCH" "$WORK/ids" && grep -qx wake_01ACCEPTOLD "$WORK/ids" && grep -q OLD-RUN-MARKER "$WORK/r.json") && echo ok || echo fail)" "schedule $SCH, wake_01ACCEPTOLD, run $OLD_RUN"

S1=$(consent_status node-ref echo); S2=$(consent_status documents find)
# Known only as #1290 shows it (listed granted); a lost grant still fails.
K=""; [ "$S1/$S2" = granted/granted ] && K="the list does not compare the package artifact; the gate refuses the call (#1290)"
check G1 "a grant from before the upgrade is stale" "$(is "$S1/$S2" stale/stale)" "node-ref/echo $S1, documents/find $S2" "$K"
api POST /api/v1/module-consents/modules/node-ref/tools/echo '{}'
check G2 "the grant POST is closed by default" "$(is "$CODE/$(jq -r .error.code "$WORK/r.json")" 403/grants_closed)" "$CODE $(jq -c .error.code "$WORK/r.json")"

ORIGIN=$(db "SELECT group_concat(schedule_id || '=' || classification, ' ') FROM (SELECT * FROM schedule_source_origin ORDER BY schedule_id)")
check S1 "self-wakes from before the upgrade read as unknown, the user's schedule has no origin" "$(is "$ORIGIN" "wake_01ACCEPTGONE=unknown wake_01ACCEPTOLD=unknown")" "$ORIGIN"
check V1 "an old run is withheld from peers" "$(withheld "$OLD_RUN")" "$OLD_RUN"
check V2 "a run of a deleted old self-wake is withheld" "$(withheld run_01ACCEPTWAKEGONE)" "run_01ACCEPTWAKEGONE"

api POST /api/v1/schedules/wake_01ACCEPTOLD/run_now; WAKE_RUN=$(jq -r .run_id "$WORK/r.json"); wait_run "$WAKE_RUN"
api POST "/api/v1/schedules/$SCH/run_now"; USER_RUN=$(jq -r .run_id "$WORK/r.json"); wait_run "$USER_RUN"
W=$(withheld "$WAKE_RUN"); U=$(peer "$USER_RUN"); UP_PROMPT=$(jq -r .prompt "$WORK/r.json")
check S2 "an old self-wake firing now is withheld, the user's schedule is not" "$(is "$W/$U/$UP_PROMPT" "ok/false/USER-SCHEDULE-MARKER")" "wake run $WAKE_RUN $W; user run $USER_RUN $U"

api POST /api/v1/runs '{"prompt":"NEW-ORDINARY-MARKER"}'; NEW_RUN=$(jq -r .id "$WORK/r.json"); wait_run "$NEW_RUN"
api POST /api/v1/sessions '{}'; SESS=$(jq -r .id "$WORK/r.json")
api POST /api/v1/runs "{\"prompt\":\"SESSION-MARKER\",\"session_id\":\"$SESS\"}"; SESS_RUN=$(jq -r .id "$WORK/r.json"); wait_run "$SESS_RUN"
N=$(peer "$NEW_RUN"); S=$(withheld "$SESS_RUN")
check V3 "a new run with no session is shared, a session run is withheld" "$(is "$N/$S" false/ok)" "$NEW_RUN $N; $SESS_RUN $S"
api GET /api/v1/peer/runs; cp "$WORK/r.json" "$WORK/peer.json"
LEAK=$( [ "$CODE" = 200 ] && [ "$(jq '.runs | length' "$WORK/peer.json")" -ge 6 ] && echo ok || echo fail)
for m in OLD-RUN-MARKER WAKE-MARKER SESSION-MARKER; do [ "$(lacks "$WORK/peer.json" "$m")" = ok ] || LEAK=fail; done
check V4 "the peer list carries no withheld content" "$LEAK" "$WORK/peer.json"

printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"accept","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agent24_list_runs","arguments":{}}}' \
  | (cat; sleep 5) | AGENT24_DATA_DIR="$DATA" limit 30 "$NEW/agent24" mcp 2> "$WORK/mcp.err" | grep '"id":2' > "$WORK/mcp.out"
jq -r '.result.content[0].text' "$WORK/mcp.out" > "$WORK/mcp.runs" 2>/dev/null
MCP=$( (grep -q NEW-ORDINARY-MARKER "$WORK/mcp.runs" && grep -qF "$NOTICE" "$WORK/mcp.runs" \
  && ! grep -qE 'OLD-RUN-MARKER|WAKE-MARKER|SESSION-MARKER' "$WORK/mcp.runs") && echo ok || echo fail)
check V5 "MCP agent24_list_runs shows the shared run and hides the rest" "$MCP" "$WORK/mcp.out"

cat > "$WORK/acp.py" <<'EOF'
import json, signal, subprocess, sys
p = subprocess.Popen([sys.argv[1], "acp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
def give_up(*_):
    p.kill(); p.wait(); sys.exit("acp timed out")
signal.signal(signal.SIGALRM, give_up); signal.alarm(60)
def call(i, method, params):
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params}) + "\n"); p.stdin.flush()
    seen = []
    for line in p.stdout:
        seen.append(line.strip())
        if json.loads(line).get("id") == i:
            return json.loads(line), seen
    sys.exit("acp closed")
try:
    call(1, "initialize", {"protocolVersion": 1})
    sid = call(2, "session/new", {"cwd": "/", "mcpServers": []})[0]["result"]["sessionId"]
    print("\n".join(call(3, "session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": "ACP-MARKER"}]})[1]))
finally:
    p.kill(); p.wait()
EOF
AGENT24_DATA_DIR="$DATA" python3 -I "$WORK/acp.py" "$NEW/agent24" > "$WORK/acp.out" 2>&1
ACP=$( (grep -qF "$NOTICE" "$WORK/acp.out" && ! grep -qE 'ACP-MARKER|provider' "$WORK/acp.out") && echo ok || echo fail)
check V6 "an ACP prompt (always in a session) gets only the fixed notice" "$ACP" "$WORK/acp.out"
stop_daemon

echo "== 3. the grant switch"
start_daemon "$NEW" "$WORK/up" A24_MODULE_TOOL_GRANTS=1 A24_MODULE_GRANT_MAX_DAYS=90
api POST /api/v1/module-consents/modules/node-ref/tools/echo '{}'
check G3 "A24_MODULE_TOOL_GRANTS=1 opens it (max 90 days accepted)" "$(is "$CODE/$(jq -r .error.code "$WORK/r.json")" 404/not_found)" "$CODE $(jq -c .error.code "$WORK/r.json") — past the switch; node-ref is not installed yet"
stop_daemon
for bad in A24_MODULE_TOOL_GRANTS=yes A24_MODULE_GRANT_MAX_DAYS=91; do
  if start_daemon "$NEW" "$WORK/up" "$bad"; then R=fail; stop_daemon; else R=$(has "$WORK/up.serve.log" "${bad%%=*}"); fi
  check G4 "the daemon refuses to start with $bad" "$R" "$(grep -m1 -o "serve failed: ${bad%%=*}.*" "$WORK/up.serve.log" | cut -c1-120)"
done

echo "== 4. a protocol-1 module, install and disable"
AGENT24_DATA_DIR="$WORK/up" A24_OS_PACKAGES="$WORK/up.pk" "$NEW/agent24" os install "$REPO/examples/node-module" > "$WORK/g5.out" 2>&1; RC=$?
DATA="$WORK/up"; S=$(sqlite3 "$DATA/agent24.db" "SELECT count(*) FROM module_consent_revocations WHERE module = 'node-ref'")
check G5 "os install revokes earlier grants of the name" "$(is "$RC/$S" 0/1)" "exit $RC; revocation rows $S"
grant node-ref echo "$(iso 1)"; sleep 2
start_daemon "$NEW" "$WORK/up"
sleep 3; api GET /api/v1/os
NODE=$(jq -r '.modules[] | select(.name=="node-ref") | .state' "$WORK/r.json")
check P1 "a module with no protocol declared (1) still mounts" "$(is "$NODE" mounted)" "node-ref state $NODE"
api PATCH /api/v1/os/node-ref '{"enabled":false}'; PATCHED=$CODE
S=$(consent_status node-ref echo)
check G6 "disabling a module revokes its grants" "$(is "$PATCHED/$S" 200/revoked)" "PATCH $PATCHED; node-ref/echo $S"
stop_daemon

echo "== 5. os install / uninstall with no HOME"
NOHOME() { env -i PATH="$PATH" A24_OS_PACKAGES="$WORK/nohome.pk" "$@"; }
NOHOME "$NEW/agent24" os install "$REPO/examples/node-module" > "$WORK/h1.out" 2>&1; RC=$?
check H1 "os install with no HOME completes and says nothing was revoked" "$( [ $RC = 0 ] && is "$(has "$WORK/h1.out" installed)$(has "$WORK/h1.out" 'HOME is not set')" okok || echo fail)" "exit $RC; $(tr '\n' ' ' < "$WORK/h1.out" | cut -c1-200)"
NOHOME "$NEW/agent24" os uninstall node-ref > "$WORK/h2.out" 2>&1; RC=$?
check H2 "os uninstall with no HOME completes and says so" "$( [ $RC = 0 ] && has "$WORK/h2.out" 'HOME is not set' || echo fail)" "exit $RC; $(tr '\n' ' ' < "$WORK/h2.out" | cut -c1-200)"
NOHOME "$OLD/agent24" os install "$REPO/examples/node-module" > "$WORK/h3.out" 2>&1; RC=$?
check H3 "for comparison: main exits non-zero there" "$( [ $RC != 0 ] && echo ok || echo fail)" "exit $RC; $(tr '\n' ' ' < "$WORK/h3.out" | cut -c1-200)"

echo "== 6. no way back"
cp -R "$WORK/up" "$WORK/down"; rm -f "$WORK/down/daemon.json"
dump() { sqlite3 "$WORK/down/agent24.db" .dump | shasum; }
BEFORE=$(dump)
if start_daemon "$OLD" "$WORK/down"; then R=fail; stop_daemon; else R=$(has "$WORK/down.serve.log" "previously applied but is missing"); fi
[ "$(dump)" = "$BEFORE" ] || R=fail
check D1 "main refuses the upgraded library and leaves it alone" "$R" "$(grep -m1 -o 'migration [0-9]* was previously applied[^"]*' "$WORK/down.serve.log" | cut -c1-90); dump unchanged: $( [ "$(dump)" = "$BEFORE" ] && echo yes || echo no)"
if start_daemon "$NEW" "$WORK/down"; then R=ok; stop_daemon; else R=fail; fi
check D2 "the release still opens it afterwards" "$R" "$WORK/down.serve.log"

if [ "$REAL_BEFORE" = absent ]; then [ -e "$REAL" ] && R=fail || R=ok
else [ -z "$(find "$REAL" -newer "$WORK/.start" -print -quit)" ] && R=ok || R=fail; fi
check Z1 "the user's ~/.agent24 was not touched" "$R" "$REAL $REAL_BEFORE before"

{
  echo "# K1 r2 acceptance — $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo
  echo "- old: \`$("$OLD/agent24d" --version)\` ($OLD)"
  echo "- new: \`$("$NEW/agent24d" --version)\` ($NEW)"
  echo "- machine: $(uname -sm), $(sw_vers -productVersion 2>/dev/null || uname -r); work dir $WORK"
  echo
  echo "| ID | check | result | evidence |"
  echo "|---|---|---|---|"
  cat "$WORK/results"
  echo
  echo "$(grep -c '| PASS |' "$WORK/results") passed, $(grep -c '| KNOWN |' "$WORK/results") known limitation(s), $FAILS failed."
} > "$REPORT"
echo "report: $REPORT"
[ "$FAILS" = 0 ]
