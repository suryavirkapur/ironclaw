#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
guest_binary="${1:-${repo_root}/target/x86_64-unknown-linux-musl/release/irowclaw}"
base_image="${2:-${repo_root}/rootfs/build/ubuntu-24.04.ext4}"
users_root="${3:-${repo_root}/data/engineering-team/users}"

if [[ ! -x "${guest_binary}" ]]; then
  echo "guest binary is missing or not executable: ${guest_binary}" >&2
  exit 1
fi
if [[ ! -f "${base_image}" ]]; then
  echo "base rootfs is missing: ${base_image}" >&2
  exit 1
fi

mapfile -t private_images < <(find "${users_root}" -mindepth 3 -maxdepth 3 \
  -type f -path '*/vm/rootfs.ext4' -print | sort)
images=("${base_image}" "${private_images[@]}")
expected_size="$(stat -c %s "${guest_binary}")"

for image in "${images[@]}"; do
  if findmnt -rn -S "${image}" >/dev/null 2>&1; then
    echo "refusing to modify mounted rootfs: ${image}" >&2
    exit 1
  fi
  e2fsck -pf "${image}" >/dev/null || status=$?
  if [[ "${status:-0}" -gt 1 ]]; then
    echo "filesystem check failed for ${image}" >&2
    exit 1
  fi
  unset status
  debugfs -w -R 'rm /usr/local/bin/irowclaw' "${image}" >/dev/null 2>&1
  debugfs -w -R "write ${guest_binary} /usr/local/bin/irowclaw" "${image}" >/dev/null 2>&1
  debugfs -w -R 'set_inode_field /usr/local/bin/irowclaw mode 0100755' "${image}" >/dev/null 2>&1
  installed_size="$(debugfs -R 'stat /usr/local/bin/irowclaw' "${image}" 2>/dev/null \
    | awk '/Size:/{for (i=1; i<=NF; i++) if ($i == "Size:") {print $(i+1); exit}}')"
  if [[ "${installed_size}" != "${expected_size}" ]]; then
    echo "guest binary validation failed for ${image}: ${installed_size} != ${expected_size}" >&2
    exit 1
  fi
  echo "updated ${image}"
done
