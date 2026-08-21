#!/usr/bin/env bash
# soak-monitor.sh — F5 7×24 soak observer for agent24d.
#
# Samples the running daemon on a fixed interval and appends one JSONL heartbeat
# per sample, so a 7-day unattended run leaves an auditable trace. It observes;
# it never touches the daemon's state (only GET /health + GET /schedules).
#
# What each sample records:
#   - health: GET /api/v1/health == 200 with {"status":"ok"} (no token needed)
#   - pid / restarts: daemon.json pid vs. previous sample (a change == a restart,
#     which F2 crash-recovery is supposed to make invisible to the user)
#   - rss_mb / cpu_pct: agent24d process footprint (watch for a slow leak)
#   - schedules / overdue: GET /api/v1/schedules (bearer), and how many have a
#     next_run_at already in the past (a stuck scheduler)
#
# On exit (Ctrl-C, SIGTERM, or --duration elapsed) it prints a PASS/FAIL summary
# against the F5 criteria.
#
# Usage:
#   scripts/soak-monitor.sh                 # 7 days, sample every 5 min
#   scripts/soak-monitor.sh --interval 60 --duration 3600   # 1h smoke, 1 min
#   scripts/soak-monitor.sh --log ~/agent24-soak.jsonl
#
# Env: A24_DAEMON_JSON (default ~/.agent24/daemon.json). Requires curl + jq.
set -u

INTERVAL=300            # seconds between samples
DURATION=$((7*24*3600)) # total run length (7 days)
DAEMON_JSON="${A24_DAEMON_JSON:-$HOME/.agent24/daemon.json}"
LOG=""

while [ $# -gt 0 ]; do
  case "$1" in
    --interval) INTERVAL="$2"; shift 2 ;;
    --duration) DURATION="$2"; shift 2 ;;
    --daemon-json) DAEMON_JSON="$2"; shift 2 ;;
    --log) LOG="$2"; shift 2 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

command -v jq   >/dev/null || { echo "need jq"   >&2; exit 2; }
command -v curl >/dev/null || { echo "need curl" >&2; exit 2; }
[ -n "$LOG" ] || LOG="$HOME/agent24-soak-$(date +%Y%m%d-%H%M%S).jsonl"

# Counters (whole-run accounting).
samples=0; health_ok=0; restarts=0; overdue_max=0; last_pid=""
schedule_min=""; anomalies=0

read_daemon() { # -> echoes "port token pid" or nothing
  [ -f "$DAEMON_JSON" ] || return 1
  jq -r '"\(.port) \(.token) \(.pid)"' "$DAEMON_JSON" 2>/dev/null
}

# ISO-8601 UTC without relying on GNU date flags.
now_iso() { date -u +%Y-%m-%dT%H:%M:%SZ; }

summarize() {
  local up="n/a"
  if [ "$samples" -gt 0 ]; then
    up=$(awk "BEGIN{printf \"%.2f\", 100*$health_ok/$samples}")
  fi
  echo
  echo "==== F5 soak summary ===="
  echo "log:            $LOG"
  echo "samples:        $samples (every ${INTERVAL}s)"
  echo "health uptime:  ${up}%  ($health_ok/$samples ok)"
  echo "daemon restarts: $restarts   (F2 should keep the user unaffected)"
  echo "schedules seen:  min=${schedule_min:-n/a}  max_overdue=${overdue_max}"
  echo "anomalies:       $anomalies"
  echo
  # F5 pass gate: 100% health, scheduler never wedged. Restarts are allowed
  # (F2's job) as long as health recovered — that is exactly what we're proving.
  if [ "$samples" -gt 0 ] && [ "$health_ok" -eq "$samples" ] && [ "$overdue_max" -eq 0 ]; then
    echo "RESULT: PASS (health 100%, no stuck schedules)"
  else
    echo "RESULT: NEEDS REVIEW (see anomalies in the log)"
  fi
  exit 0
}
trap summarize INT TERM

echo "soak-monitor → $LOG"
echo "interval=${INTERVAL}s duration=${DURATION}s daemon=$DAEMON_JSON"

start=$(date +%s)
while :; do
  ts=$(now_iso)
  info=$(read_daemon || true)
  if [ -z "$info" ]; then
    # No discovery file → daemon not registered. Record and keep watching;
    # F1a autostart / F2 recovery may bring it back.
    echo "{\"at\":\"$ts\",\"health\":false,\"reason\":\"no daemon.json\"}" >> "$LOG"
    samples=$((samples+1)); anomalies=$((anomalies+1))
  else
    port=$(echo "$info" | cut -d' ' -f1)
    token=$(echo "$info" | cut -d' ' -f2)
    pid=$(echo "$info" | cut -d' ' -f3)
    base="http://127.0.0.1:${port}"

    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "$base/api/v1/health" 2>/dev/null || echo 000)
    ok=false; [ "$code" = "200" ] && ok=true

    # Restart detection: a changed pid between samples == the daemon was replaced.
    if [ -n "$last_pid" ] && [ "$pid" != "$last_pid" ]; then
      restarts=$((restarts+1))
    fi
    last_pid="$pid"

    # Footprint (best-effort; ps flags are portable enough on macOS/Linux).
    rss_mb=$(ps -o rss= -p "$pid" 2>/dev/null | awk '{printf "%.1f", $1/1024}')
    cpu_pct=$(ps -o %cpu= -p "$pid" 2>/dev/null | tr -d ' ')

    # Schedules (token-gated). Count total + how many are overdue.
    sched=$(curl -s --max-time 10 -H "Authorization: Bearer $token" "$base/api/v1/schedules" 2>/dev/null || echo '')
    n_sched=$(echo "$sched" | jq -r '(.schedules // .) | length' 2>/dev/null || echo "null")
    overdue=$(echo "$sched" | jq -r --arg now "$ts" '[(.schedules // .)[]? | select((.next_run_at // "") != "" and .next_run_at < $now)] | length' 2>/dev/null || echo 0)
    [ "$overdue" = "null" ] && overdue=0

    samples=$((samples+1))
    $ok && health_ok=$((health_ok+1)) || anomalies=$((anomalies+1))
    [ "$overdue" -gt "$overdue_max" ] 2>/dev/null && overdue_max="$overdue"
    if [ "$overdue" -gt 0 ] 2>/dev/null; then anomalies=$((anomalies+1)); fi
    if [ "$n_sched" != "null" ] && { [ -z "$schedule_min" ] || [ "$n_sched" -lt "$schedule_min" ] 2>/dev/null; }; then
      schedule_min="$n_sched"
    fi

    printf '{"at":"%s","health":%s,"code":"%s","pid":%s,"restarts":%s,"rss_mb":%s,"cpu_pct":%s,"schedules":%s,"overdue":%s}\n' \
      "$ts" "$ok" "$code" "${pid:-0}" "$restarts" "${rss_mb:-0}" "${cpu_pct:-0}" "${n_sched:-0}" "${overdue:-0}" >> "$LOG"
  fi

  # Stop after the configured duration.
  [ $(( $(date +%s) - start )) -ge "$DURATION" ] && summarize

  # Portable sleep (whole seconds).
  sleep "$INTERVAL"
done
