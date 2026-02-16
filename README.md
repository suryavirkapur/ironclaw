# ironclaw

a small vertical slice scaffold for a host daemon plus a guest runtime.

## crates

- `crates/ironclawd` - host daemon
  - Axum HTTP server
  - WebSocket bridge at `/ws`
  - Serves UI under `/ui`
- `crates/irowclaw` - guest runtime with safety checks, tool execution, and guest loop modes
- `crates/common` - shared config, protocol, transport, VM manager
- `crates/memory` - SQLite schema + indexing helpers
- `crates/tools` - guest tool registry and allowlisted built-ins

## build

```bash
cargo check
cargo check -p ironclawd --features firecracker
```

## run (local guest mode)

local guest mode is the default when firecracker is disabled.
in this mode, `ironclawd` uses an in-process transport pair
and runs the guest runtime locally.

```bash
cargo run -p ironclawd
```

then open a websocket connection to:

- `ws://127.0.0.1:9938/ws`

(port and bind come from the host config.)

## telegram mvp quickstart

the host can also run a telegram dm channel via long polling.

home directory semantics:

- telegram tool execution is vm-only; firecracker is required for isolated guest tools
- if firecracker is not enabled, telegram guest tool execution is disabled
- on first message per telegram session, runtime banner reports:
  - `running in firecracker vm`, or
  - `tools disabled: firecracker not enabled`

required env vars:

- `TELEGRAM_BOT_TOKEN` (bot token from botfather)
- `OWNER_TELEGRAM_CHAT_ID` (single allowlisted chat id for mvp)

optional config values (in `~/.config/ironclaw/ironclawd.toml`):

```toml
[telegram]
enabled = true
bot_token = "123456:example"
owner_chat_id = 123456789
poll_timeout_seconds = 30
```

run:

```bash
TELEGRAM_BOT_TOKEN=... OWNER_TELEGRAM_CHAT_ID=... \
cargo run -p ironclawd -- --telegram
```

to enable isolated telegram tools, set firecracker + guest mode in
`~/.config/ironclaw/ironclawd.toml` and run with the firecracker feature:

```toml
execution_mode = "guest_tools"

[firecracker]
enabled = true
kernel_path = "kernels/firecracker/vmlinux-6.1.155.bin"
rootfs_path = "rootfs/build/guest-rootfs.ext4"
api_socket_dir = "/tmp/ironclaw-fc"
vsock_uds_dir = "/tmp/ironclaw/vsock"
vsock_port = 5000
```

```bash
cargo run -p ironclawd --features firecracker -- --telegram
```

test:

- send a dm to your bot from the owner chat id
- check host logs for `telegram loop started`
- verify replies arrive in telegram
- offset is persisted to `data/telegram.offset`
- per-session transcript is persisted to
  `data/users/telegram-<chat_id>/telegram.transcript.json` (or your configured `users_root`)
- planning uses a rolling context window of the last 50 turns per telegram session

## whatsapp quickstart

the host supports a native rust whatsapp channel using `whatsapp-rust` (not baileys).

required behavior:

- `auth_method = "md5"` in host config (`[whatsapp]`)
- terminal qr pairing on first login
- session persistence in sqlite under `whatsapp.session_dir`

config keys:

```toml
[whatsapp]
enabled = true
session_dir = "data/whatsapp"
auth_method = "md5"
qr_timeout_ms = 120000
allowlist = ["+15551234567"]
self_chat_enabled = false
```

env override:

- `WHATSAPP_SESSION_PATH` overrides `whatsapp.session_dir`

run:

```bash
cargo run -p ironclawd -- --whatsapp
```

first run prints a qr code in terminal.
scan from whatsapp mobile app:

1. settings
2. linked devices
3. link a device
4. scan terminal qr

dm flow:

- incoming whatsapp dm -> `ironclawd` session routing -> agent response -> whatsapp reply

## config

host config path resolution:

1) `IRONCLAWD_CONFIG` env var, or
2) `~/.config/ironclaw/ironclawd.toml`, or
3) fallback defaults

guest config path resolution:

- `IROWCLAW_CONFIG` env var, or
- `/mnt/brain/config/irowclaw.toml`

### execution modes

host supports these `execution_mode` values:

- `host_only`: host runs planning/tool loop and returns responses over websocket
- `guest_tools`: host routes websocket messages to guest; guest asks host for plan and executes
  allowlisted guest tools
- `guest_autonomous`: host is router/ui; guest performs planning + tool execution locally
- `auto`: defaults to `guest_tools` when firecracker is enabled, otherwise `host_only`

guest safety checks run in one place and are applied:

- before acting on inbound user text
- before and after each guest tool execution
- before returning tool or stream output back to host

## firecracker mode

firecracker mode is enabled when:

- host config sets `firecracker.enabled = true`, and
- `ironclawd` is built with `--features firecracker`

rootfs notes:

- `firecracker.rootfs_path` can be either:
  - an ext4 image path, or
  - a directory tree; if a directory, it is converted to an ext4 image at VM start
- the guest needs an executable `/init` and a guest binary at `/bin/irowclaw`.
  - See `rootfs/guest-skel/init` for a minimal init script that enables vsock.

smoke:

- build guest rootfs dir + ext4 image:
  - `scripts/build-guest-rootfs.sh`
- run firecracker + host/guest vsock roundtrip smoke:
  - `scripts/smoke-firecracker.sh`
- run websocket end-to-end smoke through ironclawd + guest:
  - `scripts/smoke-firecracker-ws.sh`
  - this path validates websocket -> host daemon -> guest over vsock
    -> websocket response
  - prints `PASS` with a guest-derived `streamdelta` line on success

additional websocket smoke for guest tool + safety policy:

- `scripts/smoke-guest-tools-ws.sh`
- validates:
  - guest `file_write`/`file_read` via `ToolCallRequest`/`ToolCallResponse`
  - prompt injection block path
  - leak detector block for `fake_secret_` pattern

current status:

- VM start/stop is wired via the `znskr-firecracker` crate.
- Host side vsock accept and guest side vsock connect are implemented.
- guest rootfs build now emits `rootfs/build/guest-rootfs.ext4` with `/init` + `/bin/irowclaw`.
