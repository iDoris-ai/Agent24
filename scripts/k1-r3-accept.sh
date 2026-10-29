#!/usr/bin/env bash
# K1 third release acceptance on a real machine, driven as a user drives it:
# `agent24`; the daemon's HTTP API only where no product entry exists (starting
# a run with tools, creating a session, approving). Cases and the negative
# control: docs/agent/K1-R3-ACCEPTANCE.md.
#
#   scripts/k1-r3-accept.sh --old <main bin dir> --new <release bin dir> [--work <dir>]
#
# Run it from the release checkout: U2 expects the migrations found here.
# Each bin dir holds `agent24d` and `agent24` (`cargo build -p agent24d -p agent24-cli`).
# Every daemon runs on its own AGENT24_DATA_DIR under --work (Z1 checks the
# user's ~/.agent24, so stop your own daemon first). The model is
# scripts/k1-r3-model-stub.py on loopback; nothing leaves the machine.
# Needs curl, jq, sqlite3, python3 and perl.
set -uo pipefail
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
[ -n "$OLD" ] && [ -n "$NEW" ] || { sed -n 2,12p "$0" >&2; exit 2; }
for b in "$OLD/agent24d" "$OLD/agent24" "$NEW/agent24d" "$NEW/agent24"; do
  [ -x "$b" ] || { echo "missing binary: $b" >&2; exit 2; }
done
for t in curl jq sqlite3 python3 perl; do
  command -v "$t" >/dev/null || { echo "missing tool: $t" >&2; exit 2; }
done
resolve() { python3 -I -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$1"; }
REAL=$(resolve "${HOME:-/nonexistent}/.agent24")
NEW=$(resolve "$NEW"); OLD=$(resolve "$OLD")
printf '%s' "$NEW$OLD" | grep -q '[^ -~]' && { echo "--new and --old must be ASCII paths" >&2; exit 2; }
WORK=$(resolve "${WORK:-${TMPDIR:-/tmp}/k1r3-accept.$$}")
printf '%s' "$WORK" | grep -q '[^!-~]' && { echo "--work must be an ASCII path without spaces: $WORK" >&2; exit 2; }
[ -s "$REAL/daemon.json" ] && kill -0 "$(jq -r .pid "$REAL/daemon.json")" 2>/dev/null \
  && { echo "a daemon is running on $REAL: stop it first (agent24 daemon stop)" >&2; exit 2; }
# inside DIR TOP: DIR is TOP or below it, both absolute. TOP is cut to its
# nearest existing ancestor and the missing rest; an existing ancestor of DIR
# that is that same directory by inode (a firmlink or bind mount names it
# otherwise) is followed by DIR's rest, compared without case (a
# case-insensitive volume takes ~/.Agent24 for ~/.agent24). As K1-R2.
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
# A binary's first run can wait on a system scan for longer than a case allows
for b in "$OLD/agent24d" "$OLD/agent24" "$NEW/agent24d" "$NEW/agent24"; do "$b" --version > /dev/null 2>&1; done
for v in $(env | grep -oE '^(A24|AGENT24|OMLX|OLLAMA)_[A-Z0-9_]+'); do unset "$v"; done
unset DEFAULT_MODEL

FAILS=0
: > "$WORK/results"
touch "$WORK/.start"
[ -e "$REAL" ] && REAL_BEFORE=present || REAL_BEFORE=absent

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
both() { [ "$1" = ok ] && [ "$2" = ok ] && echo ok || echo fail; }

# The model: one stub as the local tier, one counting stub as a Remote tier
# (a v4-mapped loopback URL is labelled Remote yet reachable here).
free_port() { python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'; }
mkdir -p "$WORK/local" "$WORK/remote"
# steps: what the stub model does for each marker, for the library in $DATA
# (fs_read must name a file in that daemon's workspace)
steps() {
  cat > "$WORK/steps.json" <<EOF
{"S-MARKER": [{"call": {"name": "fs_read", "arguments": {"path": "$DATA/workspace/note.txt"}}}, {"say": "S said: {last}"}],
 "L-MARKER": [{"call": {"name": "http_fetch", "arguments": {"url": "https://k1r3.invalid/l"}}}, {"say": "L said: {last}"}],
 "T1-MARKER": [{"call": {"name": "shell_exec", "arguments": {"argv": ["/bin/echo", "T1-SHELL-OUTPUT"]}}},
               {"call": {"name": "http_fetch", "arguments": {"url": "https://k1r3.invalid/t1"}}}, {"say": "T1 said: {all}"}],
 "T2-MARKER": [{"call": {"name": "fs_read", "arguments": {"path": "$DATA/workspace/note.txt"}}},
               {"call": {"name": "http_fetch", "arguments": {"url": "https://k1r3.invalid/t2"}}}, {"say": "T2 said: {all}"}],
 "T3-MARKER": [{"call": {"name": "shell_exec", "arguments": {"argv": ["/bin/echo", "T3-RESUMED"]}}}, {"say": "T3 said: {last}"}],
 "H-MARKER": [{"call": {"name": "shell_exec", "arguments": {"argv": ["/bin/echo", "H-SHELL-OUTPUT"]}}},
              {"call": {"name": "fs_write", "arguments": {"path": "$DATA/workspace/h.txt", "content": "h"}}},
              {"call": {"name": "http_fetch", "arguments": {"url": "https://k1r3.invalid/h"}}}, {"say": "H said: {all}"}],
 "C2-MARKER": [{"call": {"name": "shell_exec", "arguments": {"argv": ["/bin/echo", "C2-NEVER"]}}}, {"say": "C2 said: {last}"}]}
EOF
  mkdir -p "$DATA/workspace"; printf NOTE-CONTENT > "$DATA/workspace/note.txt"
}
echo '{}' > "$WORK/steps.json"
LPORT=$(free_port); RPORT=$(free_port)
python3 -I "$REPO/scripts/k1-r3-model-stub.py" "$LPORT" "$WORK/local" "$WORK/steps.json" > "$WORK/local.log" 2>&1 & LSTUB=$!
python3 -I "$REPO/scripts/k1-r3-model-stub.py" "$RPORT" "$WORK/remote" "$WORK/steps.json" > "$WORK/remote.log" 2>&1 & RSTUB=$!
MODEL_ENV=(OMLX_URL="http://127.0.0.1:$LPORT" OLLAMA_URL="http://[::ffff:127.0.0.1]:$RPORT" DEFAULT_MODEL=k1r3-stub)

PID="" PORT="" TOKEN="" DATA="" BY_CLI=""
STOPS=0 STOP_FAILS=""
# daemons BIN: pids of `BIN/agent24d serve` started since this run began
daemons() {
  ps -axo pid=,command= | C="$1/agent24d serve" awk '{ pid = $1; sub(/^ *[0-9]+ /, ""); if (index($0, ENVIRON["C"]) == 1) print pid }' \
    | grep -vxF -f "$WORK/.before"
}
{ daemons "$NEW"; daemons "$OLD"; } > "$WORK/.before" 2>/dev/null || :
reap() {
  local pid
  for pid in $(daemons "$NEW") $(daemons "$OLD"); do
    echo "a daemon outlived its case (pid $pid); killing it" >&2; kill -KILL "$pid" 2>/dev/null
  done
}
# start_cli BIN_DIR DATA_DIR [VAR=value ...]: `agent24 daemon start`, as a user
# starts it, with the stub models; 0 once started, its words in DATA_DIR.start.out
start_cli() {
  local bin=$1; DATA=$2; shift 2
  mkdir -p "$DATA"; rm -f "$DATA/daemon.json"
  env AGENT24_DATA_DIR="$DATA" A24_OS_PACKAGES="$DATA.pk" "${MODEL_ENV[@]}" "$@" perl -e 'alarm shift; exec @ARGV' 60 \
    "$bin/agent24" daemon start > "$DATA.start.out" 2>&1 || { reap; return 1; }
  PID=$(jq -r .pid "$DATA/daemon.json"); PORT=$(jq -r .port "$DATA/daemon.json")
  TOKEN=$(jq -r .token "$DATA/daemon.json"); BY_CLI=$bin
  [ -n "$PID" ] && [ "$PID" != null ] || { echo "daemon start left no daemon.json in $DATA" >&2; PID="" BY_CLI=""; reap; return 1; }
}
# start_raw BIN_DIR DATA_DIR: agent24d itself, its log kept (D1 reads why it refused)
start_raw() {
  local bin=$1; DATA=$2; shift 2
  mkdir -p "$DATA"; rm -f "$DATA/daemon.json"
  env AGENT24_DATA_DIR="$DATA" A24_OS_PACKAGES="$DATA.pk" "${MODEL_ENV[@]}" "$@" "$bin/agent24d" serve > "$DATA.serve.log" 2>&1 &
  PID=$!; BY_CLI=""
  for _ in $(seq 1 60); do
    if [ -s "$DATA/daemon.json" ]; then
      PORT=$(jq -r .port "$DATA/daemon.json"); TOKEN=$(jq -r .token "$DATA/daemon.json"); return 0
    fi
    kill -0 "$PID" 2>/dev/null || { wait "$PID"; PID=""; return 1; }
    sleep 0.5
  done
  stop_daemon; return 1
}
# stop_daemon: `agent24 daemon stop` (SIGTERM for a raw one), SIGKILL after
# 10 s. A `daemon stop` that failed or needed the SIGKILL counts against U5.
stop_daemon() {
  [ -n "$PID" ] || return 0
  local rc=0
  if [ -n "$BY_CLI" ]; then
    env AGENT24_DATA_DIR="$DATA" perl -e 'alarm shift; exec @ARGV' 30 "$BY_CLI/agent24" daemon stop > "$DATA.stop.out" 2>&1 || rc=$?
  else
    kill -TERM "$PID" 2>/dev/null
  fi
  for _ in $(seq 1 20); do kill -0 "$PID" 2>/dev/null || break; sleep 0.5; done
  if kill -0 "$PID" 2>/dev/null; then echo "daemon $PID did not stop; killing it" >&2; kill -KILL "$PID"; rc=killed; fi
  if [ -n "$BY_CLI" ]; then STOPS=$((STOPS + 1)); [ "$rc" = 0 ] || STOP_FAILS="$STOP_FAILS $DATA:$rc"
  else wait "$PID" 2>/dev/null; fi
  PID="" BY_CLI=""
}
trap 'stop_daemon; reap; kill "$LSTUB" "$RSTUB" 2>/dev/null' EXIT
# cli BIN ARGS...: that build's agent24 on the current library, time-limited
cli() { local bin=$1; shift; env AGENT24_DATA_DIR="$DATA" A24_OS_PACKAGES="$DATA.pk" perl -e 'alarm shift; exec @ARGV' 60 "$bin/agent24" "$@"; }
# api METHOD PATH [BODY] -> body in $WORK/r.json, status in $CODE
api() {
  rm -f "$WORK/r.json"
  CODE=$(curl -s -m 20 -o "$WORK/r.json" -w '%{http_code}' -X "$1" \
    -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
    ${3:+-d "$3"} "http://127.0.0.1:$PORT$2")
}
# status ID: the run's status over HTTP (`run get` prints the same, but this keeps one parser)
status() { api GET "/api/v1/runs/$1"; jq -r .status "$WORK/r.json" 2>/dev/null; }
# wait_status ID WANTED...: until the run reaches one of them (30 s)
wait_status() {
  local id=$1 s; shift
  for _ in $(seq 1 60); do
    s=$(status "$id"); for w in "$@"; do [ "$s" = "$w" ] && return 0; done
    sleep 0.5
  done
  return 1
}
# pending ID: the pending approval of the run, empty when there is none
pending() { api GET /api/v1/approvals; jq -r --arg r "$1" '[.approvals[] | select(.run_id == $r and .status == "pending") | .id][0] // empty' "$WORK/r.json"; }
# approve ID: approve the run's pending approval (no CLI entry exists)
approve() { local a; a=$(pending "$1"); [ -n "$a" ] && api POST "/api/v1/approvals/$a" '{"type":"approve"}'; }
# new_run PROMPT [SESSION]: POST /api/v1/runs, the run id on stdout
new_run() {
  api POST /api/v1/runs "$(jq -nc --arg p "$1" --arg s "${2:-}" '{prompt: $p} + (if $s == "" then {} else {session_id: $s} end)')"
  jq -r .id "$WORK/r.json"
}
output() { api GET "/api/v1/runs/$1"; jq -r '.output.text // (.error.code + ": " + .error.message) // empty' "$WORK/r.json"; }
db() { sqlite3 "$DATA/agent24.db" "$1"; }
iso() { python3 -c "import datetime,sys;print((datetime.datetime.now(datetime.timezone.utc)+datetime.timedelta(seconds=int(sys.argv[1]))).strftime('%Y-%m-%dT%H:%M:%SZ'))" "$1"; }
# consent_status MODULE TOOL: the grant's status as `agent24 consent list` shows it
consent_status() { cli "$NEW" consent list --json 2>/dev/null | jq -r --arg m "$1" --arg t "$2" '.authorizations[] | select(.module==$m and .tool==$t) | .status'; }

# grant MODULE OP AT: a granted row as an earlier build wrote it (no artifact)
grant() {
  db "INSERT OR REPLACE INTO module_consents (module, op, module_version, scope_fingerprint, source, risk, decision, decided_at, expires_at)
      VALUES ('$1', '$2', '0.1.0', 'fp-accept', 'manual_install', 'high', 'granted', '$3', '$(iso 1728000)')"
}
# run_end ID: until the run has ended (30 s); its status
run_end() { wait_status "$1" completed failed cancelled; status "$1"; }
# after_approval ID: approve the run's pending approval, then its end
after_approval() {
  wait_status "$1" awaiting_approval || { status "$1"; return; }
  approve "$1"; run_end "$1"
}
DENIED="denied by policy"
DNS="dns lookup failed"

echo "== 1. an old library, written by main ($OLD)"
start_cli "$OLD" "$WORK/old" || { echo "main daemon did not start: $(cat "$WORK/old.start.out")" >&2; exit 1; }
steps
api POST /api/v1/runs '{"prompt":"OLD-RUN-MARKER"}'; OLD_RUN=$(jq -r .id "$WORK/r.json"); run_end "$OLD_RUN" > /dev/null
# A session that called a tool on main, as the desktop's chat or ACP leaves one
api POST /api/v1/sessions '{"title":"k1r3 accept","channel":"creative"}'; SID=$(jq -r .id "$WORK/r.json")
OLD_SESSION_RUN=$(new_run "S-MARKER on main" "$SID"); OLD_S=$(run_end "$OLD_SESSION_RUN")
# A run that has taken in a shell's output and waits on its next approval
# when main's daemon dies; the upgrade follows before that approval expires
H_RUN=$(new_run "H-MARKER"); wait_status "$H_RUN" awaiting_approval; H_FIRST=$(pending "$H_RUN"); approve "$H_RUN"
H_PARKED=""
for _ in $(seq 1 60); do
  H_PARKED=$(pending "$H_RUN"); [ -n "$H_PARKED" ] && [ "$H_PARKED" != "$H_FIRST" ] && break; H_PARKED=""; sleep 0.5
done
# Dies: a `daemon stop` would abort the waiting approval instead
kill -KILL "$PID"; for _ in $(seq 1 20); do kill -0 "$PID" 2>/dev/null || break; sleep 0.5; done; PID="" BY_CLI=""
grant node-ref echo "$(iso -3600)"
grant documents find "$(iso -3600)"
OLD_VERSION=$(db "SELECT max(version) FROM _sqlx_migrations")
echo "old library: migration $OLD_VERSION, run $OLD_RUN, session $SID (run $OLD_SESSION_RUN $OLD_S)"
[ "$OLD_S" = completed ] || { echo "the session run on main did not complete" >&2; exit 1; }
[ -n "$H_PARKED" ] || { echo "the run $H_RUN did not park on its second approval on main" >&2; exit 1; }
rsync -a --exclude '*.sock' "$WORK/old/" "$WORK/up/"

echo "== 2. upgrade with the release ($NEW)"
if start_cli "$NEW" "$WORK/up"; then UP=ok; else UP=fail; fi
check U1 "agent24 daemon start opens the main library" "$UP" "migration $OLD_VERSION before; $(head -1 "$WORK/up.start.out")"
[ "$UP" = ok ] || { cat "$WORK/up.start.out" >&2; exit 1; }
steps
cli "$NEW" daemon status > "$WORK/u2.out" 2>&1; RC=$?
check U2 "agent24 daemon status after the upgrade" "$( [ $RC = 0 ] && grep -q '^running · ' "$WORK/u2.out" && echo ok || echo fail)" "exit $RC; $(head -1 "$WORK/u2.out")"
# Every migration this checkout carries past main's
# R1 comes first: the approval main left must not expire before it is answered
approve "$H_RUN"; H_END=$(run_end "$H_RUN"); H_CODE=$(jq -r '.error.code // empty' "$WORK/r.json"); H_OUT=$(output "$H_RUN")
check R1 "a run main parked after a shell_exec is not resumed after the upgrade" "$( [ "$H_END/$H_CODE" = failed/source_unproven ] && case "$H_OUT" in (*"$DNS"*) echo fail ;; (*) echo ok ;; esac || echo fail)" "$H_END ${H_CODE:-}; $(echo "$H_OUT" | cut -c1-70)"
versions() { ls "$1" | grep -oE '^[0-9]+' | sed 's/^0*//' | sort -nu; }
WANT=$(versions "$REPO/rust/crates/agent24-store/migrations" | awk -v o="$OLD_VERSION" '$1 > o' | paste -sd, -)
APPLIED=$(db "SELECT group_concat(version) FROM (SELECT version FROM _sqlx_migrations WHERE version > $OLD_VERSION AND success = 1 ORDER BY version)")
check U3 "store migrations past main's are applied (31, 32 and 33)" "$(case ",$WANT," in (*,31,*32,*33,*) is "$APPLIED" "$WANT" ;; (*) echo fail ;; esac)" "want $WANT; applied ${APPLIED:-none}"
cli "$NEW" run list > "$WORK/u4.list" 2>&1; cli "$NEW" run show "$OLD_RUN" > "$WORK/u4.show" 2>&1
check U4 "agent24 run list / run show have main's runs" "$(both "$(has "$WORK/u4.list" "$OLD_RUN")" "$(has "$WORK/u4.show" OLD-RUN-MARKER)")" "$(grep -c '^run_' "$WORK/u4.list") runs listed; $(head -1 "$WORK/u4.show" | cut -c1-80)"

S1=$(consent_status node-ref echo); S2=$(consent_status documents find)
check G1 "agent24 consent list: a grant from before the upgrade is stale (#1317)" "$(is "$S1/$S2" stale/stale)" "node-ref/echo $S1, documents/find $S2"

ORIGIN=$(db "SELECT origin FROM session_tool_result_origins WHERE session_id = '$SID'" 2>&1)
L_RUN=$(new_run "L-MARKER in the old session" "$SID"); run_end "$L_RUN" > /dev/null; L_OUT=$(output "$L_RUN")
check L1 "a session that called tools on main keeps restricting egress (0031)" "$(both "$(is "$ORIGIN" tool:legacy)" "$(case "$L_OUT" in (*"$DENIED"*) echo ok ;; (*) echo fail ;; esac)")" "origin ${ORIGIN:-none}; http_fetch: $(echo "$L_OUT" | cut -c1-70)"
F_RUN=$(new_run "L-MARKER outside any session"); run_end "$F_RUN" > /dev/null; F_OUT=$(output "$F_RUN")
check L2 "a run outside that session is not restricted" "$(case "$F_OUT" in (*"$DNS"*) echo ok ;; (*) echo fail ;; esac)" "http_fetch: $(echo "$F_OUT" | cut -c1-70)"

T1=$(new_run "T1-MARKER"); T1_END=$(after_approval "$T1"); T1_OUT=$(output "$T1")
check T1 "after an approved shell_exec, the run's http_fetch is refused" "$( [ "$T1_END" = completed ] && case "$T1_OUT" in (*"$DENIED"*) echo ok ;; (*) echo fail ;; esac || echo fail)" "$T1_END; $(echo "$T1_OUT" | cut -c1-80)"
T2=$(new_run "T2-MARKER"); T2_END=$(run_end "$T2"); T2_OUT=$(output "$T2")
check T2 "after an fs_read that read the file, the run's http_fetch passes the gate (fails at DNS)" "$( [ "$T2_END" = completed ] && case "$T2_OUT" in ("T2 said: NOTE-CONTENT | "*"$DNS"*) echo ok ;; (*) echo fail ;; esac || echo fail)" "$T2_END; $(echo "$T2_OUT" | cut -c1-80)"
cli "$NEW" run peers --json > "$WORK/p1.json" 2>&1
P=$(jq -r --arg a "$T2" --arg b "$T1" '[(.runs[] | select(.id == $a) | .withheld), (.runs[] | select(.id == $b) | .withheld)] | map(tostring) | join("/")' "$WORK/p1.json" 2>/dev/null)
# The shared one with its own prompt and output, the withheld one with nothing of its own
SHARED=$(jq -e --arg a "$T2" --arg b "$T1" '(.runs[] | select(.id == $a) | .prompt == "T2-MARKER" and (.output.text | contains("NOTE-CONTENT")))
  and (.runs[] | select(.id == $b) | .prompt == null and .output == null)' "$WORK/p1.json" > /dev/null 2>&1 && echo ok || echo fail)
check P1 "agent24 run peers: the fs_read run is shared, the shell_exec one withheld" "$(both "$(is "$P/$SHARED" false/true/ok)" "$(lacks "$WORK/p1.json" T1-SHELL-OUTPUT)")" "T2/T1 withheld: ${P:-?}; contents as expected: $SHARED"

cli "$NEW" schedule create --name k1r3 --prompt C1-PROMPT --every 3600 > "$WORK/c1.create" 2>&1; R1=$?
SCH=$(sed -n '1s/^\(sch_[A-Za-z0-9]*\) .*/\1/p' "$WORK/c1.create")
cli "$NEW" schedule update "$SCH" --prompt C1-CHANGED > "$WORK/c1.update" 2>&1; R2=$?
cli "$NEW" schedules run-now "$SCH" > "$WORK/c1.now" 2>&1; R3=$?; C1_RUN=$(sed -n 's/^started //p' "$WORK/c1.now")
cli "$NEW" schedule delete "$SCH" > "$WORK/c1.delete" 2>&1; R4=$?
cli "$NEW" schedule list > "$WORK/c1.list" 2>&1; R5=$?
# The run run-now started: of this schedule, with the changed prompt, and it ended
C1_END=$( [ -n "$C1_RUN" ] && run_end "$C1_RUN")
cli "$NEW" run show "$C1_RUN" --json > "$WORK/c1.run" 2>&1
FIRED=$(jq -e --arg s "$SCH" '.schedule_id == $s and .input.prompt == "C1-CHANGED"' "$WORK/c1.run" > /dev/null 2>&1 && echo ok || echo fail)
C1=$( [ "$R1$R2$R3$R4$R5" = 00000 ] && [ -n "$SCH" ] && [ "$C1_END" = completed ] && echo ok || echo fail)
C1=$(both "$C1" "$(both "$FIRED" "$(both "$(has "$WORK/c1.update" C1-CHANGED)" "$(lacks "$WORK/c1.list" "$SCH")")")")
check C1 "agent24 schedule create / update / run-now / delete" "$C1" "exits $R1$R2$R3$R4$R5; $SCH; run ${C1_RUN:-none} ${C1_END:-} of it with the new prompt: $FIRED"

C2=$(new_run "C2-MARKER"); wait_status "$C2" awaiting_approval
cli "$NEW" run cancel "$C2" > "$WORK/c2.out" 2>&1; RC=$?
check C2 "agent24 run cancel stops a run awaiting approval" "$( [ $RC = 0 ] && is "$(run_end "$C2")" cancelled || echo fail)" "exit $RC; $(head -1 "$WORK/c2.out" | cut -c1-80)"

cli "$NEW" chat "记住我喜欢绿茶" --session chat_k1r3 > "$WORK/c3.out" 2>&1; RC=$?
cli "$NEW" memory list --json > "$WORK/c3.mem" 2>&1
check C3 "agent24 chat --session keeps an explicit remember" "$( [ $RC = 0 ] && both "$(has "$WORK/c3.out" '· remembered')" "$(has "$WORK/c3.mem" 绿茶)" || echo fail)" "exit $RC; $(tail -1 "$WORK/c3.out")"

T3=$(new_run "T3-MARKER"); wait_status "$T3" awaiting_approval
kill -KILL "$PID"; for _ in $(seq 1 20); do kill -0 "$PID" 2>/dev/null || break; sleep 0.5; done; PID="" BY_CLI=""
start_cli "$NEW" "$WORK/up"; T3_END=$(after_approval "$T3"); T3_OUT=$(output "$T3")
check T3 "a run awaiting approval when the daemon died resumes once approved" "$( [ "$T3_END" = completed ] && case "$T3_OUT" in (*T3-RESUMED*) echo ok ;; (*) echo fail ;; esac || echo fail)" "$T3_END; $(echo "$T3_OUT" | cut -c1-80)"
stop_daemon

if start_cli "$NEW" "$WORK/up" A24_MODULE_TOOL_GRANTS=yes; then R=fail; stop_daemon; else R=$(has "$WORK/up.start.out" 'agent24d did not start: A24_MODULE_TOOL_GRANTS'); fi
check C4 "agent24 daemon start says why the daemon refused to start" "$R" "$(head -1 "$WORK/up.start.out" | cut -c1-100)"

N=$(ls "$WORK/remote" | wc -l | tr -d ' ')
check Z2 "no shell_exec output ever reached the remote tier" "$( [ -z "$(grep -rlE 'T1-SHELL-OUTPUT|H-SHELL-OUTPUT' "$WORK/remote")" ] && echo ok || echo fail)" "$N request(s) to the remote stub, $(ls "$WORK/local" | wc -l | tr -d ' ') to the local one"

echo "== 3. no way back"
dump() { sqlite3 "$WORK/down/agent24.db" .dump | shasum; }
DATA="$WORK/up"
rsync -a --delete --exclude '*.sock' --exclude daemon.json "$WORK/up/" "$WORK/down/"
BEFORE=$(dump)
if start_raw "$OLD" "$WORK/down"; then R=fail; stop_daemon; else R=$(has "$WORK/down.serve.log" "previously applied but is missing"); fi
SAME=$( [ "$(dump)" = "$BEFORE" ] && echo yes || echo no); [ "$SAME" = yes ] || R=fail
check D1 "main refuses the upgraded library and leaves it alone" "$R" "$(grep -m1 -o 'migration [0-9]* was previously applied[^"]*' "$WORK/down.serve.log" | cut -c1-90); dump unchanged: $SAME"
if start_cli "$NEW" "$WORK/down"; then R=ok; stop_daemon; else R=fail; fi
check D2 "the release still opens it afterwards" "$R" "$(head -1 "$WORK/down.start.out")"

check U5 "agent24 daemon stop stops each daemon it is asked to" "$( [ "$STOPS" -gt 0 ] && [ -z "$STOP_FAILS" ] && echo ok || echo fail)" "$STOPS stops; failed:${STOP_FAILS:- none}"
if [ "$REAL_BEFORE" = absent ]; then [ -e "$REAL" ] && R=fail || R=ok
else [ -z "$(find "$REAL" -newer "$WORK/.start" -print -quit)" ] && R=ok || R=fail; fi
check Z1 "the user's ~/.agent24 was not touched" "$R" "$REAL $REAL_BEFORE before"

REPORT="$WORK/report.md"
{
  echo "# K1 r3 acceptance — $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo
  echo "- old: \`$("$OLD/agent24d" --version)\` ($OLD, sha256 $(shasum -a 256 "$OLD/agent24d" | cut -c1-12))"
  echo "- new: \`$("$NEW/agent24d" --version)\` ($NEW, sha256 $(shasum -a 256 "$NEW/agent24d" | cut -c1-12))"
  echo "- checkout: $(git -C "$REPO" rev-parse --short HEAD 2>/dev/null)"
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
