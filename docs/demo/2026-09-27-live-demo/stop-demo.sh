#!/usr/bin/env bash
# Stop everything the demo started: the CLI-driven agent24d (isolated
# home-cli) and the desktop dev processes (Vite + Electron + its own
# sidecar agent24d under home-desktop). Never touches the real ~/.agent24.
set -uo pipefail

DEMO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENT24_ROOT="/Users/jason/Dev/auraai/Agent24"
AGENT24_CLI_BIN="$AGENT24_ROOT/rust/target/debug/agent24"
# Kept as short /tmp paths, not under scratchpad -- see run-demo.sh's note
# on the Unix domain socket path-length limit.
HOME_CLI="/tmp/a24demo-cli"
HOME_DESKTOP="/tmp/a24demo-desktop"

echo "== stopping CLI-driven daemon (home-cli) =="
if [ -f "$HOME_CLI/.agent24/daemon.json" ]; then
  HOME="$HOME_CLI" "$AGENT24_CLI_BIN" daemon stop || true
else
  echo "no daemon.json under $HOME_CLI -- nothing to stop via CLI"
fi

echo "== stopping desktop-sidecar daemon (home-desktop), if any =="
if [ -f "$HOME_DESKTOP/.agent24/daemon.json" ]; then
  HOME="$HOME_DESKTOP" "$AGENT24_CLI_BIN" daemon stop || true
fi

echo "== killing any leftover demo processes by PID file / pattern =="
if [ -f "$DEMO_DIR/desktop-dev.pid" ]; then
  PGID=$(cat "$DEMO_DIR/desktop-dev.pid")
  echo "killing desktop dev process group $PGID"
  kill -TERM "-$PGID" 2>/dev/null || true
  sleep 2
  kill -KILL "-$PGID" 2>/dev/null || true
  rm -f "$DEMO_DIR/desktop-dev.pid"
fi

# Belt-and-suspenders: HOME is an env var, not argv, so agent24d's own
# command line never shows it -- `daemon stop` above is the real mechanism.
# This just catches an Electron (main + all its Helper subprocesses) that
# the daemon-stop path wouldn't touch. The user-data-dir arg is unique to
# this demo and appears in every one of that tree's process lines.
pkill -f -- "--user-data-dir=/tmp/a24demo-desktop-profile" 2>/dev/null || true

echo "== remaining agent24d / electron / vite processes (for manual review) =="
ps aux | grep -E "agent24d|electron .|vite" | grep -v grep || echo "(none)"

echo "done."
