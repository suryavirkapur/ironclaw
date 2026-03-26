#!/usr/bin/env bash
set -euo pipefail

# builds a minimal firecracker guest rootfs directory and ext4 image.
# outputs:
# - root tree: rootfs/build/guest-root (or arg1)
# - ext4 image: rootfs/build/guest-rootfs.ext4 (or arg2 / ROOTFS_IMAGE)
#
# optional: set INCLUDE_PYTHON=1 to include Python3
# optional: set INCLUDE_NODE=1 to include Node.js

ROOT_DIR="${1:-rootfs/build/guest-root}"
ROOTFS_IMAGE="${2:-${ROOTFS_IMAGE:-rootfs/build/guest-rootfs.ext4}}"
ROOTFS_SIZE_MB="${ROOTFS_SIZE_MB:-256}"
IROWCLAW_BIN_REL="${IROWCLAW_BIN_REL:-target/x86_64-unknown-linux-musl/release/irowclaw}"
IROWCLAW_BIN_ALT_REL="${IROWCLAW_BIN_ALT_REL:-target/release/irowclaw}"
IROWCLAW_USE_MUSL="${IROWCLAW_USE_MUSL:-1}"
INCLUDE_PYTHON="${INCLUDE_PYTHON:-0}"
INCLUDE_NODE="${INCLUDE_NODE:-0}"

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

# ============================================================================
# Python 3 Installation
# ============================================================================
if [[ "${INCLUDE_PYTHON}" == "1" ]]; then
  echo "Adding Python 3 to rootfs..." >&2
  
  PYTHON_BIN=""
  if command -v python3 >/dev/null 2>&1; then
    PYTHON_BIN="$(command -v python3)"
  elif [[ -x /usr/bin/python3 ]]; then
    PYTHON_BIN="/usr/bin/python3"
  else
    echo "WARNING: python3 not found, skipping" >&2
  fi

  if [[ -n "${PYTHON_BIN}" ]]; then
    # Install python binary
    install -m 0755 "${PYTHON_BIN}" "${ROOT_DIR}/usr/bin/python3"
    ln -sf python3 "${ROOT_DIR}/usr/bin/python"
    
    # Copy python shared library dependencies
    copy_binary_deps "${ROOT_DIR}/usr/bin/python3"
    
    # Copy Python standard library (minimal)
    PYTHON_VER="$(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
    PYTHON_LIB="/usr/lib/python${PYTHON_VER}"
    
    if [[ -d "${PYTHON_LIB}" ]]; then
      mkdir -p "${ROOT_DIR}/usr/lib/python${PYTHON_VER}"
      
      # Copy essential stdlib modules
      for module in os sys io re json asyncio threading subprocess pathlib \
        typing collections itertools functools contextlib traceback warnings \
        types weakref copy pickle heapq bisect math time datetime hashlib random \
        string textwrap unicodedata encodings; do
        if [[ -d "${PYTHON_LIB}/${module}" ]]; then
          mkdir -p "${ROOT_DIR}/usr/lib/python${PYTHON_VER}/${module}"
          cp -r "${PYTHON_LIB}/${module}"/* "${ROOT_DIR}/usr/lib/python${PYTHON_VER}/${module}/"
        elif [[ -f "${PYTHON_LIB}/${module}.py" ]]; then
          cp "${PYTHON_LIB}/${module}.py" "${ROOT_DIR}/usr/lib/python${PYTHON_VER}/"
        fi
      done
      
      # Copy __pycache__ for compiled modules
      if [[ -d "${PYTHON_LIB}/__pycache__" ]]; then
        mkdir -p "${ROOT_DIR}/usr/lib/python${PYTHON_VER}/__pycache__"
        cp "${PYTHON_LIB}/__pycache__/"*.pyc "${ROOT_DIR}/usr/lib/python${PYTHON_VER}/__pycache__/" 2>/dev/null || true
      fi
    fi
    
    # Install pip if available
    if command -v pip3 >/dev/null 2>&1; then
      PIP_BIN="$(command -v pip3)"
      install -m 0755 "${PIP_BIN}" "${ROOT_DIR}/usr/bin/pip3" 2>/dev/null || true
      ln -sf pip3 "${ROOT_DIR}/usr/bin/pip" 2>/dev/null || true
    fi
    
    echo "Python ${PYTHON_VER} installed" >&2
  fi
fi

# ============================================================================
# Node.js Installation
# ============================================================================
if [[ "${INCLUDE_NODE}" == "1" ]]; then
  echo "Adding Node.js to rootfs..." >&2
  
  NODE_BIN=""
  if command -v node >/dev/null 2>&1; then
    NODE_BIN="$(command -v node)"
  elif [[ -x /usr/bin/node ]]; then
    NODE_BIN="/usr/bin/node"
  else
    echo "WARNING: node not found, skipping" >&2
  fi

  if [[ -n "${NODE_BIN}" ]]; then
    # Install node binary
    install -m 0755 "${NODE_BIN}" "${ROOT_DIR}/usr/bin/node"
    
    # Copy node shared library dependencies
    copy_binary_deps "${ROOT_DIR}/usr/bin/node"
    
    # Create minimal node_modules structure
    mkdir -p "${ROOT_DIR}/usr/lib/node_modules"
    
    # Install npm if available
    if command -v npm >/dev/null 2>&1; then
      NPM_BIN="$(command -v npm)"
      install -m 0755 "${NPM_BIN}" "${ROOT_DIR}/usr/bin/npm" 2>/dev/null || true
      
      # npm is a script, also need node_modules/npm
      NPM_LIB="/usr/lib/node_modules/npm"
      if [[ -d "${NPM_LIB}" ]]; then
        cp -r "${NPM_LIB}" "${ROOT_DIR}/usr/lib/node_modules/" 2>/dev/null || true
      fi
    fi
    
    echo "Node.js installed" >&2
  fi
fi

# Increase rootfs size if runtimes are included
if [[ "${INCLUDE_PYTHON}" == "1" || "${INCLUDE_NODE}" == "1" ]]; then
  # Python ~50MB, Node ~30MB + stdlib
  ROOTFS_SIZE_MB="${ROOTFS_SIZE_MB:-512}"
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
