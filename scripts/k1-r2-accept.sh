#!/usr/bin/env bash
# K1 second release (#1280) acceptance on a real machine. The release is driven
# as a user drives it: `agent24` and its MCP and ACP bridges; the daemon's HTTP
# API only where no product entry exists. Cases: docs/agent/K1-R2-ACCEPTANCE.md.
#
#   scripts/k1-r2-accept.sh --old <main bin dir> --new <release bin dir> [--work <dir>]
#
# Run it from the release checkout: U3 expects the migrations found here.
# Each bin dir holds `agent24d` and `agent24` (`cargo build -p agent24d -p agent24-cli`).
# Every daemon runs on its own AGENT24_DATA_DIR under --work (Z1 checks the
# user's ~/.agent24, so stop your own daemon first). No model is needed.
# Needs curl, jq, sqlite3, python3, perl and node.
set -uo pipefail
# perl (the time limits) warns on a locale it cannot load; tr and grep stay byte-wise.
export LC_ALL=C
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
NEW=$(resolve "$NEW")
# reap finds leftover daemons by this path in `ps`, which escapes other bytes
# under LC_ALL=C
printf '%s' "$NEW" | grep -q '[^ -~]' && { echo "--new must be an ASCII path: $NEW" >&2; exit 2; }
WORK=$(resolve "${WORK:-${TMPDIR:-/tmp}/k1r2-accept.$$}")
[ -s "$REAL/daemon.json" ] && kill -0 "$(jq -r .pid "$REAL/daemon.json")" 2>/dev/null \
  && { echo "a daemon is running on $REAL: stop it first (agent24 daemon stop)" >&2; exit 2; }
# inside DIR TOP: DIR is TOP or below it, both absolute. TOP is cut to its
# nearest existing ancestor and the missing rest; an existing ancestor of DIR
# that is that same directory by inode (a firmlink or bind mount names it
# otherwise) is followed by DIR's rest, compared without case (a
# case-insensitive volume takes ~/.Agent24 for ~/.agent24).
lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }
inside() {
  local top=$2 missing="" a=$1 rest=""
  while [ ! -e "$top" ]; do missing="/$(basename "$top")$missing"; top=$(dirname "$top"); done
  while :; do
    if [ -e "$a" ] && [ "$a" -ef "$top" ]; then
      case "$(lower "$rest")/" in "$(lower "$missing")"/*) return 0 ;; esac
    fi
    [ "$a" = / ] && return 1
    rest="/$(basename "$a")$rest"; a=$(dirname "$a")
  done
}
inside "$WORK" "$REAL" && { echo "--work must not be inside $REAL" >&2; exit 2; }
[ -z "$(ls -A "$WORK" 2>/dev/null)" ] || { echo "--work must be new or empty: $WORK" >&2; exit 2; }
mkdir -p "$WORK" || exit 2
# Only what each case sets reaches a daemon or the CLI.
for v in $(env | grep -oE '^(A24|AGENT24)_[A-Z0-9_]+'); do unset "$v"; done

REPORT="$WORK/report.md"
FAILS=0
: > "$WORK/results"
touch "$WORK/.start"
# release_daemons: the pids of `$NEW/agent24d serve` processes (the command
# line `agent24 daemon start` gives the daemon next to it), less those that were
# there before this run
release_daemons() {
  ps -axo pid=,command= | awk -v c="$NEW/agent24d serve" '{ pid = $1; sub(/^ *[0-9]+ /, ""); if (index($0, c) == 1) print pid }' \
    | grep -vxF -f "$WORK/.before"
}
: > "$WORK/.before"; release_daemons > "$WORK/.before.tmp"; mv "$WORK/.before.tmp" "$WORK/.before"
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

PID="" PORT="" TOKEN="" DATA="" BY_CLI=""
# start_daemon BIN_DIR DATA_DIR [VAR=value ...]: agent24d itself, its log kept
# (main, whose `agent24 daemon start` does not say why a start failed); 0 once
# ready, 1 if it exited.
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
# reap: kill every release daemon this run left behind. A start that gave up
# waiting leaves its daemon running (kill_on_drop is off for a resident one),
# with or without a state file; no pid is taken from a state file, which may
# be stale.
reap() {
  local pid
  for pid in $(release_daemons); do
    echo "a release daemon outlived its case (pid $pid); killing it" >&2
    kill -KILL "$pid" 2>/dev/null
  done
}
# start_cli BIN_DIR DATA_DIR [VAR=value ...]: `agent24 daemon start`, as a user
# starts it; 0 once it started, 1 otherwise, its words in DATA_DIR.start.out
start_cli() {
  local bin=$1; DATA=$2; shift 2
  mkdir -p "$DATA"; rm -f "$DATA/daemon.json"
  env AGENT24_DATA_DIR="$DATA" A24_OS_PACKAGES="$DATA.pk" "$@" perl -e 'alarm shift; exec @ARGV' 60 \
    "$bin/agent24" daemon start > "$DATA.start.out" 2>&1 || { reap; return 1; }
  PID=$(jq -r .pid "$DATA/daemon.json"); PORT=$(jq -r .port "$DATA/daemon.json")
  TOKEN=$(jq -r .token "$DATA/daemon.json"); BY_CLI=$bin
}
# stop_daemon: `agent24 daemon stop` (or SIGTERM to a daemon started without
# it), then SIGKILL after 10 s, so no daemon outlives a case. A `daemon stop`
# that failed or needed the SIGKILL is counted for U5.
STOPS=0 STOP_FAILS=""
stop_daemon() {
  [ -n "$PID" ] || return 0
  local rc=0
  if [ -n "$BY_CLI" ]; then
    env AGENT24_DATA_DIR="$DATA" perl -e 'alarm shift; exec @ARGV' 30 "$BY_CLI/agent24" daemon stop > "$DATA.stop.out" 2>&1 || rc=$?
  else
    kill -TERM "$PID" 2>/dev/null
  fi
  for _ in $(seq 1 20); do kill -0 "$PID" 2>/dev/null || break; sleep 0.5; done
  if kill -0 "$PID" 2>/dev/null; then
    echo "daemon $PID did not stop; killing it" >&2; kill -KILL "$PID"; rc=killed
  fi
  if [ -n "$BY_CLI" ]; then
    STOPS=$((STOPS + 1)); [ "$rc" = 0 ] || STOP_FAILS="$STOP_FAILS $DATA:$rc"
  else
    wait "$PID" 2>/dev/null
  fi
  PID="" BY_CLI=""
}
trap 'stop_daemon; reap' EXIT
# cli ARGS...: the release's agent24 on the current library, time-limited
cli() { env AGENT24_DATA_DIR="$DATA" A24_OS_PACKAGES="$DATA.pk" perl -e 'alarm shift; exec @ARGV' 60 "$NEW/agent24" "$@"; }

# api METHOD PATH [BODY] -> body in $WORK/r.json, status in $CODE; no answer
# leaves no r.json, so an earlier body is never read as this one's
api() {
  rm -f "$WORK/r.json"
  CODE=$(curl -s -m 20 -o "$WORK/r.json" -w '%{http_code}' -X "$1" \
    -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
    ${3:+-d "$3"} "http://127.0.0.1:$PORT$2")
}
# wait_old_run ID: until the run has ended, asked of main's daemon (main's
# agent24 has no `runs`)
wait_old_run() {
  for _ in $(seq 1 40); do
    api GET "/api/v1/runs/$1"
    case $(jq -r .status "$WORK/r.json") in completed|failed|cancelled) return 0 ;; esac
    sleep 0.5
  done
  return 1
}
# wait_run ID: the same through `agent24 runs show`
wait_run() {
  for _ in $(seq 1 40); do
    case $(cli runs show "$1" --json 2>/dev/null | jq -r .status 2>/dev/null) in completed|failed|cancelled) return 0 ;; esac
    sleep 0.5
  done
  return 1
}
# run_with MARKER: the id of the run whose prompt holds MARKER, from `agent24 runs list`
run_with() { cli runs list --json | jq -r --arg m "$1" '[.runs[] | select(.input.prompt | contains($m)) | .id][0] // empty'; }
peer() { api GET "/api/v1/peer/runs/$1"; jq -r .withheld "$WORK/r.json"; }
NOTICE="this run used sources that are not ordinary, so its content is not shared with connected tools; open it in Agent24 to see it"
# withheld ID: ok when a peer gets the run withheld, with the notice and nothing else
WITHHELD='.withheld == true and .notice == $n and .prompt == null and .output == null and .error == null'
withheld() {
  api GET "/api/v1/peer/runs/$1"
  [ "$CODE" = 200 ] && jq -e --arg n "$NOTICE" "$WITHHELD" "$WORK/r.json" > /dev/null && echo ok || echo fail
}
# peer_list FILE: ok when the list holds at least 6 runs, the run with
# NEW-ORDINARY-MARKER shared among them, and at least 4 others, each withheld
# as `withheld` checks it
peer_list() {
  jq -e --arg n "$NOTICE" "(.runs | length) >= 6
    and any(.runs[]; .withheld == false and .prompt == \"NEW-ORDINARY-MARKER\")
    and ([.runs[] | select(.withheld != false)] | length >= 4 and all($WITHHELD))" "$1" > /dev/null \
    && echo ok || echo fail
}
db() { sqlite3 "$DATA/agent24.db" "$1"; }
iso() { python3 -c "import datetime,sys;print((datetime.datetime.now(datetime.timezone.utc)+datetime.timedelta(seconds=int(sys.argv[1]))).strftime('%Y-%m-%dT%H:%M:%SZ'))" "$1"; }
# grant MODULE OP AT: a granted row as an earlier build wrote it (no artifact)
grant() {
  db "INSERT OR REPLACE INTO module_consents (module, op, module_version, scope_fingerprint, source, risk, decision, decided_at, expires_at)
      VALUES ('$1', '$2', '0.1.0', 'fp-accept', 'manual_install', 'high', 'granted', '$3', '$(iso 1728000)')"
}
consent_status() { api GET /api/v1/module-consents; jq -r --arg m "$1" --arg t "$2" '.authorizations[] | select(.module==$m and .tool==$t) | .status' "$WORK/r.json"; }

# bridge mcp TOOL ARGS_JSON | bridge acp PROMPT: one call over the release's
# `agent24 mcp` (its answer) or one ACP session/prompt (every line until the answer)
cat > "$WORK/bridge.py" <<'EOF'
import json, signal, subprocess, sys
kind = sys.argv[2]
p = subprocess.Popen([sys.argv[1], kind], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
def give_up(*_):
    p.kill(); p.wait(); sys.exit(kind + " timed out")
signal.signal(signal.SIGALRM, give_up); signal.alarm(90)
def send(message):
    p.stdin.write(json.dumps({"jsonrpc": "2.0", **message}) + "\n"); p.stdin.flush()
def call(i, method, params):
    send({"id": i, "method": method, "params": params})
    seen = []
    for line in p.stdout:
        seen.append(line.strip())
        if json.loads(line).get("id") == i:
            return json.loads(line), seen
    sys.exit(kind + " closed")
try:
    if kind == "mcp":
        call(1, "initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "accept", "version": "0"}})
        send({"method": "notifications/initialized"})
        print(json.dumps(call(2, "tools/call", {"name": sys.argv[3], "arguments": json.loads(sys.argv[4])})[0]))
    else:
        call(1, "initialize", {"protocolVersion": 1})
        sid = call(2, "session/new", {"cwd": "/", "mcpServers": []})[0]["result"]["sessionId"]
        print("\n".join(call(3, "session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": sys.argv[3]}]})[1]))
finally:
    p.kill(); p.wait()
EOF
bridge() { env AGENT24_DATA_DIR="$DATA" python3 -I "$WORK/bridge.py" "$NEW/agent24" "$@"; }

echo "== 1. an old library, written by main ($OLD)"
start_daemon "$OLD" "$WORK/old" || { echo "main daemon did not start: $WORK/old.serve.log" >&2; exit 1; }
api POST /api/v1/schedules '{"name":"accept user schedule","spec":{"type":"every","secs":86400},"action":{"type":"agent_run","prompt":"USER-SCHEDULE-MARKER"}}'
SCH=$(jq -r .id "$WORK/r.json")
api POST /api/v1/runs '{"prompt":"OLD-RUN-MARKER"}'
OLD_RUN=$(jq -r .id "$WORK/r.json")
wait_old_run "$OLD_RUN"
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
if start_cli "$NEW" "$WORK/up"; then UP=ok; else UP=fail; fi
check U1 "agent24 daemon start opens the main library with the release" "$UP" "migration $OLD_VERSION before; $(head -1 "$WORK/up.start.out")"
[ "$UP" = ok ] || { cat "$WORK/up.start.out" >&2; exit 1; }
cli daemon status > "$WORK/u2.out" 2>&1; RC=$?
check U2 "agent24 daemon status after the upgrade" "$( [ $RC = 0 ] && grep -q '^running · ' "$WORK/u2.out" && echo ok || echo fail)" "exit $RC; $(head -1 "$WORK/u2.out")"
# Every migration this checkout carries past main's, and at least 19–26
versions() { ls "$1" | grep -oE '^[0-9]+' | sed 's/^0*//' | sort -nu; }
WANT=$(versions "$REPO/rust/crates/agent24-store/migrations" | awk -v o="$OLD_VERSION" '$1 > o' | paste -sd, -)
WANT_MEM=$(versions "$REPO/rust/crates/agent24-memory/migrations" | tail -1)
case "$WANT," in 19,20,21,22,23,24,25,26,*) ;; *) WANT="at least 19–26, found $WANT" ;; esac
APPLIED=$(db "SELECT group_concat(version) FROM (SELECT version FROM _sqlx_migrations WHERE version > $OLD_VERSION AND success = 1 ORDER BY version)")
MEM=$(sqlite3 "$DATA/memory.db" "SELECT max(version) FROM _sqlx_migrations")
check U3 "store migrations $WANT and memory $WANT_MEM applied" "$( [ "$WANT_MEM" -ge 18 ] 2>/dev/null && is "$APPLIED/$MEM" "$WANT/$WANT_MEM" || echo fail)" "store $APPLIED; memory $MEM"
cli schedules list --json > "$WORK/sch.json" 2>&1; jq -r '.schedules[]?.id // .[]?.id' "$WORK/sch.json" > "$WORK/ids"
cli runs show "$OLD_RUN" --json > "$WORK/u4.run" 2>&1
check U4 "agent24 schedules list / runs show still have the old ones" "$( (grep -qx "$SCH" "$WORK/ids" && grep -qx wake_01ACCEPTOLD "$WORK/ids" && grep -q OLD-RUN-MARKER "$WORK/u4.run") && echo ok || echo fail)" "schedule $SCH, wake_01ACCEPTOLD, run $OLD_RUN"

S1=$(consent_status node-ref echo); S2=$(consent_status documents find)
check G1 "a grant from before the upgrade is stale" "$(is "$S1/$S2" stale/stale)" "node-ref/echo $S1, documents/find $S2"
api POST /api/v1/module-consents/modules/node-ref/tools/echo '{}'
check G2 "the grant POST is closed by default" "$(is "$CODE/$(jq -r .error.code "$WORK/r.json")" 403/grants_closed)" "$CODE $(jq -c .error.code "$WORK/r.json")"

ORIGIN=$(db "SELECT group_concat(schedule_id || '=' || classification, ' ') FROM (SELECT * FROM schedule_source_origin ORDER BY schedule_id)")
check S1 "self-wakes from before the upgrade read as unknown, the user's schedule has no origin" "$(is "$ORIGIN" "wake_01ACCEPTGONE=unknown wake_01ACCEPTOLD=unknown")" "$ORIGIN"
check V1 "an old run is withheld from peers" "$(withheld "$OLD_RUN")" "$OLD_RUN"
check V2 "a run of a deleted old self-wake is withheld" "$(withheld run_01ACCEPTWAKEGONE)" "run_01ACCEPTWAKEGONE"

WAKE_RUN=$(cli schedules run-now wake_01ACCEPTOLD | sed -n 's/^started //p'); wait_run "$WAKE_RUN"
USER_RUN=$(cli schedules run-now "$SCH" | sed -n 's/^started //p'); wait_run "$USER_RUN"
W=$(withheld "$WAKE_RUN"); U=$(peer "$USER_RUN"); UP_PROMPT=$(jq -r .prompt "$WORK/r.json")
check S2 "agent24 schedules run-now: an old self-wake is withheld, the user's schedule is not" "$(is "$W/$U/$UP_PROMPT" "ok/false/USER-SCHEDULE-MARKER")" "wake run $WAKE_RUN $W; user run $USER_RUN $U"

# A run an MCP client starts has no session; an ACP prompt is always in one.
bridge mcp agent24_run '{"prompt":"NEW-ORDINARY-MARKER"}' > "$WORK/v3.mcp" 2>&1
bridge acp ACP-MARKER > "$WORK/acp.out" 2>&1
NEW_RUN=$(run_with NEW-ORDINARY-MARKER); ACP_RUN=$(run_with ACP-MARKER); wait_run "$ACP_RUN"
N=$(peer "$NEW_RUN"); S=$(withheld "$ACP_RUN")
# The MCP answer is that run's own end, as agent24_run gives a shared run back:
# its output, or its error as `code: message`; any other error text is not
cli runs show "$NEW_RUN" --json > "$WORK/v3.run" 2>/dev/null
END=$(jq -r .status "$WORK/v3.run" 2>/dev/null)
EXPECT=$(jq -r 'if .status == "completed" then (.output.text // "") else "\(.error.code): \(.error.message)" end' "$WORK/v3.run" 2>/dev/null)
GOT=$(jq -r '.result.content[0].text' "$WORK/v3.mcp" 2>/dev/null)
# A tool answer at all (not a bridge or JSON-RPC error), an error exactly when the run failed
ANSWERED=$(jq -e --arg end "$END" '(.result.content[0].text | type == "string")
  and ((.result.isError // false) == ($end == "failed"))' "$WORK/v3.mcp" > /dev/null 2>&1 && echo yes)
M=$(case "$END" in (completed|failed) [ "$ANSWERED" = yes ] && [ "$GOT" = "$EXPECT" ] && echo ok || echo fail ;; (*) echo fail ;; esac)
check V3 "a run started over MCP (no session) is shared, an ACP run (a session) is withheld" "$(is "$N/$M/$S" false/ok/ok)" "MCP run $NEW_RUN $N, answer $(jq -c '.result.content[0].text' "$WORK/v3.mcp" 2>/dev/null | cut -c1-80); ACP run $ACP_RUN $S"
api GET /api/v1/peer/runs; cp "$WORK/r.json" "$WORK/peer.json"
LEAK=$( [ "$CODE" = 200 ] && peer_list "$WORK/peer.json" || echo fail)
for m in OLD-RUN-MARKER WAKE-MARKER ACP-MARKER; do [ "$(lacks "$WORK/peer.json" "$m")" = ok ] || LEAK=fail; done
check V4 "the peer list carries no withheld content" "$LEAK" "$WORK/peer.json"

bridge mcp agent24_list_runs '{}' > "$WORK/mcp.out" 2>&1
# The tool's text is the daemon's peer list as it came
jq -r '.result.content[0].text' "$WORK/mcp.out" > "$WORK/mcp.runs" 2>/dev/null
MCP=$( [ "$(peer_list "$WORK/mcp.runs")" = ok ] \
  && ! grep -qE 'OLD-RUN-MARKER|WAKE-MARKER|ACP-MARKER' "$WORK/mcp.runs" && echo ok || echo fail)
check V5 "MCP agent24_list_runs shows the shared run and hides the rest" "$MCP" "$WORK/mcp.out"

ACP=$( (grep -qF "$NOTICE" "$WORK/acp.out" && ! grep -qE 'ACP-MARKER|provider' "$WORK/acp.out") && echo ok || echo fail)
check V6 "an ACP prompt (always in a session) gets only the fixed notice" "$ACP" "$WORK/acp.out"
stop_daemon

echo "== 3. the grant switch"
start_cli "$NEW" "$WORK/up" A24_MODULE_TOOL_GRANTS=1 A24_MODULE_GRANT_MAX_DAYS=90
api POST /api/v1/module-consents/modules/node-ref/tools/echo '{}'
check G3 "A24_MODULE_TOOL_GRANTS=1 opens it (max 90 days accepted)" "$(is "$CODE/$(jq -r .error.code "$WORK/r.json")" 404/not_found)" "$CODE $(jq -c .error.code "$WORK/r.json") — past the switch; node-ref is not installed yet"
stop_daemon
for bad in A24_MODULE_TOOL_GRANTS=yes A24_MODULE_GRANT_MAX_DAYS=91; do
  if start_cli "$NEW" "$WORK/up" "$bad"; then R=fail; stop_daemon; else R=$(has "$WORK/up.start.out" "agent24d did not start: ${bad%%=*}"); fi
  check G4 "agent24 daemon start refuses $bad and names it" "$R" "$(grep -m1 -o "agent24d did not start: .*" "$WORK/up.start.out" | cut -c1-120)"
done

echo "== 4. a protocol-1 module, install and disable"
AGENT24_DATA_DIR="$WORK/up" A24_OS_PACKAGES="$WORK/up.pk" "$NEW/agent24" os install "$REPO/examples/node-module" > "$WORK/g5.out" 2>&1; RC=$?
DATA="$WORK/up"; S=$(sqlite3 "$DATA/agent24.db" "SELECT count(*) FROM module_consent_revocations WHERE module = 'node-ref'")
check G5 "os install revokes earlier grants of the name" "$(is "$RC/$S" 0/1)" "exit $RC; revocation rows $S"
grant node-ref echo "$(iso 1)"; sleep 2
start_cli "$NEW" "$WORK/up"
for _ in $(seq 1 20); do
  cli os list > "$WORK/p1.out" 2>&1; grep -qE 'node-ref +[^ ]+ +\[mounted\]' "$WORK/p1.out" && break; sleep 0.5
done
check P1 "agent24 os list: a module with no protocol declared (1) still mounts" "$(grep -qE 'node-ref +[^ ]+ +\[mounted\]' "$WORK/p1.out" && echo ok || echo fail)" "$(grep -m1 node-ref "$WORK/p1.out" | cut -c1-120)"
cli os disable node-ref > "$WORK/g6.out" 2>&1; RC=$?
S=$(consent_status node-ref echo)
check G6 "agent24 os disable revokes the module's grants" "$(is "$RC/$S" 0/revoked)" "exit $RC; node-ref/echo $S"
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
if start_cli "$NEW" "$WORK/down"; then R=ok; stop_daemon; else R=fail; fi
check D2 "the release still opens it afterwards" "$R" "$(head -1 "$WORK/down.start.out")"

check U5 "agent24 daemon stop stops each release daemon it is asked to" "$( [ "$STOPS" -gt 0 ] && [ -z "$STOP_FAILS" ] && echo ok || echo fail)" "$STOPS stops; failed:${STOP_FAILS:- none}"

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
