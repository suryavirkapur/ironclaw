#!/usr/bin/env bash
set -euo pipefail

UBUNTU_RELEASE="${UBUNTU_RELEASE:-noble}"
UBUNTU_IMAGE="${UBUNTU_IMAGE:-ubuntu-24.04-minimal-cloudimg-amd64-root.tar.xz}"
UBUNTU_BASE_URL="${UBUNTU_BASE_URL:-https://cloud-images.ubuntu.com/minimal/releases/${UBUNTU_RELEASE}/release}"
ROOTFS_IMAGE="${1:-rootfs/build/ubuntu-24.04.ext4}"
ROOTFS_CACHE_DIR="${ROOTFS_CACHE_DIR:-rootfs/cache}"
ROOTFS_FREE_MB="${ROOTFS_FREE_MB:-3072}"
IROWCLAW_BIN="${IROWCLAW_BIN:-target/x86_64-unknown-linux-musl/release/irowclaw}"

for required in curl sha256sum tar truncate mkfs.ext4 du install chmod; do
  if ! command -v "${required}" >/dev/null 2>&1; then
    echo "missing required command: ${required}" >&2
    exit 1
  fi
done

mkdir -p "${ROOTFS_CACHE_DIR}" "$(dirname "${ROOTFS_IMAGE}")"
archive="${ROOTFS_CACHE_DIR}/${UBUNTU_IMAGE}"
sums="${ROOTFS_CACHE_DIR}/SHA256SUMS-${UBUNTU_RELEASE}"

if [[ ! -f "${archive}" ]]; then
  curl --fail --location --retry 3 \
    "${UBUNTU_BASE_URL}/${UBUNTU_IMAGE}" \
    --output "${archive}"
fi
curl --fail --location --retry 3 \
  "${UBUNTU_BASE_URL}/SHA256SUMS" \
  --output "${sums}"

expected="$(
  awk -v image="${UBUNTU_IMAGE}" \
    '$2 == image || $2 == "*" image { print $1; exit }' \
    "${sums}"
)"
if [[ -z "${expected}" ]]; then
  echo "Ubuntu checksum entry missing for ${UBUNTU_IMAGE}" >&2
  exit 1
fi
printf '%s  %s\n' "${expected}" "${archive}" | sha256sum --check -

export RUSTFLAGS='-C target-feature=+crt-static'
rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1 || true
cargo build -q -p irowclaw --release --target x86_64-unknown-linux-musl

build_root="$(mktemp -d)"
tar --extract --xz --file "${archive}" --directory "${build_root}" \
  --no-same-owner --exclude='dev/*' --exclude='var/lib/snapd/void'
install -m 0755 rootfs/ubuntu-init "${build_root}/init"
install -m 0755 "${IROWCLAW_BIN}" "${build_root}/usr/local/bin/irowclaw"
mkdir -p \
  "${build_root}/mnt/brain/config" \
  "${build_root}/mnt/brain/workspace" \
  "${build_root}/mnt/brain/cron" \
  "${build_root}/mnt/brain/logs" \
  "${build_root}/mnt/brain/db" \
  "${build_root}/root"
install -m 0644 configs/irowclaw.allow-bash.toml \
  "${build_root}/mnt/brain/config/irowclaw.toml"
printf 'jobs = []\n' >"${build_root}/mnt/brain/cron/jobs.toml"

if [[ -L "${build_root}/etc/resolv.conf" ]]; then
  unlink "${build_root}/etc/resolv.conf"
fi
printf 'nameserver 1.1.1.1\nnameserver 8.8.8.8\n' \
  >"${build_root}/etc/resolv.conf"

used_mb="$(du -sm "${build_root}" | awk '{print $1}')"
image_mb="$((used_mb + ROOTFS_FREE_MB))"
if [[ -e "${ROOTFS_IMAGE}" ]]; then
  chmod u+w "${ROOTFS_IMAGE}"
fi
if [[ -e "${ROOTFS_IMAGE}.sha256" ]]; then
  chmod u+w "${ROOTFS_IMAGE}.sha256"
fi
truncate --size "${image_mb}M" "${ROOTFS_IMAGE}"
mkfs.ext4 -F -q -d "${build_root}" "${ROOTFS_IMAGE}"
sha256sum "${ROOTFS_IMAGE}" >"${ROOTFS_IMAGE}.sha256"
chmod 0444 "${ROOTFS_IMAGE}" "${ROOTFS_IMAGE}.sha256"

echo "Ubuntu root-agent image ready: ${ROOTFS_IMAGE} (${image_mb} MiB)" >&2
