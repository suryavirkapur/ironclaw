# Telegram test setup (Firecracker only)

Ironclaw uses Telegram long polling on the host, but every agent request is routed through
the Firecracker guest sandbox. The supplied config deliberately enables `guest_tools` and
Firecracker; there is no host-only test path in these instructions.

## 1. Create the bot and credentials

1. Open [@BotFather](https://t.me/BotFather), run `/newbot`, and save the bot token.
2. Copy the environment template and fill in the API and bot tokens:

   ```bash
   cp .env.example .env
   chmod 600 .env
   ```

3. Send `/start` to your new bot, then load the token and inspect the latest update:

   ```bash
   set -a
   source .env
   set +a
   curl --fail-with-body \
     "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/getUpdates"
   ```

   Copy the numeric `message.chat.id` into `OWNER_TELEGRAM_CHAT_ID` in `.env`. Ironclaw
   rejects messages from every other chat ID.

If the bot previously used a webhook, remove it before starting Ironclaw because Telegram
does not allow `getUpdates` while a webhook is active:

```bash
curl --fail-with-body --request POST \
  "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/deleteWebhook"
```

## 2. Build the Firecracker guest

The test config expects the checked-in kernel and a generated guest root filesystem:

```bash
./scripts/build-ubuntu-rootfs.sh
test -f kernels/firecracker/vmlinux-6.1.155.bin
test -f rootfs/build/ubuntu-24.04.ext4
```

Firecracker also requires Linux KVM access:

```bash
test -r /dev/kvm
test -w /dev/kvm
```

## 3. Start Ironclaw

Load the secrets and run the daemon with the Firecracker feature and Telegram config:

```bash
set -a
source .env
set +a
IRONCLAWD_CONFIG=configs/ironclawd.telegram.toml \
  sudo --preserve-env=OPENAI_API_KEY,BRAVE_API_KEY,TELEGRAM_BOT_TOKEN,OWNER_TELEGRAM_CHAT_ID \
  cargo +nightly-2025-12-26 run -p ironclawd --features firecracker
```

Wait for `telegram loop started` in the logs, then send the bot a message. On its first
reply it reports `running in firecracker vm`; subsequent replies are produced through the
sandboxed guest. Runtime data, offsets, and transcripts stay under the ignored `data/`
directory. PNG and JPEG artifacts published by the guest are delivered with Telegram's
`sendPhoto`; SVG and PDF artifacts use `sendDocument`.

You can send the bot a Telegram document with an optional caption describing what to do.
PDF, TeX, source, archive, and binary documents up to 8 MiB are downloaded only for the
owner chat and transferred into `workspace/uploads` inside the owner's Firecracker VM.
The guest receives a sanitized workspace path rather than host filesystem access.

`BRAVE_API_KEY` is transferred ephemerally to the Firecracker guest during its authenticated
startup handshake. It is kept in memory for the `browser` tool's `search` action and is not
written into the rootfs or guest configuration.

## Troubleshooting

- `telegram enabled but bot token is missing`: reload `.env`.
- `owner telegram chat id is missing`: set `OWNER_TELEGRAM_CHAT_ID` to the numeric chat ID.
- `BRAVE_API_KEY is not configured`: set it in `.env` and preserve it when starting the daemon.
- `telegram getupdates failed`: remove any webhook and ensure only one poller uses the bot.
- Firecracker startup or KVM errors: verify `/dev/kvm`, the kernel path, and the generated
  rootfs before retrying.
