#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

ADDR="${ADDR:-127.0.0.1:9938}"
WS_URL="${WS_URL:-ws://${ADDR}/ws}"
HOST_CONFIG="${HOST_CONFIG:-configs/ironclawd.guest-tools.toml}"
HOST_LOG="${HOST_LOG:-/tmp/ironclawd.guest-tools.log}"
CLIENT_LOG="${CLIENT_LOG:-/tmp/ironclawd.guest-tools.client.log}"
MAX_ATTEMPTS="${MAX_ATTEMPTS:-8}"
BASE_DELAY_SEC="${BASE_DELAY_SEC:-0.5}"

require_bin() {
  local bin="$1"
  if ! command -v "${bin}" >/dev/null 2>&1; then
    echo "missing required tool: ${bin}" >&2
    exit 1
  fi
}

cleanup() {
  if [[ -n "${IRONCLAWD_PID:-}" ]]; then
    kill "${IRONCLAWD_PID}" 2>/dev/null || true
    wait "${IRONCLAWD_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

require_bin cargo
require_bin node

rm -f "${HOST_LOG}" "${CLIENT_LOG}" 2>/dev/null || true

IRONCLAWD_CONFIG="${HOST_CONFIG}" cargo run -q -p ironclawd >"${HOST_LOG}" 2>&1 &
IRONCLAWD_PID=$!

attempt=1
passed=0
while [[ "${attempt}" -le "${MAX_ATTEMPTS}" ]]; do
  if WS_TIMEOUT_MS=15000 \
    node ./scripts/smoke-guest-tools-ws.mjs "${WS_URL}" >"${CLIENT_LOG}" 2>&1; then
    passed=1
    break
  fi
  sleep_sec=$(awk "BEGIN {print ${attempt} * ${BASE_DELAY_SEC}}")
  sleep "${sleep_sec}"
  attempt=$((attempt + 1))
done

if [[ "${passed}" -ne 1 ]]; then
  echo "FAIL: guest websocket tool/policy smoke" >&2
  echo "--- host log (tail) ---" >&2
  tail -n 120 "${HOST_LOG}" >&2 || true
  echo "--- client log ---" >&2
  cat "${CLIENT_LOG}" >&2 || true
  exit 1
fi

echo "$(tail -n 1 "${CLIENT_LOG}")"
