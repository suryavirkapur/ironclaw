#!/usr/bin/env bash
set -euo pipefail

# builds a minimal firecracker guest rootfs directory and ext4 image.
# outputs:
# - root tree: rootfs/build/guest-root (or arg1)
# - ext4 image: rootfs/build/guest-rootfs.ext4 (or arg2 / ROOTFS_IMAGE)

ROOT_DIR="${1:-rootfs/build/guest-root}"
ROOTFS_IMAGE="${2:-${ROOTFS_IMAGE:-rootfs/build/guest-rootfs.ext4}}"
ROOTFS_SIZE_MB="${ROOTFS_SIZE_MB:-256}"
IROWCLAW_BIN_REL="${IROWCLAW_BIN_REL:-target/x86_64-unknown-linux-musl/release/irowclaw}"
IROWCLAW_BIN_ALT_REL="${IROWCLAW_BIN_ALT_REL:-target/release/irowclaw}"
IROWCLAW_USE_MUSL="${IROWCLAW_USE_MUSL:-1}"

require_bin() {
  local bin
  bin="$1"
  if ! command -v "${bin}" >/dev/null 2>&1; then
    echo "missing required tool: ${bin}" >&2
    exit 1
  fi
}

copy_binary_deps() {
  local bin_path
  bin_path="$1"

  if ldd "${bin_path}" 2>/dev/null | grep -q "=>"; then
    while IFS= read -r line; do
      local src
      src="$(echo "${line}" | awk '{print $3}' | sed 's/(.*//')"
      if [[ -z "${src}" || ! -e "${src}" ]]; then
        src="$(echo "${line}" | awk '{print $1}' | sed 's/(.*//')"
      fi
      if [[ -n "${src}" && -e "${src}" ]]; then
        local dest_dir
        dest_dir="${ROOT_DIR}$(dirname "${src}")"
        mkdir -p "${dest_dir}"
        cp -L "${src}" "${dest_dir}/"
      fi
    done < <(ldd "${bin_path}" || true)

    if [[ -f "${ROOT_DIR}/usr/lib64/ld-linux-x86-64.so.2" && \
      ! -e "${ROOT_DIR}/lib64/ld-linux-x86-64.so.2" ]]; then
      mkdir -p "${ROOT_DIR}/lib64"
      ln -sf /usr/lib64/ld-linux-x86-64.so.2 "${ROOT_DIR}/lib64/ld-linux-x86-64.so.2"
    fi
  fi

  if file "${bin_path}" | grep -q "interpreter /lib/ld-musl-x86_64.so.1"; then
    if [[ -f /usr/lib/musl/lib/libc.so ]]; then
      mkdir -p "${ROOT_DIR}/lib"
      install -m 0755 /usr/lib/musl/lib/libc.so "${ROOT_DIR}/lib/ld-musl-x86_64.so.1"
    fi
  fi
}

require_bin mkfs.ext4
require_bin truncate
require_bin file
require_bin ldd

mkdir -p "${ROOT_DIR}"
mkdir -p "$(dirname "${ROOTFS_IMAGE}")"

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

install -m 0755 rootfs/guest-skel/init "${ROOT_DIR}/init"

BUSYBOX_BIN="${BUSYBOX_BIN:-}"
if [[ -z "${BUSYBOX_BIN}" ]]; then
  if command -v busybox >/dev/null 2>&1; then
    BUSYBOX_BIN="$(command -v busybox)"
  elif [[ -x /usr/bin/busybox ]]; then
    BUSYBOX_BIN="/usr/bin/busybox"
  else
    echo "busybox not found" >&2
    exit 1
  fi
fi

install -m 0755 "${BUSYBOX_BIN}" "${ROOT_DIR}/bin/busybox"

for app in sh mount mkdir ln echo cat sleep ip ifconfig route udhcpc su ss \
  modprobe insmod lsmod ls; do
  ln -sf busybox "${ROOT_DIR}/bin/${app}"
done

pushd . >/dev/null
cd "$(git rev-parse --show-toplevel)"

if [[ "${IROWCLAW_USE_MUSL}" == "1" ]]; then
  rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1 || true
  # Build a fully static guest binary. Avoid forcing -no-pie here because some
  # environments have shown early startup segfaults with non-PIE static executables.
  export RUSTFLAGS='-C target-feature=+crt-static'
  cargo build -q -p irowclaw --release --target x86_64-unknown-linux-musl || true
fi

IROWCLAW_BIN=""
if [[ -f "${IROWCLAW_BIN_REL}" ]]; then
  IROWCLAW_BIN="${IROWCLAW_BIN_REL}"
else
  cargo build -q -p irowclaw --release
  IROWCLAW_BIN="${IROWCLAW_BIN_ALT_REL}"
fi

popd >/dev/null

install -m 0755 "${IROWCLAW_BIN}" "${ROOT_DIR}/bin/irowclaw"

copy_binary_deps "${ROOT_DIR}/bin/busybox"
copy_binary_deps "${ROOT_DIR}/bin/irowclaw"

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

rm -f "${ROOTFS_IMAGE}"
truncate -s "${ROOTFS_SIZE_MB}M" "${ROOTFS_IMAGE}"
mkfs.ext4 -q -F -d "${ROOT_DIR}" -L ironclaw-rootfs "${ROOTFS_IMAGE}"

if ! debugfs -R "stat /init" "${ROOTFS_IMAGE}" >/dev/null 2>&1; then
  echo "rootfs image validation failed: /init missing" >&2
  exit 1
fi
if ! debugfs -R "stat /bin/irowclaw" "${ROOTFS_IMAGE}" >/dev/null 2>&1; then
  echo "rootfs image validation failed: /bin/irowclaw missing" >&2
  exit 1
fi

echo "rootfs dir ready: ${ROOT_DIR}" >&2
echo "rootfs image ready: ${ROOTFS_IMAGE}" >&2
