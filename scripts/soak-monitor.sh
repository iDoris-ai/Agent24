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
#   - nostr_state / nostr_confirmed / nostr_degraded: the Nostr bridge's liveness
#     snapshot (FU-32). WITHOUT this, F5 criterion 6 ("inbound was never dead")
#     cannot be checked after the fact at all: that file is OVERWRITTEN in place,
#     so an outage on Tuesday that recovered by Wednesday leaves no trace in it.
#     Sampling it here, plus the cumulative degraded_transitions counter it
#     carries, is what makes the criterion evidence instead of a promise.
#
# On exit (Ctrl-C, SIGTERM, or --duration elapsed) it prints a PASS/FAIL summary
# against the F5 criteria.
#
# Usage:
#   scripts/soak-monitor.sh                 # 7 days, sample every 5 min
#   scripts/soak-monitor.sh --interval 60 --duration 3600   # 1h smoke, 1 min
#   scripts/soak-monitor.sh --log ~/agent24-soak.jsonl
#   scripts/soak-monitor.sh --no-nostr        # this run genuinely has no bridge
#
# Env: A24_DAEMON_JSON (default ~/.agent24/daemon.json). Requires curl + jq.
set -u

INTERVAL=300            # seconds between samples
DURATION=$((7*24*3600)) # total run length (7 days)
DAEMON_JSON="${A24_DAEMON_JSON:-$HOME/.agent24/daemon.json}"
# Identity-scoped, matching the bridge's own default (packages/nostr-bridge
# config.ts): one file per bridge, so two instances cannot overwrite each other's
# ledger and make one's outage invisible behind the other's health.
NOSTR_IDENTITY="${A24_NOSTR_IDENTITY:-agent24}"
# Deliberately NOT resolved here: the default path depends on the identity, and
# the identity can still change during argument parsing. Computing it now made
# `--nostr-identity alice` read agent24's file — the new flag was broken for its
# only intended use.
NOSTR_HEALTH="${A24_NOSTR_HEALTH_FILE:-}"
# F5 runs Nostr, so its absence is a FAILURE, not a "not configured" — a bridge
# that never started, or crashed before writing, otherwise leaves exactly the
# same evidence as "the user didn't ask for Nostr" and the gate is skipped.
# Opting out has to be explicit.
REQUIRE_NOSTR=1
# A snapshot that stopped being updated is worthless even if its last recorded
# state was `ok`: a bridge that died on day 1 freezes the file at ok/confirmed=N
# forever, and a start-vs-end comparison happily passes. Same shape of bug as
# FU-32 itself — stale evidence that cannot be told from healthy evidence.
NOSTR_STALE=900
LOG=""

while [ $# -gt 0 ]; do
  case "$1" in
    --interval) INTERVAL="$2"; shift 2 ;;
    --duration) DURATION="$2"; shift 2 ;;
    --daemon-json) DAEMON_JSON="$2"; shift 2 ;;
    --nostr-health) NOSTR_HEALTH="$2"; shift 2 ;;
    --nostr-stale) NOSTR_STALE="$2"; shift 2 ;;
    --nostr-identity) NOSTR_IDENTITY="$2"; shift 2 ;;
    --no-nostr) REQUIRE_NOSTR=0; shift ;;
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
# Zero would satisfy the "reached DURATION" test on the very first sample, so a
# `--duration 0` run could hand back a verdict having observed nothing.
[ "$INTERVAL" -gt 0 ] || { echo "--interval must be > 0" >&2; exit 2 ; }
[ "$DURATION" -gt 0 ] || { echo "--duration must be > 0" >&2; exit 2 ; }
# A non-integer here would make every `-gt` print an error and evaluate FALSE,
# i.e. silently DISABLE the frozen-snapshot check — the gate would still say
# "fine" while checking nothing.
case "$NOSTR_STALE" in ''|*[!0-9]*) echo "--nostr-stale must be integer seconds" >&2; exit 2 ;; esac
[ "$NOSTR_STALE" -gt 0 ] || { echo "--nostr-stale must be > 0" >&2; exit 2 ; }
# Resolved AFTER parsing, and only when the operator did not name a file.
# Sanitised exactly like the bridge does it (packages/nostr-bridge config.ts) —
# otherwise the two sides compute different default paths and a healthy run fails
# for no reason. Identities outside that character set are refused rather than
# silently mangled, because the two sanitisers only agree on ASCII.
case "$NOSTR_IDENTITY" in
  *[!A-Za-z0-9_.-]*)
    if [ -z "$NOSTR_HEALTH" ]; then
      echo "identity '$NOSTR_IDENTITY' 含特殊字符,默认路径可能与桥算出的不一致;请显式传 --nostr-health" >&2
      exit 2
    fi ;;
esac
[ -n "$NOSTR_HEALTH" ] || NOSTR_HEALTH="$HOME/.agent24/nostr-bridge-health-${NOSTR_IDENTITY}.json"
[ -n "$LOG" ] || LOG="$HOME/agent24-soak-$(date +%Y%m%d-%H%M%S).jsonl"
# Fail loudly NOW if the log is unwritable — never run a 7-day soak whose PASS
# rests on a log that was never written (soak review #109 Low).
( : >> "$LOG" ) 2>/dev/null || { echo "log not writable: $LOG" >&2; exit 2; }

# Set only when the loop reaches DURATION. A 7-day run killed at minute 20 has
# not shown anything about 7 days, and `summarize()` runs from the signal trap
# too — without this it would happily print PASS for it.
completed=0

# Counters (whole-run accounting).
samples=0; health_ok=0; restarts=0; overdue_max=0; last_pid=""
schedule_min=""; anomalies=0; disabled_seen=0; sched_err_seen=0; stuck_failing_seen=0
# Nostr inbound liveness (FU-32). `nostr_seen` starts at 0 so a soak run without
# the bridge configured is not failed by criterion 6; once a snapshot HAS been
# read, a later disappearance is an anomaly rather than "no bridge here".
nostr_seen=0; nostr_conf_first=""; nostr_conf_last=0; nostr_read_err=0
nostr_degraded_seen=0; nostr_stale_seen=0; nostr_tr_base=""; nostr_tr_last=0; nostr_tr_back=0
nostr_first_epoch=""; nostr_last_epoch=0; nostr_short=0
nostr_gen_first=""; nostr_gen_changed=0; nostr_conf_back=0; nostr_late=0
n_state="null"; n_conf="null"; n_degr="null"; n_age="null"; n_gen="null"

read_daemon() { # -> echoes "port token pid" or nothing
  [ -f "$DAEMON_JSON" ] || return 1
  jq -r '"\(.port) \(.token) \(.pid)"' "$DAEMON_JSON" 2>/dev/null
}

# ISO-8601 UTC without relying on GNU date flags.
now_iso() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# Read the Nostr bridge's liveness snapshot into n_state/n_conf/n_degr/n_age and
# fold it into the whole-run accounting.
#
# The read is a single strict `jq` program on purpose. Reading the fields one by
# one and comparing them with `[ -gt ]` looks equivalent and is not: a
# `degraded_transitions` of 0.5 or a `confirmed` of "x" makes the test print an
# integer-expression error and evaluate FALSE, which leaves the gate variables
# saying "fine". Malformed evidence has to fail the gate, never slip through it.
sample_nostr() {
  n_state="null"; n_conf="null"; n_degr="null"; n_age="null"; n_gen="null"
  # `--no-nostr` means SKIP, not "read it anyway and hope nothing fails". A stale
  # or foreign snapshot left over from an earlier run would otherwise set
  # nostr_seen and drag the whole gate in — the flag would fail the very runs it
  # exists to let through.
  [ "$REQUIRE_NOSTR" -eq 0 ] && return
  if [ ! -f "$NOSTR_HEALTH" ]; then
    # Missing before the bridge has ever written one is normal for a moment — the
    # monitor is usually started first. But only for a moment: the grace period
    # is bounded, because "the file showed up eventually" is not evidence about
    # the days before it did. Missing AFTER we have seen it is a separate
    # failure: the file was removed or the bridge tore it down.
    if [ "$nostr_seen" -eq 1 ]; then
      nostr_read_err=1; anomalies=$((anomalies+1))
    elif [ "$REQUIRE_NOSTR" -eq 1 ] && [ $(( $(date +%s) - start )) -gt "$NOSTR_STALE" ]; then
      nostr_late=1; anomalies=$((anomalies+1))
    fi
    return
  fi
  local row st cf dg age gen
  # `-s` (slurp) + `length == 1` is load-bearing: a file holding two concatenated
  # top-level JSON documents otherwise makes jq emit TWO rows and exit 0, and the
  # shell then compares strings like "degraded\nok" — every numeric test errors,
  # evaluates false, and the gate reads "fine" on evidence that is plainly broken.
  row=$(jq -ers --arg want "$NOSTR_IDENTITY" '
    def isnat: type == "number" and . >= 0 and . == floor and . < 9007199254740991;
    select(length == 1) | .[0]
    | select((.state | type) == "string")
    | select(.state == "ok" or .state == "starting" or .state == "degraded")
    | (.canaries.confirmed) as $c
    | (.degraded_transitions // 0) as $d
    | select(($c | isnat) and ($d | isnat))
    | (.updated_at | sub("\\.[0-9]+Z$"; "Z") | fromdateiso8601) as $u
    | ((now - $u) | floor) as $age
    # A snapshot stamped in the FUTURE is not fresh evidence, it is a clock that
    # moved (or a file that was touched). Left unchecked it buys a dead bridge
    # unlimited extra grace, because a negative age never exceeds the threshold.
    # A few seconds of sub-second/NTP jitter is tolerated and floored to 0.
    | select($age >= -5)
    # Shape-checked because it is echoed back into the JSONL below and the
    # snapshot is an operator-editable file: a generation containing a quote
    # would emit invalid JSON.
    | select(((.generation // "") | tostring) | test("^[A-Za-z0-9_.:-]{0,64}$"))
    # The snapshot has to say whose it is. Without this, pointing
    # `--nostr-health` at a file belonging to another identity lets THAT bridge
    # vouch for this one — and the runbook already claims a foreign snapshot
    # fails, so the claim has to be true here, not only inside the bridge.
    # (No apostrophes in these comments: the jq program lives inside a
    # single-quoted shell string, and one would close it mid-program.)
    | select(((.context.identity // "") | tostring) == $want)
    | [.state, ($c|tostring), ($d|tostring), (([$age, 0] | max) | tostring),
       (.generation // "" | tostring)]
    | @tsv' "$NOSTR_HEALTH" 2>/dev/null) || row=""
  if [ -z "$row" ]; then
    # The file EXISTS and does not validate. Never "no bridge configured" — that
    # would let a snapshot corrupt from the very first sample leave criterion 6
    # silently un-evaluated.
    nostr_seen=1; nostr_read_err=1; anomalies=$((anomalies+1))
    return
  fi
  # Belt and braces after the slurp guard: anything multi-line here is malformed.
  # `$'\n'`, NOT `$(printf '\n')` — command substitution strips trailing
  # newlines, so the latter is an EMPTY pattern that matches every row and would
  # silently reject every snapshot.
  case "$row" in *$'\n'*) nostr_seen=1; nostr_read_err=1; anomalies=$((anomalies+1)); return ;; esac
  st=$(printf '%s' "$row" | cut -f1)
  cf=$(printf '%s' "$row" | cut -f2)
  dg=$(printf '%s' "$row" | cut -f3)
  age=$(printf '%s' "$row" | cut -f4)
  gen=$(printf '%s' "$row" | cut -f5)

  # Checked HERE, not only in the missing-file branch: if the last missing
  # sample fell before the threshold and the file appeared after it but before
  # the next sample, that branch never runs again and the lateness would be lost
  # — one whole interval of no-evidence, silently forgiven.
  if [ "$nostr_seen" -eq 0 ] && [ "$REQUIRE_NOSTR" -eq 1 ] \
     && [ $(( $(date +%s) - start )) -gt "$NOSTR_STALE" ]; then
    nostr_late=1; anomalies=$((anomalies+1))
  fi
  nostr_seen=1
  local nowsec; nowsec=$(date +%s)
  [ -z "$nostr_first_epoch" ] && nostr_first_epoch="$nowsec"
  nostr_last_epoch="$nowsec"
  # Compare against the PREVIOUS value before overwriting it — comparing after
  # the assignment is `cf < cf`, i.e. a rollback check that can never fire.
  [ "$cf" -lt "$nostr_conf_last" ] && nostr_conf_back=1
  [ -z "$nostr_conf_first" ] && nostr_conf_first="$cf"
  nostr_conf_last="$cf"
  # Transitions are LIFETIME cumulative (they survive bridge restarts), so the
  # gate is "did it grow during THIS run", not "is it zero". Otherwise one
  # degradation months ago would fail every future soak, and the runbook would
  # have to tell people to delete the file — throwing away the cross-restart
  # evidence and the stored self_npub with it.
  [ -z "$nostr_tr_base" ] && nostr_tr_base="$dg"
  [ "$dg" -lt "$nostr_tr_last" ] && nostr_tr_back=1   # counter went backwards = reset/tampered
  nostr_tr_last="$dg"
  # Rollback detection on the counters catches only a reset we happen to SAMPLE
  # mid-way. A snapshot lost and rebuilt from zero between two samples shows no
  # rollback by the time anyone looks — while the reset has taken the accumulated
  # silence with it, which is precisely how a real outage would vanish from the
  # record. The generation id is the chain: it survives a restart that restores
  # cleanly and changes whenever the accounting starts over.
  if [ -z "$nostr_gen_first" ]; then
    nostr_gen_first="$gen"
  elif [ "$gen" != "$nostr_gen_first" ]; then
    nostr_gen_changed=1; anomalies=$((anomalies+1))
  fi
  # Any sample that catches it degraded is a sticky failure on its own, not just
  # an anomaly count that nothing gates on.
  if [ "$st" = "degraded" ]; then nostr_degraded_seen=1; anomalies=$((anomalies+1)); fi
  if [ "$age" -gt "$NOSTR_STALE" ]; then nostr_stale_seen=1; anomalies=$((anomalies+1)); fi
  n_state="\"$st\""; n_conf="$cf"; n_degr="$dg"; n_age="$age"; n_gen="\"$gen\""
}

summarize() {
  # A second Ctrl-C while this is printing would re-enter and interleave two
  # summaries (and could exit before writing the verdict).
  trap '' INT TERM
  # Computed first: the report lines below read it.
  #
  # Measured against the RUN's own length, not the span between the first and
  # last Nostr sample. Using the sample span let a bridge that appeared for the
  # final ten minutes of a seven-day run claim the short-run exemption — six days
  # with no evidence at all, and a PASS at the end of it.
  local run_secs=$(( $(date +%s) - start ))
  if [ "$run_secs" -lt "$NOSTR_STALE" ]; then nostr_short=1; fi
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
  if [ "$nostr_seen" -eq 1 ]; then
    echo "nostr inbound:   transitions ${nostr_tr_base:-0}→${nostr_tr_last}  confirmed ${nostr_conf_first:-0}→${nostr_conf_last}"
    echo "                 degraded_sampled=${nostr_degraded_seen}  stale_snapshots=${nostr_stale_seen}  read_errors=${nostr_read_err}"
    echo "                 counter_reset=${nostr_tr_back}/${nostr_conf_back}  generation_changed=${nostr_gen_changed}"
    [ "${nostr_short:-0}" -eq 1 ] && echo "                 ⚠️  本 run 采样跨度 < ${NOSTR_STALE}s,「confirmed 必须增长」这一条未评估" 
  elif [ "$REQUIRE_NOSTR" -eq 1 ]; then
    echo "nostr inbound:   NO HEALTH SNAPSHOT (bridge never wrote one — criterion 6 FAILS; pass --no-nostr if this run genuinely has no Nostr bridge)"
  else
    echo "nostr inbound:   (skipped, --no-nostr)"
  fi
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
  # F5 criterion 6 (FU-32). Gates only once a snapshot has been seen, so a run
  # without the Nostr bridge is not failed by it — but from then on it is HARD,
  # and it is judged from the CUMULATIVE counter, not from "state looked ok
  # whenever someone happened to sample it": the health file is overwritten in
  # place, so a degradation that recovered leaves no other trace. `confirmed`
  # must also have advanced — a bridge stuck at `starting` never reported
  # degraded either, and that must not read as healthy.
  local nostr_ok=1
  # A snapshot that only appeared long after the run began cannot vouch for the
  # stretch before it. Sticky, so a late arrival cannot clear it.
  [ "$nostr_late" -ne 0 ] && nostr_ok=0
  if [ "$REQUIRE_NOSTR" -eq 1 ] && [ "$nostr_seen" -eq 0 ]; then
    # "No evidence" is not "no bridge asked for". A bridge that never started
    # leaves exactly the same trace as one that was never configured, so the
    # intent has to be stated, not inferred.
    nostr_ok=0
  elif [ "$nostr_seen" -eq 1 ]; then
    # Fails if: it was ever caught degraded · the cumulative transition counter
    # grew during THIS run · the counter went backwards (file reset/tampered) ·
    # any snapshot was unreadable · any snapshot was STALE (a frozen file is not
    # evidence of health) · confirmations never advanced (a bridge stuck in
    # `starting` never reported degraded either).
    if [ "$nostr_degraded_seen" -ne 0 ] || [ "$nostr_read_err" -ne 0 ] \
       || [ "$nostr_stale_seen" -ne 0 ] || [ "$nostr_tr_back" -ne 0 ] \
       || [ "$nostr_conf_back" -ne 0 ] || [ "$nostr_gen_changed" -ne 0 ] \
       || [ "$nostr_tr_last" -gt "${nostr_tr_base:-0}" ]; then
      nostr_ok=0
    fi
    # "confirmations advanced" can only be required of a run long enough for a
    # canary to have gone out and come back. Demanding it of a two-minute smoke
    # test would fail a perfectly healthy bridge — and a false FAIL costs a week,
    # because the answer to it is "run the soak again". Below that span the check
    # is reported as NOT EVALUATED rather than silently passed.
    if [ "$nostr_short" -eq 0 ]; then
      [ "$nostr_conf_last" -le "${nostr_conf_first:-0}" ] && nostr_ok=0
    fi
  fi

  # Placed AFTER the diagnostics above, not before: someone who interrupts a run
  # still wants to see what it had collected. It just must not be called a
  # verdict.
  if [ "$completed" -eq 0 ]; then
    echo "RESULT: INCOMPLETE (run 未跑满 ${DURATION}s 就被中断 —— 不是 F5 结论,上面的数字只是中断时的快照)"
    exit 1
  fi

  local result=1
  if [ "$samples" -gt 0 ] && [ "$health_ok" -eq "$samples" ] \
     && [ "${schedule_min:-0}" -gt 0 ] && [ "$overdue_max" -eq 0 ] \
     && [ "$disabled_seen" -eq 0 ] && [ "$sched_err_seen" -eq 0 ] \
     && [ "$nostr_ok" -eq 1 ]; then
    if [ "${nostr_short:-0}" -eq 1 ] && [ "$REQUIRE_NOSTR" -eq 1 ]; then
      # Everything checked passed, but the run was too short to evaluate
      # criterion 6 at all. That is not an F5 PASS — saying so would let a
      # ten-minute smoke test read as a seven-day result. Non-zero on purpose,
      # so automation cannot mistake it either.
      echo "RESULT: SMOKE PASS (其余判据通过,但 run 长度 < ${NOSTR_STALE}s,F5 判据 6 未评估 —— 不能作为 F5 结论)"
      result=1
    else
      echo "RESULT: PASS (health 100%, ≥1 live schedule every sample, no overdue/auto-disabled/fetch-error$([ "$nostr_seen" -eq 1 ] && echo ", nostr inbound never degraded"))"
      result=0
    fi
  else
    echo "RESULT: NEEDS REVIEW (see anomalies + scheduler faults in the log)"
    if [ "$nostr_ok" -eq 0 ]; then
      if [ "$nostr_seen" -eq 0 ] && [ "$nostr_late" -eq 0 ]; then
        echo "  · Nostr 入站(F5 判据 6):整个 run 没有读到任何健康快照 —— 桥没起来/没写过文件。确实不测 Nostr 请显式加 --no-nostr"
      elif [ "$nostr_late" -ne 0 ]; then
        echo "  · Nostr 入站(F5 判据 6):健康快照迟到超过 ${NOSTR_STALE}s 才出现 —— 在那之前这一段没有任何入站证据"
      else
        echo "  · Nostr 入站(F5 判据 6):degraded_sampled=${nostr_degraded_seen} transitions ${nostr_tr_base:-0}→${nostr_tr_last} stale=${nostr_stale_seen} read_err=${nostr_read_err} counter_reset=${nostr_tr_back}/${nostr_conf_back} generation_changed=${nostr_gen_changed} confirmed ${nostr_conf_first:-0}→${nostr_conf_last}"
      fi
    fi
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
  sample_nostr
  if [ -z "$info" ]; then
    # No discovery file → daemon not registered. Record and keep watching;
    # F1a autostart / F2 recovery may bring it back.
    echo "{\"at\":\"$ts\",\"health\":false,\"reason\":\"no daemon.json\",\"nostr_state\":$n_state,\"nostr_confirmed\":$n_conf,\"nostr_degraded\":$n_degr,\"nostr_age_s\":$n_age,\"nostr_gen\":$n_gen}" >> "$LOG"
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
    printf '{"at":"%s","health":%s,"code":"%s","sched_code":"%s","pid":%s,"restarts":%s,"rss_mb":%s,"cpu_pct":%s,"schedules":%s,"disabled":%s,"nullnext":%s,"overdue":%s,"sched_err":%s,"max_consec_fail":%s,"last_run":%s,"nostr_state":%s,"nostr_confirmed":%s,"nostr_degraded":%s,"nostr_age_s":%s,"nostr_gen":%s}\n' \
      "$ts" "$ok" "$code" "$scode" "${pid:-0}" "$restarts" "${rss_mb:-0}" "${cpu_pct:-0}" "${n_sched:-0}" "$disabled" "$nullnext" "$overdue" "$sched_err" "${max_cfail:-0}" "${last_run:-\"null\"}" "$n_state" "$n_conf" "$n_degr" "$n_age" "$n_gen" >> "$LOG" \
      || { echo "soak-monitor: cannot write $LOG — aborting" >&2; exit 2; }
  fi

  # Stop after the configured duration.
  if [ $(( $(date +%s) - start )) -ge "$DURATION" ]; then completed=1; summarize; fi

  # Sleep in the background and `wait` on it so an INT/TERM (Ctrl-C, or `kill`
  # on a nohup'd run) fires the summarize trap IMMEDIATELY instead of blocking
  # up to a full interval — at the default 300s that made a backgrounded run
  # feel unkillable (soak review #109 Low).
  sleep "$INTERVAL" &
  wait "$!"
done
