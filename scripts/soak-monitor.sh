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
# --interval/--duration must be integer seconds, else a typo (`--duration abc`)
# would arithmetic-compare to 0 and the run would never stop.
case "$INTERVAL" in ''|*[!0-9]*) echo "--interval must be integer seconds" >&2; exit 2 ;; esac
case "$DURATION" in ''|*[!0-9]*) echo "--duration must be integer seconds" >&2; exit 2 ;; esac
[ -n "$LOG" ] || LOG="$HOME/agent24-soak-$(date +%Y%m%d-%H%M%S).jsonl"
# Fail loudly NOW if the log is unwritable — never run a 7-day soak whose PASS
# rests on a log that was never written (soak review #109 Low).
( : >> "$LOG" ) 2>/dev/null || { echo "log not writable: $LOG" >&2; exit 2; }

# Counters (whole-run accounting).
samples=0; health_ok=0; restarts=0; overdue_max=0; last_pid=""
schedule_min=""; anomalies=0; disabled_seen=0; sched_err_seen=0; stuck_failing_seen=0

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
  echo "scheduler faults: auto_disabled=${disabled_seen}  fetch_errors=${sched_err_seen}  stuck_failing=${stuck_failing_seen}"
  echo "anomalies:       $anomalies"
  echo
  # F5 pass gate. The scheduler must be positively ALIVE the whole run — not
  # merely "a /schedules row still exists" (soak review #109):
  #   - health 100%;
  #   - a POSITIVE requirement: at least one schedule present at EVERY sample
  #     (schedule_min > 0). A soak where nobody created a schedule proves nothing
  #     about the scheduler and must NOT pass (B1a). schedule_min stays unset (→
  #     0 via `:-0`) if any sample failed to produce a valid count, which
  #     correctly fails the gate;
  #   - no overdue fire, no auto-disabled schedule (enabled=false,next_run=null
  #     after 5 failures — the row lingers), no failed/miscounted /schedules read.
  # `stuck_failing` (a schedule at consecutive_failures>0) is recorded but does
  # NOT gate — a flapping schedule can legitimately carry it; the JSONL
  # last_run/max_consec_fail fields are the post-hoc "did it actually fire" check.
  # NEEDS REVIEW exits NON-ZERO so TASKS.md automation can gate on it (B3).
  local result=1
  if [ "$samples" -gt 0 ] && [ "$health_ok" -eq "$samples" ] \
     && [ "${schedule_min:-0}" -gt 0 ] && [ "$overdue_max" -eq 0 ] \
     && [ "$disabled_seen" -eq 0 ] && [ "$sched_err_seen" -eq 0 ]; then
    echo "RESULT: PASS (health 100%, ≥1 live schedule every sample, no overdue/auto-disabled/fetch-error)"
    result=0
  else
    echo "RESULT: NEEDS REVIEW (see anomalies + scheduler faults in the log)"
  fi
  exit "$result"
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

    # `-w '%{http_code}'` prints 000 on a timeout/connection failure, so no
    # `|| echo 000` (which would concatenate a SECOND 000 → "000000").
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 "$base/api/v1/health" 2>/dev/null)
    code=${code:-000}
    ok=false; [ "$code" = "200" ] && ok=true

    # Restart detection: a changed pid between samples == the daemon was replaced.
    if [ -n "$last_pid" ] && [ "$pid" != "$last_pid" ]; then
      restarts=$((restarts+1))
    fi
    last_pid="$pid"

    # Footprint (best-effort; ps flags are portable enough on macOS/Linux).
    rss_mb=$(ps -o rss= -p "$pid" 2>/dev/null | awk '{printf "%.1f", $1/1024}')
    cpu_pct=$(ps -o %cpu= -p "$pid" 2>/dev/null | tr -d ' ')

    # Schedules (token-gated). A live scheduler keeps every enabled schedule with
    # a FUTURE next_run_at. Two soak-review (#109) failure modes to catch:
    #   (1) 401/500 returns an ERROR ENVELOPE, not a list — the old
    #       `(.schedules // .)|length` fell back to the envelope and counted its
    #       KEYS (=1), fabricating a healthy-looking count. So capture the HTTP
    #       code and only count a real array; any other shape is `sched_err`.
    #   (2) after 5 consecutive failures the daemon sets enabled=false +
    #       next_run_at=null but leaves the row in the list — a DEAD scheduler
    #       reading healthy. So count auto-disabled schedules explicitly.
    sresp=$(curl -s -w '\n%{http_code}' --max-time 10 -H "Authorization: Bearer $token" "$base/api/v1/schedules" 2>/dev/null)
    scode=$(printf '%s' "$sresp" | tail -n1)
    sbody=$(printf '%s' "$sresp" | sed '$d')
    n_sched="null"; disabled=0; nullnext=0; overdue=0; sched_err=0; max_cfail=0; last_run='"null"'
    if [ "$scode" = "200" ]; then
      # Normalize to the schedule array ONLY when `.schedules` is genuinely an
      # ARRAY (or the body itself is an array). An object whose `.schedules` is
      # an error struct must NOT be walked into and key-counted (soak review #109
      # B1b: `has("schedules")` alone let `{"schedules":{"error":…}}` count keys).
      # Anything else is `sched_err`, never a fabricated count.
      arr=$(printf '%s' "$sbody" | jq -c 'if type=="object" and (.schedules|type)=="array" then .schedules elif type=="array" then . else "ERR" end' 2>/dev/null || echo '"ERR"')
      if [ "$arr" = '"ERR"' ] || [ -z "$arr" ]; then
        sched_err=1
      else
        n_sched=$(printf '%s' "$arr" | jq 'length' 2>/dev/null || echo null)
        disabled=$(printf '%s' "$arr" | jq '[.[]|select(.enabled==false)]|length' 2>/dev/null || echo 0)
        # next_run_at strictly in the past on a still-enabled schedule = a wedged
        # fire (a legitimately-fired one-shot clears next_run_at but is not < now).
        overdue=$(printf '%s' "$arr" | jq --arg now "$ts" '[.[]|select(.enabled!=false and .next_run_at!=null and .next_run_at<$now)]|length' 2>/dev/null || echo 0)
        nullnext=$(printf '%s' "$arr" | jq '[.[]|select(.enabled!=false and .next_run_at==null)]|length' 2>/dev/null || echo 0)
        # Positive-liveness signals RECORDED (not gated): a schedule can sit at
        # consecutive_failures>0 forever (a single success resets it,
        # transitions.rs:113) while staying enabled + future — "responds but does
        # no work" (soak review #109). last_run_at should advance across samples;
        # max_consec_fail flags a failing schedule. Grep these in the JSONL.
        max_cfail=$(printf '%s' "$arr" | jq '[.[].consecutive_failures // 0]|max // 0' 2>/dev/null || echo 0)
        last_run=$(printf '%s' "$arr" | jq -c '[.[].last_run_at // empty]|max // "null"' 2>/dev/null || echo '"null"')
      fi
    else
      sched_err=1   # non-200 = scheduler surface unhealthy; do NOT fabricate a count
    fi

    samples=$((samples+1))
    $ok && health_ok=$((health_ok+1)) || anomalies=$((anomalies+1))
    [ "$overdue" -gt "$overdue_max" ] 2>/dev/null && overdue_max="$overdue"
    # Hard scheduler faults → the run cannot PASS.
    if [ "$sched_err" -eq 1 ]; then sched_err_seen=1; anomalies=$((anomalies+1)); fi
    if [ "$disabled" -gt 0 ] 2>/dev/null; then disabled_seen=1; anomalies=$((anomalies+1)); fi
    if [ "$overdue" -gt 0 ] 2>/dev/null; then anomalies=$((anomalies+1)); fi
    [ "$max_cfail" -gt 0 ] 2>/dev/null && stuck_failing_seen=1   # observability only
    # nullnext on an enabled schedule is logged (could be a fired one-shot) but
    # does not itself fail the gate — the auto-disable case is caught by `disabled`.
    if [ "$n_sched" != "null" ] && { [ -z "$schedule_min" ] || [ "$n_sched" -lt "$schedule_min" ] 2>/dev/null; }; then
      schedule_min="$n_sched"
    fi

    # An unwritable log must abort, not silently PASS on a phantom log the summary
    # then tells you to archive (soak review #109 Low).
    printf '{"at":"%s","health":%s,"code":"%s","sched_code":"%s","pid":%s,"restarts":%s,"rss_mb":%s,"cpu_pct":%s,"schedules":%s,"disabled":%s,"nullnext":%s,"overdue":%s,"sched_err":%s,"max_consec_fail":%s,"last_run":%s}\n' \
      "$ts" "$ok" "$code" "$scode" "${pid:-0}" "$restarts" "${rss_mb:-0}" "${cpu_pct:-0}" "${n_sched:-0}" "$disabled" "$nullnext" "$overdue" "$sched_err" "${max_cfail:-0}" "${last_run:-\"null\"}" >> "$LOG" \
      || { echo "soak-monitor: cannot write $LOG — aborting" >&2; exit 2; }
  fi

  # Stop after the configured duration.
  [ $(( $(date +%s) - start )) -ge "$DURATION" ] && summarize

  # Sleep in the background and `wait` on it so an INT/TERM (Ctrl-C, or `kill`
  # on a nohup'd run) fires the summarize trap IMMEDIATELY instead of blocking
  # up to a full interval — at the default 300s that made a backgrounded run
  # feel unkillable (soak review #109 Low).
  sleep "$INTERVAL" &
  wait "$!"
done
