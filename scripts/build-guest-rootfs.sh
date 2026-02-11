#!/usr/bin/env bash
set -euo pipefail

# Builds a minimal ext4 rootfs directory tree for the Firecracker guest.
# - includes /init (busybox shell script)
# - includes /bin/irowclaw
# - includes busybox + common applets
#
# Notes:
# - This builds a directory tree, not an ext4 image.
#   ironclawd will convert it to ext4 at VM start (if configured that way).
# - If /bin/irowclaw is dynamically linked, this script will try to copy required libs.

ROOT_DIR="${1:-rootfs/build/guest-root}"
IROWCLAW_BIN_REL="${IROWCLAW_BIN_REL:-target/x86_64-unknown-linux-musl/release/irowclaw}"
IROWCLAW_BIN_ALT_REL="${IROWCLAW_BIN_ALT_REL:-target/release/irowclaw}"

mkdir -p "${ROOT_DIR}"

# Layout
mkdir -p \
  "${ROOT_DIR}/bin" \
  "${ROOT_DIR}/sbin" \
  "${ROOT_DIR}/etc" \
  "${ROOT_DIR}/proc" \
  "${ROOT_DIR}/sys" \
  "${ROOT_DIR}/dev" \
  "${ROOT_DIR}/tmp" \
  "${ROOT_DIR}/mnt/brain/config" \
  "${ROOT_DIR}/usr/bin" \
  "${ROOT_DIR}/usr/sbin" \
  "${ROOT_DIR}/lib" \
  "${ROOT_DIR}/lib64" \
  "${ROOT_DIR}/lib/modules"

# /init
install -m 0755 rootfs/guest-skel/init "${ROOT_DIR}/init"

# BusyBox
BUSYBOX_BIN="${BUSYBOX_BIN:-}"
if [[ -z "${BUSYBOX_BIN}" ]]; then
  if command -v busybox >/dev/null 2>&1; then
    BUSYBOX_BIN="$(command -v busybox)"
  elif [[ -x /usr/bin/busybox ]]; then
    BUSYBOX_BIN="/usr/bin/busybox"
  else
    echo "busybox not found. Install it (eg: sudo pacman -S busybox)" >&2
    exit 1
  fi
fi

install -m 0755 "${BUSYBOX_BIN}" "${ROOT_DIR}/bin/busybox"

# Common applet links
for app in sh mount mkdir ln echo cat sleep ip ifconfig route udhcpc su ss modprobe insmod lsmod; do
  ln -sf busybox "${ROOT_DIR}/bin/${app}"
done

# Build irowclaw
pushd . >/dev/null
cd "$(git rev-parse --show-toplevel)"

IROWCLAW_USE_MUSL="${IROWCLAW_USE_MUSL:-0}"

if [[ "${IROWCLAW_USE_MUSL}" == "1" && ! -f "${IROWCLAW_BIN_REL}" ]]; then
  if command -v musl-gcc >/dev/null 2>&1; then
    echo "building irowclaw musl" >&2
    rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1 || true
    export CC_x86_64_unknown_linux_musl=musl-gcc
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc
    # Force a fully static, non-PIE binary for the minimal guest.
    export RUSTFLAGS='-C target-feature=+crt-static -C link-arg=-static -C link-arg=-no-pie'
    cargo build -q -p irowclaw --release --target x86_64-unknown-linux-musl || true
  else
    echo "musl-gcc not found, skipping musl build" >&2
  fi
fi

IROWCLAW_BIN=""
if [[ -f "${IROWCLAW_BIN_REL}" ]]; then
  IROWCLAW_BIN="${IROWCLAW_BIN_REL}"
elif [[ -f "${IROWCLAW_BIN_ALT_REL}" ]]; then
  echo "musl build not found, falling back to host target/release (likely dynamic)" >&2
  cargo build -q -p irowclaw --release
  IROWCLAW_BIN="${IROWCLAW_BIN_ALT_REL}"
else
  echo "irowclaw binary not found and build failed" >&2
  exit 1
fi

popd >/dev/null

install -m 0755 "${IROWCLAW_BIN}" "${ROOT_DIR}/bin/irowclaw"

# If this is a musl-linked binary, ensure the musl interpreter exists inside the rootfs.
# Without it, execve returns ENOENT and /init will report "/bin/irowclaw: not found".
if file "${ROOT_DIR}/bin/irowclaw" | grep -q "interpreter /lib/ld-musl-x86_64.so.1"; then
  if [[ -f /usr/lib/musl/lib/libc.so ]]; then
    mkdir -p "${ROOT_DIR}/lib"
    install -m 0755 /usr/lib/musl/lib/libc.so "${ROOT_DIR}/lib/ld-musl-x86_64.so.1"
  fi
fi

# Copy vsock-related kernel modules (best effort).
# This helps when the host kernel has vsock as modules, and the guest rootfs is minimal.
KVER="$(uname -r)"
MOD_BASE="/usr/lib/modules/${KVER}"
if [[ -d "${MOD_BASE}" ]]; then
  mods=(
    "kernel/net/vmw_vsock/vsock.ko.zst"
    "kernel/net/vmw_vsock/vmw_vsock_virtio_transport_common.ko.zst"
    "kernel/net/vmw_vsock/vmw_vsock_virtio_transport.ko.zst"
    "kernel/drivers/vhost/vhost_iotlb.ko.zst"
    "kernel/drivers/vhost/vhost.ko.zst"
    "kernel/drivers/vhost/vhost_vsock.ko.zst"
  )

  if command -v zstd >/dev/null 2>&1; then
    for rel in "${mods[@]}"; do
      src="${MOD_BASE}/${rel}"
      if [[ -f "${src}" ]]; then
        out_name="$(basename "${src}" .zst)"
        out_path="${ROOT_DIR}/lib/modules/${out_name}"
        zstd -q -d -c "${src}" >"${out_path}" || true
      fi
    done
  fi
fi

# If dynamic, copy libs.
# This is best-effort and intended for local dev.
if ldd "${ROOT_DIR}/bin/irowclaw" 2>/dev/null | grep -q "=>"; then
  echo "irowclaw appears dynamically linked, copying shared libs" >&2

  while read -r line; do
    # Example lines:
    #   libz.so.1 => /usr/lib/libz.so.1 (0x...)
    #   /lib64/ld-linux-x86-64.so.2 (0x...)
    src="$(echo "$line" | awk '{print $3}' | sed 's/(.*//')"
    if [[ -z "${src}" || ! -e "${src}" ]]; then
      # try direct path line
      src="$(echo "$line" | awk '{print $1}' | sed 's/(.*//')"
    fi
    if [[ -n "${src}" && -e "${src}" ]]; then
      dest_dir="${ROOT_DIR}$(dirname "${src}")"
      mkdir -p "${dest_dir}"
      cp -L "${src}" "${dest_dir}/"
    fi
  done < <(ldd "${ROOT_DIR}/bin/irowclaw" || true)

  # Ensure the dynamic loader exists at the path encoded in the ELF interpreter.
  # On Arch the file often lives under /usr/lib64 but the interpreter is /lib64/ld-linux-x86-64.so.2.
  if [[ -f "${ROOT_DIR}/usr/lib64/ld-linux-x86-64.so.2" && ! -e "${ROOT_DIR}/lib64/ld-linux-x86-64.so.2" ]]; then
    mkdir -p "${ROOT_DIR}/lib64"
    ln -sf /usr/lib64/ld-linux-x86-64.so.2 "${ROOT_DIR}/lib64/ld-linux-x86-64.so.2"
  fi
fi

echo "rootfs dir ready: ${ROOT_DIR}" >&2
