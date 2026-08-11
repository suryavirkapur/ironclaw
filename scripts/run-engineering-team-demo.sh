#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ -f .env ]]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

: "${OPENAI_API_KEY:?Set OPENAI_API_KEY to an OpenRouter-compatible API key}"
: "${TELEGRAM_BOT_TOKEN:?Set TELEGRAM_BOT_TOKEN from BotFather}"
: "${OWNER_TELEGRAM_CHAT_ID:?Set OWNER_TELEGRAM_CHAT_ID to your numeric Telegram chat ID}"

kernel="kernels/firecracker/vmlinux-6.1.155.bin"
rootfs="rootfs/build/ubuntu-24.04.ext4"

if [[ ! -f "$kernel" || ! -f "$rootfs" ]]; then
  echo "The demo VM image is missing. Build it with:"
  echo "  ./scripts/build-ubuntu-rootfs.sh"
  exit 1
fi

if [[ ! -r /dev/kvm || ! -w /dev/kvm ]]; then
  echo "The current user needs read/write access to /dev/kvm."
  exit 1
fi

export IRONCLAWD_CONFIG="configs/ironclawd.engineering-team.telegram.toml"
exec cargo run -p ironclawd --features firecracker
