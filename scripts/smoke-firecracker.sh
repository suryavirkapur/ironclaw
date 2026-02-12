#!/usr/bin/env bash
set -euo pipefail

# runs a firecracker smoke test with a full host<->guest vsock round trip.
# this avoids tcp websocket bind requirements and validates vm bootstrap directly.

KERNEL_PATH="${KERNEL_PATH:-kernels/firecracker/vmlinux-6.1.155.bin}"
ROOTFS_DIR="${ROOTFS_DIR:-rootfs/build/guest-root}"
ROOTFS_PATH="${ROOTFS_PATH:-rootfs/build/guest-rootfs.ext4}"
API_SOCKET_DIR="${API_SOCKET_DIR:-/tmp/ironclaw-fc}"
VSOCK_UDS_DIR="${VSOCK_UDS_DIR:-/tmp/ironclaw/vsock}"
VSOCK_PORT="${VSOCK_PORT:-5000}"
FIRECRACKER_BIN="${FIRECRACKER_BIN:-firecracker}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

./scripts/build-guest-rootfs.sh "${ROOTFS_DIR}" "${ROOTFS_PATH}"

rm -f "${API_SOCKET_DIR}"/*.sock "${VSOCK_UDS_DIR}"/*.sock 2>/dev/null || true
pgrep -af firecracker >/dev/null 2>&1 && pkill -9 firecracker 2>/dev/null || true

if ! FIRECRACKER_BIN="${FIRECRACKER_BIN}" \
  KERNEL_PATH="${KERNEL_PATH}" \
  ROOTFS_PATH="${ROOTFS_PATH}" \
  API_SOCKET_DIR="${API_SOCKET_DIR}" \
  VSOCK_UDS_DIR="${VSOCK_UDS_DIR}" \
  VSOCK_PORT="${VSOCK_PORT}" \
  cargo run -q -p common --features firecracker --example firecracker_vsock_roundtrip; then
  if [[ -f /tmp/firecracker-console.log ]]; then
    echo "firecracker console log:" >&2
    tail -n 80 /tmp/firecracker-console.log >&2 || true
  fi
  exit 1
fi
