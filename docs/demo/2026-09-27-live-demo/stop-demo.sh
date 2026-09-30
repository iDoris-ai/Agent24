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
  # Review (ME4-CODEX-DEBT-10 #4): a stale PID file (left over from an
  # earlier demo run, or from a run that already exited) can name a PGID the
  # OS has since recycled for a totally unrelated process group. Before
  # sending any signal, confirm the group is (a) still alive and (b) still
  # actually this demo's Electron/vite tree -- not just some other process
  # that happened to land on the same recycled PGID.
  if kill -0 "-$PGID" 2>/dev/null && ps -o command= -g "$PGID" 2>/dev/null | grep -qE 'Electron|electron|[Vv]ite'; then
    echo "killing desktop dev process group $PGID"
    kill -TERM "-$PGID" 2>/dev/null || true
    sleep 2
    kill -KILL "-$PGID" 2>/dev/null || true
  else
    echo "desktop-dev.pid names group $PGID, but it is gone or no longer looks like Electron/vite -- not signaling it"
  fi
  rm -f "$DEMO_DIR/desktop-dev.pid"
fi

# Belt-and-suspenders: HOME is an env var, not argv, so agent24d's own
# command line never shows it -- `daemon stop` above is the real mechanism.
# This just catches an Electron (main + all its Helper subprocesses) that
# the daemon-stop path wouldn't touch. The user-data-dir arg is unique to
# this demo and appears in every one of that tree's process lines.
#
# Review (ME4-CODEX-DEBT-10 #5): `pkill -f` does an UNANCHORED substring
# match against the whole command line -- it would just as happily kill an
# unrelated process that merely mentions this path (e.g. an editor with a
# log file under this profile dir open as an argument). Use `pgrep -f` for
# CANDIDATES only, then for each one require the profile flag to appear as
# a whole argument (word-boundary padded compare, not a raw substring) AND
# that the command line actually looks like Electron or vite, before
# signaling it.
DESKTOP_PROFILE_ARG="--user-data-dir=/tmp/a24demo-desktop-profile"
for pid in $(pgrep -f -- "$DESKTOP_PROFILE_ARG" 2>/dev/null || true); do
  cmd=$(ps -o command= -p "$pid" 2>/dev/null || true)
  [ -n "$cmd" ] || continue
  case " $cmd " in
    *" $DESKTOP_PROFILE_ARG "*)
      if echo "$cmd" | grep -qE 'Electron|electron|[Vv]ite'; then
        echo "killing pid $pid ($cmd)"
        kill -TERM "$pid" 2>/dev/null || true
      else
        echo "skipping pid $pid: has the profile arg but does not look like Electron/vite: $cmd"
      fi
      ;;
    *)
      echo "skipping pid $pid: profile path matched only as a substring, not the exact argument: $cmd"
      ;;
  esac
done

echo "== remaining agent24d / electron / vite processes (for manual review) =="
ps aux | grep -E "agent24d|electron .|vite" | grep -v grep || echo "(none)"

echo "done."
