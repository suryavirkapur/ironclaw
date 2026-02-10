#!/usr/bin/env bash
set -euo pipefail

# Runs ironclawd in local guest mode and performs a websocket smoke test.

ADDR="${ADDR:-127.0.0.1:9938}"
WS_URL="${WS_URL:-ws://${ADDR}/ws}"

cleanup() {
  if [[ -n "${IRONCLAWD_PID:-}" ]]; then
    kill "${IRONCLAWD_PID}" 2>/dev/null || true
    wait "${IRONCLAWD_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

cargo build -q -p ironclawd

./target/debug/ironclawd >/tmp/ironclawd.local.log 2>&1 &
IRONCLAWD_PID=$!

# give it a moment
sleep 0.3

node scripts/smoke-local-ws.mjs "${WS_URL}"
