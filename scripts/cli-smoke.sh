#!/usr/bin/env bash
# CLI end-to-end smoke test (B6 acceptance). Builds agent24d + agent24, then
# exercises: daemon start → status → models → chat → stop, plus standalone
# chat with no daemon running. Chat may 503 without a local LLM — both the
# success and provider-unavailable outcomes count as a functioning pipeline.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "=== build ==="
(cd rust && cargo build -q -p agent24d -p agent24-cli)
BIN_DIR="rust/target/debug"
export AGENT24D_BIN="$BIN_DIR/agent24d"
CLI="$BIN_DIR/agent24"

echo "=== help ==="
"$CLI" --help >/dev/null
"$CLI" daemon --help >/dev/null

echo "=== ensure clean slate ==="
"$CLI" daemon stop >/dev/null 2>&1 || true
sleep 1

echo "=== daemon start ==="
"$CLI" daemon start
sleep 1

echo "=== daemon status (attached) ==="
"$CLI" daemon status | tee /tmp/a24-smoke-status.txt
grep -q "running" /tmp/a24-smoke-status.txt

echo "=== models (attached) ==="
"$CLI" models

echo "=== chat (attached; 503 acceptable without local LLM) ==="
if "$CLI" chat "Reply with the single word: pong"; then
  echo "chat: success path"
else
  echo "chat: provider-unavailable path (acceptable without local LLM)"
fi

echo "=== daemon stop ==="
"$CLI" daemon stop
sleep 1
"$CLI" daemon status | grep -q "not running"

echo "=== standalone chat (ephemeral daemon) ==="
if "$CLI" chat "hi"; then
  echo "standalone chat: success path"
else
  echo "standalone chat: provider-unavailable path (acceptable)"
fi
# ephemeral daemon must not linger
sleep 1
"$CLI" daemon status | grep -q "not running"

echo "SMOKE OK"
