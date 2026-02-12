#!/usr/bin/env bash
set -euo pipefail

# end-to-end firecracker websocket smoke:
# websocket -> ironclawd -> guest over vsock -> ironclawd -> websocket

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

KERNEL_PATH="${KERNEL_PATH:-kernels/firecracker/vmlinux-6.1.155.bin}"
ROOTFS_DIR="${ROOTFS_DIR:-rootfs/build/guest-root}"
ROOTFS_PATH="${ROOTFS_PATH:-rootfs/build/guest-rootfs.ext4}"
HOST_CONFIG="${HOST_CONFIG:-configs/ironclawd.firecracker.toml}"
FIRECRACKER_BIN="${FIRECRACKER_BIN:-firecracker}"
ADDR="${ADDR:-127.0.0.1:9938}"
WS_URL="${WS_URL:-ws://${ADDR}/ws}"
API_SOCKET_DIR="${API_SOCKET_DIR:-/tmp/ironclaw-fc}"
VSOCK_UDS_DIR="${VSOCK_UDS_DIR:-/tmp/ironclaw/vsock}"
HOST_LOG="${HOST_LOG:-/tmp/ironclawd.firecracker-ws.log}"
CLIENT_LOG="${CLIENT_LOG:-/tmp/ironclawd.firecracker-ws.client.log}"
FC_CONSOLE_LOG="${FC_CONSOLE_LOG:-/tmp/firecracker-console.log}"
MAX_ATTEMPTS="${MAX_ATTEMPTS:-8}"
BASE_DELAY_SEC="${BASE_DELAY_SEC:-0.5}"

require_bin() {
  local bin="$1"
  if ! command -v "${bin}" >/dev/null 2>&1; then
    echo "missing required tool: ${bin}" >&2
    exit 1
  fi
}

preflight_ws_bind() {
  local bind_host="$1"
  local bind_port="$2"
  if ! node -e '
const net = require("net");
const host = process.argv[1];
const port = Number(process.argv[2]);
const server = net.createServer();
server.once("error", (err) => {
  console.error(`cannot bind ${host}:${port}: ${err.message}`);
  process.exit(1);
});
server.listen(port, host, () => {
  server.close(() => process.exit(0));
});
' "${bind_host}" "${bind_port}" >/dev/null 2>&1; then
    echo "cannot bind websocket server on ${bind_host}:${bind_port}" >&2
    echo "run this script on a host that allows local tcp listen sockets" >&2
    exit 1
  fi
}

fail_with_logs() {
  local message="$1"
  echo "FAIL: ${message}" >&2
  if [[ -f "${HOST_LOG}" ]]; then
    echo "--- host log (tail) ---" >&2
    tail -n 120 "${HOST_LOG}" >&2 || true
  fi
  if [[ -f "${FC_CONSOLE_LOG}" ]]; then
    echo "--- firecracker console log (tail) ---" >&2
    tail -n 120 "${FC_CONSOLE_LOG}" >&2 || true
  fi
  if [[ -f "${CLIENT_LOG}" ]]; then
    echo "--- websocket client log (tail) ---" >&2
    tail -n 40 "${CLIENT_LOG}" >&2 || true
  fi
  exit 1
}

cleanup() {
  if [[ -n "${IRONCLAWD_PID:-}" ]]; then
    kill "${IRONCLAWD_PID}" 2>/dev/null || true
    wait "${IRONCLAWD_PID}" 2>/dev/null || true
  fi
  pkill -9 firecracker >/dev/null 2>&1 || true
  rm -f "${API_SOCKET_DIR}"/*.sock "${VSOCK_UDS_DIR}"/*.sock 2>/dev/null || true
}
trap cleanup EXIT

require_bin cargo
require_bin node
require_bin "${FIRECRACKER_BIN}"

if [[ ! -e /dev/kvm ]]; then
  echo "missing /dev/kvm (kvm is required for firecracker smoke)" >&2
  exit 1
fi

if [[ ! -r /dev/kvm || ! -w /dev/kvm ]]; then
  echo "kvm is not accessible: need read/write access to /dev/kvm" >&2
  exit 1
fi

if [[ ! -f "${KERNEL_PATH}" ]]; then
  echo "kernel not found: ${KERNEL_PATH}" >&2
  exit 1
fi

bind_host="${ADDR%:*}"
bind_port="${ADDR##*:}"
preflight_ws_bind "${bind_host}" "${bind_port}"

./scripts/build-guest-rootfs.sh "${ROOTFS_DIR}" "${ROOTFS_PATH}"

rm -f "${HOST_LOG}" "${CLIENT_LOG}" "${FC_CONSOLE_LOG}" 2>/dev/null || true
rm -f "${API_SOCKET_DIR}"/*.sock "${VSOCK_UDS_DIR}"/*.sock 2>/dev/null || true

IRONCLAWD_CONFIG="${HOST_CONFIG}" \
  cargo run -q -p ironclawd --features firecracker >"${HOST_LOG}" 2>&1 &
IRONCLAWD_PID=$!

attempt=1
passed=0
while [[ "${attempt}" -le "${MAX_ATTEMPTS}" ]]; do
  marker="step4-ws-${attempt}-$(date +%s%N)"
  if WS_TIMEOUT_MS=15000 node ./scripts/smoke-firecracker-ws.mjs \
    "${WS_URL}" "${marker}" >"${CLIENT_LOG}" 2>&1; then
    echo "PASS: $(tail -n 1 "${CLIENT_LOG}")"
    passed=1
    break
  fi

  sleep_sec=$(awk "BEGIN {print ${attempt} * ${BASE_DELAY_SEC}}")
  echo "retry ${attempt}/${MAX_ATTEMPTS} failed; sleeping ${sleep_sec}s" >&2
  sleep "${sleep_sec}"
  attempt=$((attempt + 1))
done

if [[ "${passed}" -ne 1 ]]; then
  fail_with_logs "websocket roundtrip did not complete"
fi
