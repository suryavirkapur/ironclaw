# ironclaw

A small vertical slice scaffold for a host daemon plus a guest runtime.

## Crates

- `crates/ironclawd` - host daemon
  - Axum HTTP server
  - WebSocket bridge at `/ws`
  - Serves UI under `/ui`
- `crates/irowclaw` - guest runtime (currently stub)
- `crates/common` - shared config, protocol, transport, VM manager
- `crates/memory` - SQLite schema + indexing helpers
- `crates/tools` - tool stubs

## Build

```bash
cargo check
cargo check -p ironclawd --features firecracker
```

## Run (local guest mode)

Local guest mode is the default when Firecracker is disabled. In this mode, `ironclawd` uses an in-process transport pair and runs the guest runtime locally.

```bash
cargo run -p ironclawd
```

Then open a websocket connection to:

- `ws://127.0.0.1:3000/ws`

(Port and bind come from the host config.)

## Config

Host config path resolution:

1) `IRONCLAWD_CONFIG` env var, or
2) `~/.config/ironclaw/ironclawd.toml`, or
3) fallback defaults

Guest config path resolution:

- `IROWCLAW_CONFIG` env var, or
- `/mnt/brain/config/irowclaw.toml`

## Firecracker mode

Firecracker mode is enabled when:

- host config sets `firecracker.enabled = true`, and
- `ironclawd` is built with `--features firecracker`

Rootfs notes:

- `firecracker.rootfs_path` can be either:
  - an ext4 image path, or
  - a directory tree; if a directory, it is converted to an ext4 image at VM start
- The guest needs an executable `/init` and a guest binary at `/bin/irowclaw`.
  - See `rootfs/guest-skel/init` for a minimal init script that enables vsock.

Smoke:

- build guest rootfs dir + ext4 image:
  - `scripts/build-guest-rootfs.sh`
- run firecracker + host/guest vsock roundtrip smoke:
  - `scripts/smoke-firecracker.sh`

Current status:

- VM start/stop is wired via the `znskr-firecracker` crate.
- Host side vsock accept and guest side vsock connect are implemented.
- guest rootfs build now emits `rootfs/build/guest-rootfs.ext4` with `/init` + `/bin/irowclaw`.
