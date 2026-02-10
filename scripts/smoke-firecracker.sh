#!/usr/bin/env bash
set -euo pipefail

# Runs ironclawd in firecracker mode and performs the websocket smoke test.
# Assumes:
# - firecracker binary is in PATH
# - host kernel vmlinux exists
# - guest rootfs dir exists (build with scripts/build-guest-rootfs.sh)

CONFIG_PATH="${CONFIG_PATH:-configs/ironclawd.firecracker.toml}"
WS_URL="${WS_URL:-ws://127.0.0.1:9938/ws}"

cleanup() {
  if [[ -n "${IRONCLAWD_PID:-}" ]]; then
    kill "${IRONCLAWD_PID}" 2>/dev/null || true
    wait "${IRONCLAWD_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

./scripts/build-guest-rootfs.sh rootfs/build/guest-root
cargo build -q -p ironclawd --features firecracker --release

export IRONCLAWD_CONFIG="${CONFIG_PATH}"

# Make sure stale processes do not interfere.
pgrep -af firecracker >/dev/null 2>&1 && pkill -9 firecracker 2>/dev/null || true
pgrep -af ironclawd >/dev/null 2>&1 && pkill -9 ironclawd 2>/dev/null || true

rm -f /tmp/ironclaw-fc/*.sock /tmp/ironclaw/vsock/*.sock 2>/dev/null || true

./target/release/ironclawd >/tmp/ironclawd.fc.log 2>&1 &
IRONCLAWD_PID=$!

sleep 1.2

node ./scripts/smoke-local-ws.mjs "${WS_URL}"
