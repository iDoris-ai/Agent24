#!/usr/bin/env bash
# test-soak-monitor.sh — behavioural tests for soak-monitor.sh's F5 gate.
#
# WHY THIS EXISTS: every defect found in the monitor during FU-32 review was
# found by RUNNING it, never by reading it — a jq program missing a pipe (silently
# rejecting every snapshot, with the error swallowed by 2>/dev/null), a
# `$(printf '\n')` pattern that collapses to the empty string and therefore
# matches everything, a rollback check written after the assignment it compares
# against. `bash -n` is green for all three. So the running is committed.
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
trap 'rm -rf "$WORK"; [ -n "${FAKE_PID:-}" ] && kill "$FAKE_PID" 2>/dev/null' EXIT

command -v jq >/dev/null || { echo "need jq" >&2; exit 2; }
pass=0; fail=0

# ── fake agent24d ────────────────────────────────────────────────────────────
cat > "$WORK/faked.py" <<'PY'
import json, sys, threading, time
from http.server import BaseHTTPRequestHandler, HTTPServer
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        if self.path.endswith('/health'):
            body = {"status": "ok"}
        elif '/schedules' in self.path:
            body = {"schedules": [{"id": "s1", "enabled": True,
                                   "next_run_at": "2099-01-01T00:00:00Z",
                                   "consecutive_failures": 0, "last_run_at": None}]}
        else:
            self.send_response(404); self.end_headers(); return
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

start_fake() { python3 "$WORK/faked.py" "$WORK/daemon.json" "${1:-30}" & FAKE_PID=$!; sleep 0.6; }

# snap <age_secs> <state> <transitions> <generation> <confirmed>
# Written the way the bridge writes it: temp file + rename, so a sampler can
# never catch a half-written file (a `>` redirect here produced phantom read
# errors and sent me chasing a bug that was in the fixture).
snap() {
  python3 - "$WORK/health.json" "$@" <<'PY'
import datetime, json, os, sys
dest, age, state, tr, gen, conf = sys.argv[1], int(sys.argv[2]), sys.argv[3], int(sys.argv[4]), sys.argv[5], int(sys.argv[6])
t = (datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(seconds=age)).strftime('%Y-%m-%dT%H:%M:%S.000Z')
json.dump({"state": state, "updated_at": t, "degraded_transitions": tr,
           "generation": gen, "last_error": None,
           "canaries": {"sent": conf, "confirmed": conf, "lost": 0, "outstanding": 0},
           # The real bridge always stamps this; the monitor requires it so that
           # another identity's snapshot cannot vouch for this one.
           "context": {"identity": "agent24"}},
          open(dest + ".tmp", "w"))
os.rename(dest + ".tmp", dest)
PY
}

# check <name> <expected: PASS|SMOKE|REVIEW|INCOMPLETE> <extra monitor args...>
#
# The EXIT CODE is asserted alongside the label, not just printed: "a smoke run
# must not exit 0" is the whole point of that state, and automation reads the
# code, not the text.
check() {
  local name="$1" want="$2"; shift 2
  local out rc
  out=$("$MON" --log "$WORK/soak.jsonl" --nostr-health "$WORK/health.json" "$@" 2>&1)
  rc=$?
  rm -f "$WORK/soak.jsonl"
  local got="REVIEW" want_rc=1
  case "$out" in
    *"RESULT: SMOKE PASS"*) got="SMOKE" ;;
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

echo "== 判据 6 必须能通过(最危险的方向:它会不会永远给不出 PASS) =="
start_fake 20
snap 1 ok 0 gen-A 100
( for i in 101 102 103 104 105; do sleep 1; snap 1 ok 0 gen-A "$i"; done ) &
check "健康 + confirmed 在涨 → PASS" PASS \
  --interval 1 --duration 5 --nostr-stale 2 --daemon-json "$WORK/daemon.json"
wait $! 2>/dev/null

echo
echo "== 短 run 不能冒充 F5 结论 =="
start_fake 10
snap 1 ok 0 gen-A 100
check "run < stale → SMOKE PASS(非 0 退出)" SMOKE \
  --interval 1 --duration 3 --nostr-stale 60 --daemon-json "$WORK/daemon.json"

echo
echo "== 各种「证据不可信」都必须判失败 =="
start_fake 40
snap 5000 ok 0 gen-A 100
check "快照冻结(state 还是 ok,但很久没更新)" REVIEW \
  --interval 1 --duration 3 --nostr-stale 900 --daemon-json "$WORK/daemon.json"

snap 1 degraded 1 gen-A 100
check "采样时正处于 degraded" REVIEW \
  --interval 1 --duration 3 --nostr-stale 2 --daemon-json "$WORK/daemon.json"

snap -600 ok 0 gen-A 100
check "updated_at 在未来(时钟被改过)" REVIEW \
  --interval 1 --duration 3 --nostr-stale 2 --daemon-json "$WORK/daemon.json"

printf '{"state":"ok","updated_at":"2099-01-01T00:00:00Z"}\n{"state":"ok"}\n' > "$WORK/health.json"
check "文件里两个顶层 JSON 文档" REVIEW \
  --interval 1 --duration 3 --nostr-stale 2 --daemon-json "$WORK/daemon.json"

printf 'not json at all{' > "$WORK/health.json"
check "快照损坏" REVIEW \
  --interval 1 --duration 3 --nostr-stale 2 --daemon-json "$WORK/daemon.json"

# Isolated on purpose: confirmed GROWS (100→101) and every snapshot is fresh, so
# neither the growth gate nor the staleness gate can fire. Only the generation
# change is left to catch it — an earlier version of this case was quietly being
# failed by the staleness gate instead, and deleting the generation check left it
# green.
start_fake 25
snap 0 ok 0 gen-A 100
( sleep 2; snap 0 ok 0 gen-B 101 ) &
check "generation 变了(账本被重置再追平)" REVIEW \
  --interval 1 --duration 5 --nostr-stale 30 --daemon-json "$WORK/daemon.json"
wait $! 2>/dev/null

# Dips and then ends HIGHER than it started (100 → 3 → 150): the growth gate is
# satisfied, so only the per-sample rollback check can catch the dip. Ending low
# would have been failed by the growth gate regardless, which is exactly how a
# broken rollback check stayed hidden.
start_fake 25
snap 0 ok 0 gen-A 100
( sleep 2; snap 0 ok 0 gen-A 3; sleep 2; snap 0 ok 0 gen-A 150 ) &
check "confirmed 中途回退(但首尾仍在涨)" REVIEW \
  --interval 1 --duration 6 --nostr-stale 30 --daemon-json "$WORK/daemon.json"
wait $! 2>/dev/null

start_fake 25
snap 0 ok 0 gen-A 100
( sleep 2; snap 0 ok 5 gen-A 101 ) &
check "degraded_transitions 在本轮内增长" REVIEW \
  --interval 1 --duration 5 --nostr-stale 30 --daemon-json "$WORK/daemon.json"
wait $! 2>/dev/null

echo
echo "== 「没有证据」不能被当成「没配置桥」 =="
start_fake 20
rm -f "$WORK/health.json"
check "整轮没有快照(默认要求 Nostr)" REVIEW \
  --interval 1 --duration 4 --nostr-stale 2 --daemon-json "$WORK/daemon.json"

start_fake 20
rm -f "$WORK/health.json"
check "没有快照 + 显式 --no-nostr → PASS" PASS \
  --interval 1 --duration 3 --nostr-stale 2 --no-nostr --daemon-json "$WORK/daemon.json"

start_fake 20
rm -f "$WORK/health.json"
( sleep 4; snap 1 ok 0 gen-A 100 ) &
check "快照迟到(此前那一段没有证据)" REVIEW \
  --interval 1 --duration 6 --nostr-stale 2 --daemon-json "$WORK/daemon.json"
wait $! 2>/dev/null

# The escape window: the LAST missing-file sample falls just before the
# threshold, and the snapshot appears after it but before the next sample — so
# the missing-file branch never runs again and would never flag the lateness.
# Only checking it on the first SUCCESSFUL read closes that gap. The case above
# does not discriminate: there the file is still missing when the threshold
# passes, so the missing branch catches it either way.
# Everything AFTER the late arrival is healthy — the snapshot is fresh and
# confirmations keep climbing — so no other gate can fail this run. Only the
# lateness itself is left. (An earlier version had a flat `confirmed`, and the
# growth gate failed the case regardless, hiding whether lateness was detected.)
start_fake 25
rm -f "$WORK/health.json"
( sleep 3.5; snap 0 ok 0 gen-A 100
  sleep 1; snap 0 ok 0 gen-A 101
  sleep 1; snap 0 ok 0 gen-A 102
  sleep 1; snap 0 ok 0 gen-A 103 ) &
check "快照恰好在门槛后、下次采样前才出现" REVIEW \
  --interval 1 --duration 8 --nostr-stale 3 --daemon-json "$WORK/daemon.json"
wait $! 2>/dev/null

# `--nostr-stale 30` so the frozen-snapshot gate cannot fire, and the snapshot
# keeps confirming until it vanishes so the growth gate cannot either. Only the
# disappearance is left to catch it.
start_fake 25
snap 0 ok 0 gen-A 100
( sleep 1; snap 0 ok 0 gen-A 101; sleep 1; snap 0 ok 0 gen-A 102
  sleep 1; rm -f "$WORK/health.json" ) &
check "快照出现过又消失" REVIEW \
  --interval 1 --duration 6 --nostr-stale 30 --daemon-json "$WORK/daemon.json"
wait $! 2>/dev/null

echo
echo "== 身份绑定:别人的快照不能替本身份背书 =="
start_fake 20
snap 0 ok 0 gen-A 100
check "快照属于另一个身份" REVIEW \
  --interval 1 --duration 3 --nostr-stale 30 --nostr-identity someone-else \
  --daemon-json "$WORK/daemon.json"

echo
echo "== 被中断的 run 不是 F5 结论 =="
start_fake 20
snap 0 ok 0 gen-A 100
(
  # Kill it well before its duration, the way Ctrl-C or `kill` on a nohup'd run
  # would. Everything it sampled looks healthy — which is exactly the trap.
  sleep 3
  pkill -TERM -f "soak-monitor.sh --log $WORK/soak.jsonl" 2>/dev/null
) &
check "跑到一半被 SIGTERM" INCOMPLETE \
  --interval 1 --duration 600 --nostr-stale 30 --daemon-json "$WORK/daemon.json"
wait $! 2>/dev/null

echo
echo "== 新旗标必须对它们自己的用途有效 =="
# The default health path has to follow --nostr-identity. It used to be computed
# before argument parsing, so `--nostr-identity alice` still read agent24's file
# and the flag was broken for its only purpose. HOME is redirected so the default
# path resolves inside the sandbox.
start_fake 20
mkdir -p "$WORK/home/.agent24"
python3 - "$WORK/home/.agent24/nostr-bridge-health-alice.json" <<'SNAP'
import datetime, json, sys
t = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%S.000Z')
json.dump({"state": "ok", "updated_at": t, "degraded_transitions": 0, "generation": "gen-A",
           "last_error": None,
           "canaries": {"sent": 5, "confirmed": 5, "lost": 0, "outstanding": 0},
           "context": {"identity": "alice"}}, open(sys.argv[1], "w"))
SNAP
out=$(HOME="$WORK/home" "$MON" --interval 1 --duration 3 --nostr-stale 30 \
  --nostr-identity alice --log "$WORK/soak.jsonl" --daemon-json "$WORK/daemon.json" 2>&1)
rm -f "$WORK/soak.jsonl"
case "$out" in
  *"NO HEALTH SNAPSHOT"*)
    printf '  FAIL %-52s (默认路径没跟着 identity 走)\n' "--nostr-identity 决定默认路径"; fail=$((fail+1)) ;;
  *)
    printf '  ok   %-52s\n' "--nostr-identity 决定默认路径"; pass=$((pass+1)) ;;
esac

# --no-nostr has to actually skip. A stale/degraded snapshot left over from an
# earlier run used to be read anyway, dragging the whole gate in and failing the
# very runs the flag exists to let through.
start_fake 20
snap 5000 degraded 3 gen-A 100
check "--no-nostr 时遗留的坏快照不该拖累判定" PASS \
  --interval 1 --duration 3 --nostr-stale 2 --no-nostr --daemon-json "$WORK/daemon.json"

echo
echo "== 参数校验 =="
for bad in "--duration 0" "--interval 0" "--nostr-stale 0"; do
  # shellcheck disable=SC2086
  if "$MON" $bad --daemon-json "$WORK/daemon.json" 2>&1 | grep -q "must be > 0"; then
    printf '  ok   %-52s\n' "$bad 被拒"; pass=$((pass+1))
  else
    printf '  FAIL %-52s\n' "$bad 被拒"; fail=$((fail+1))
  fi
done

snap 1 ok 0 gen-A 100
if "$MON" --nostr-stale abc --daemon-json "$WORK/daemon.json" 2>&1 | grep -q "must be integer"; then
  printf '  ok   %-52s\n' "--nostr-stale 非整数被拒"; pass=$((pass+1))
else
  printf '  FAIL %-52s\n' "--nostr-stale 非整数被拒"; fail=$((fail+1))
fi

echo
echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]
