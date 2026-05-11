#!/bin/bash
# oMLX helper: kill / start / restart
# Usage:
#   ./omlx.sh          — restart (kill then start)
#   ./omlx.sh stop     — kill only
#   ./omlx.sh start    — start only (skip if already running)
#   ./omlx.sh status   — show running process + model list

PORT=8088
API_KEY=xiaobao8088
BASE_URL="http://127.0.0.1:${PORT}"

_kill() {
  # Kill by port (handles any process name: python3, oMLX, etc.)
  # lsof -ti returns one PID per line; only kill the LISTEN process
  LISTEN_PID=$(lsof -i :${PORT} -n -P 2>/dev/null | awk '/LISTEN/{print $2}' | sort -u)
  if [ -n "$LISTEN_PID" ]; then
    echo "Killing PID(s) $LISTEN_PID on :${PORT}…"
    echo "$LISTEN_PID" | xargs kill 2>/dev/null || true
    sleep 1
    echo "$LISTEN_PID" | xargs kill -9 2>/dev/null || true
    echo "Stopped."
  else
    echo "Nothing listening on :${PORT}."
  fi
  pkill -f 'omlx serve' 2>/dev/null || true
}

_start() {
  # Wait up to 5s for port to be released after kill
  for i in $(seq 1 5); do
    lsof -i :${PORT} -n -P 2>/dev/null | grep -q LISTEN || break
    echo "Waiting for :${PORT} to free… (${i}s)"
    sleep 1
  done
  if lsof -i :${PORT} -n -P 2>/dev/null | grep -q LISTEN; then
    echo ":${PORT} still in use — cannot start."
    return
  fi
  echo "Starting oMLX on :${PORT}…"
  nohup omlx serve --port "${PORT}" --api-key "${API_KEY}" \
    > /tmp/omlx.log 2>&1 &
  echo "PID $! — log: /tmp/omlx.log"

  # Wait up to 10s for server to be ready
  for i in $(seq 1 10); do
    sleep 1
    MODELS=$(curl -s -H "Authorization: Bearer ${API_KEY}" \
      "${BASE_URL}/v1/models" 2>/dev/null | grep -o '"id":"[^"]*"' | sed 's/"id":"//;s/"//')
    if [ -n "$MODELS" ]; then
      echo "Ready. Models:"
      echo "$MODELS" | sed 's/^/  /'
      return
    fi
    echo -n "."
  done
  echo ""
  echo "Timeout — check /tmp/omlx.log"
}

_status() {
  PID=$(lsof -i :${PORT} -n -P 2>/dev/null | awk '/LISTEN/{print $2}' | sort -u)
  if [ -n "$PID" ]; then
    echo "oMLX running (PID $PID) on :${PORT}"
    MODELS=$(curl -s -H "Authorization: Bearer ${API_KEY}" \
      "${BASE_URL}/v1/models" 2>/dev/null | grep -o '"id":"[^"]*"' | sed 's/"id":"//;s/"//')
    if [ -n "$MODELS" ]; then
      echo "Models:"
      echo "$MODELS" | sed 's/^/  /'
    else
      echo "Server not responding yet."
    fi
  else
    echo "oMLX not running on :${PORT}."
  fi
}

CMD="${1:-restart}"

case "$CMD" in
  stop)    _kill ;;
  start)   _start ;;
  status)  _status ;;
  restart) _kill; _start ;;
  *)       echo "Usage: $0 [stop|start|restart|status]"; exit 1 ;;
esac
