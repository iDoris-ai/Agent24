#!/usr/bin/env bash
# test-soak-monitor.sh — behavioural tests for soak-monitor.sh's F5 gate.
#
# It stands up a fake agent24d (health 200 + one live schedule) so both the PASS
# and the NEEDS REVIEW paths are exercised. A gate that has only ever been
# watched rejecting things is half tested: the dangerous direction is the one
# where it says PASS.
#
#   scripts/test-soak-monitor.sh          # run all cases
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
MON="$HERE/soak-monitor.sh"
WORK=$(mktemp -d)
# Processes first, then the directory they are writing into — the other order
# leaves a daemon writing to a path that no longer exists.
cleanup() { stop_fake; rm -rf "$WORK"; }
trap cleanup EXIT

command -v jq >/dev/null || { echo "need jq" >&2; exit 2; }
pass=0; fail=0

# ── fake agent24d ────────────────────────────────────────────────────────────
cat > "$WORK/faked.py" <<'PY'
import json, sys, threading, time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, HTTPServer
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        mode_file = Path(sys.argv[1]).with_name('mode')
        mode = mode_file.read_text().strip() if mode_file.exists() else ''
        if mode == 'unhealthy' or (mode == 'error' and '/schedules' in self.path):
            self.send_response(503); self.end_headers(); return
        if self.path.endswith('/health'):
            body = {"status": "ok"}
        elif '/schedules' in self.path:
            body = {"schedules": [{"id": "s1", "enabled": True,
                                   "next_run_at": "2099-01-01T00:00:00Z",
                                   "consecutive_failures": 0, "last_run_at": None}]}
        else:
            self.send_response(404); self.end_headers(); return
        if '/schedules' in self.path:
            if mode == 'empty': body = {"schedules": []}
            if mode == 'malformed': body = {"schedules": {"error": "bad"}}
            if mode == 'disabled': body['schedules'][0]['enabled'] = False
            if mode == 'overdue': body['schedules'][0]['next_run_at'] = '2000-01-01T00:00:00Z'
        b = json.dumps(body).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(b)))
        self.end_headers(); self.wfile.write(b)
srv = HTTPServer(('127.0.0.1', 0), H)
json.dump({"port": srv.server_address[1], "token": "t", "pid": 4242}, open(sys.argv[1], 'w'))
threading.Thread(target=srv.serve_forever, daemon=True).start()
time.sleep(float(sys.argv[2]))
PY

# Deterministic start/stop. A fixed `sleep 0.6` plus a leftover daemon.json meant
# a slow machine could leave the monitor talking to the PREVIOUS daemon, so a
# case could pass for the wrong reason.
start_fake() {
  stop_fake
  rm -f "$WORK/daemon.json"
  python3 "$WORK/faked.py" "$WORK/daemon.json" "${1:-30}" & FAKE_PID=$!
  local i=0
  while [ ! -s "$WORK/daemon.json" ]; do
    i=$((i+1)); [ "$i" -gt 200 ] && { echo "fake daemon failed to start" >&2; exit 2; }
    sleep 0.05
  done
  # Announced is not the same as answering.
  local port; port=$(jq -r .port "$WORK/daemon.json")
  i=0
  until curl -sf -o /dev/null --max-time 2 "http://127.0.0.1:$port/api/v1/health"; do
    i=$((i+1)); [ "$i" -gt 200 ] && { echo "fake daemon not answering" >&2; exit 2; }
    sleep 0.05
  done
}
stop_fake() {
  if [ -n "${FAKE_PID:-}" ]; then
    kill "$FAKE_PID" 2>/dev/null
    wait "$FAKE_PID" 2>/dev/null
    FAKE_PID=""
  fi
}

# check <name> <expected: PASS|REVIEW|INCOMPLETE> <extra monitor args...>
#
# Assert the exit code alongside the label: automation reads the code.
check() {
  local name="$1" want="$2"; shift 2
  local out rc
  out=$("$MON" --log "$WORK/soak.jsonl" "$@" 2>&1)
  rc=$?
  rm -f "$WORK/soak.jsonl"
  local got="UNKNOWN" want_rc=1
  case "$out" in
    *"RESULT: NEEDS REVIEW"*) got="REVIEW" ;;
    *"RESULT: INCOMPLETE"*) got="INCOMPLETE" ;;
    *"RESULT: PASS"*)       got="PASS" ;;
  esac
  [ "$want" = "PASS" ] && want_rc=0
  if [ "$got" = "$want" ] && [ "$rc" -eq "$want_rc" ]; then
    printf '  ok   %-52s (%s, exit %d)\n' "$name" "$got" "$rc"; pass=$((pass+1))
  else
    printf '  FAIL %-52s want %s/exit %d, got %s/exit %d\n' "$name" "$want" "$want_rc" "$got" "$rc"
    echo "$out" | sed 's/^/       | /'
    fail=$((fail+1))
  fi
}

echo "== daemon / scheduler gates (without a bridge) =="
start_fake 120
check "健康 daemon 和日程，无 bridge → PASS" PASS \
  --interval 1 --duration 2 --daemon-json "$WORK/daemon.json"
check "缺少 daemon 状态 → REVIEW" REVIEW \
  --interval 1 --duration 2 --daemon-json "$WORK/missing.json"

for mode in unhealthy empty disabled overdue error malformed; do
  echo "$mode" > "$WORK/mode"
  check "$mode → REVIEW" REVIEW \
    --interval 1 --duration 2 --daemon-json "$WORK/daemon.json"
done
rm -f "$WORK/mode"

echo "== interrupted run =="
"$MON" --interval 1 --duration 600 --log "$WORK/interrupted.jsonl" \
  --daemon-json "$WORK/daemon.json" > "$WORK/interrupted.out" 2>&1 &
MON_PID=$!
sleep 1
kill -TERM "$MON_PID"
wait "$MON_PID"; rc=$?
if [ "$rc" -eq 1 ] && grep -q 'RESULT: INCOMPLETE' "$WORK/interrupted.out"; then
  pass=$((pass+1))
else
  cat "$WORK/interrupted.out"
  fail=$((fail+1))
fi

echo "== argument validation =="
for bad in "--duration 0" "--interval 0" "--duration abc" "--interval abc"; do
  # shellcheck disable=SC2086
  out=$("$MON" $bad 2>&1); rc=$?
  if [ "$rc" -eq 2 ] && echo "$out" | grep -q 'must be'; then
    pass=$((pass+1))
  else
    echo "FAIL: $bad ($out)"; fail=$((fail+1))
  fi
done

echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]
