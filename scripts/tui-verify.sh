#!/usr/bin/env bash
# TUI verification (C6 acceptance). The interactive keystroke contracts
# (explicit decision, Esc = no decision) are covered by the agent24-cli unit
# tests (tui::app); this script verifies the surrounding wiring the TUI relies
# on and prints the manual SSH-scenario walkthrough (which needs a tool-calling
# LLM to produce a real approval, so it stays operator-driven).
set -euo pipefail
cd "$(dirname "$0")/.."

echo "=== build ==="
(cd rust && cargo build -q -p agent24d -p agent24-cli)
BIN_DIR="rust/target/debug"
export AGENT24D_BIN="$BIN_DIR/agent24d"
CLI="$BIN_DIR/agent24"
DAEMON="$BIN_DIR/agent24d"

echo "=== tui --help ==="
"$CLI" --help | grep -q "tui" || { echo "FAIL: tui subcommand missing"; exit 1; }

echo "=== start ephemeral daemon ==="
READY_LOG="$(mktemp)"
HOME_DIR="$(mktemp -d)"
HOME="$HOME_DIR" "$DAEMON" serve --port 0 --ephemeral >"$READY_LOG" 2>/dev/null &
DAEMON_PID=$!
trap 'kill "$DAEMON_PID" 2>/dev/null || true; rm -rf "$READY_LOG" "$HOME_DIR"' EXIT

for _ in $(seq 1 40); do
  grep -q '"type":"ready"' "$READY_LOG" && break
  sleep 0.25
done
grep -q '"type":"ready"' "$READY_LOG" || { echo "FAIL: daemon never became ready"; cat "$READY_LOG"; exit 1; }
PORT=$(python3 -c "import json,sys;print(json.load(open('$READY_LOG'))['port'])")
TOKEN=$(python3 -c "import json,sys;print(json.load(open('$READY_LOG'))['token'])")
BASE="http://127.0.0.1:$PORT"
echo "daemon ready on $BASE"

auth=(-H "Authorization: Bearer $TOKEN")

echo "=== TUI REST dependencies respond ==="
# The TUI reconciles via these three endpoints — all must answer with 2xx.
curl -sf "${auth[@]}" "$BASE/api/v1/runs" >/dev/null           || { echo "FAIL: GET /runs"; exit 1; }
curl -sf "${auth[@]}" "$BASE/api/v1/approvals?status=pending" >/dev/null || { echo "FAIL: GET /approvals"; exit 1; }
curl -sf "${auth[@]}" "$BASE/api/v1/tools" >/dev/null          || { echo "FAIL: GET /tools"; exit 1; }
echo "  /runs, /approvals, /tools OK"

echo "=== WS events endpoint upgrades ==="
# A non-Origin Upgrade request must get 101 Switching Protocols (not 403/426).
code=$(curl -s --max-time 5 -o /dev/null -w '%{http_code}' \
  -H "Authorization: Bearer $TOKEN" \
  -H "Connection: Upgrade" -H "Upgrade: websocket" \
  -H "Sec-WebSocket-Version: 13" -H "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==" \
  "$BASE/api/v1/events" || true)
[ "$code" = "101" ] || { echo "FAIL: WS upgrade returned $code (want 101)"; exit 1; }
echo "  /events upgrades (101)"

echo
echo "SMOKE OK — automated TUI wiring checks passed."
echo
cat <<'MANUAL'
── Manual SSH-scenario walkthrough (needs a local tool-calling LLM) ──────────
  1. Terminal A:  agent24 daemon start
  2. Terminal A:  agent24 tui
                  → three panels: Runs · Events · Approvals
  3. Terminal B:  agent24 chat "use shell_exec to run: echo hello"
                  (or POST /api/v1/runs with a prompt that calls shell_exec)
  4. In the TUI:  the run appears (status await-approval), the approval shows
                  in the Approvals panel.
  5. Press Tab to focus Approvals, ↑/↓ to select, Enter to open the modal.
  6. Contract A (explicit decision): ↑/↓ over the offered decisions, Enter on
     "approve" → the run resumes and reaches completed.
  7. Contract B (Esc semantics): re-run, open the modal, press Esc → NO
     decision is sent; the approval stays pending and later times out
     (fail-closed) — the TUI never invents an approve/deny.
──────────────────────────────────────────────────────────────────────────────
MANUAL
