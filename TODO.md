# TODO

## Done

- IronClaw scaffold created as a workspace with crates:
  - `ironclawd` (host daemon: Axum server, WebSocket handler, serves `/ui` from `include_dir`)
  - `irowclaw` (guest runtime stub with local transport hook)
  - `common` (config, protocol types, transport, VM manager interface)
  - `memory` (SQLite helpers, chunking, vector extension loader stub)
  - `tools` (tool stubs)
- Firecracker feature plumbing
  - `common` has optional path dependency on `znskr-firecracker` behind `common/firecracker` feature
  - `ironclawd` forwards `--features firecracker` to `common/firecracker`
- Firecracker integration (start/stop)
  - `FirecrackerManager` now starts a microVM via `znskr-firecracker` `MicroVmBuilder::build_and_start()`
  - Tracks handles by `user_id` and stops VMs via shutdown plus kill
- Repo cleanup
  - Added `.gitignore` to ignore `target/` (and `data/`)
  - Rewrote git history to remove previously committed `target/` artifacts (fresh root commit)
- Workspace builds
  - `cargo check` succeeds
  - `cargo check -p ironclawd --features firecracker` succeeds
- Published repo
  - Created GitHub repo and pushed: https://github.com/suryavirkapur/ironclaw

## Pending / Next

### Firecracker guest transport
- Implement a real `Transport` for Firecracker mode using vsock.
  - Host side UDS endpoint + framed stream transport is now implemented.
  - Remaining: guest side vsock client transport and guest bootstrapping so the guest actually connects.

### Host-guest protocol wiring
- Define how the guest runtime (`irowclaw`) boots and connects to host transport.
  - Guest side vsock client transport is implemented (connects to host CID 2, port 5000 by default).
  - Remaining: guest rootfs entrypoint/service must set `IRONCLAW_VSOCK=1` (and optionally `IRONCLAW_VSOCK_PORT`).
- Ensure websocket messages are bridged host <-> guest in Firecracker mode (end to end test).

### Firecracker VM spec
- Confirm required guest kernel/rootfs expectations.
  - `HostFirecrackerConfig` provides paths but we need a documented build/source for these artifacts.

### Warning cleanup
- Remove the unnecessary `unsafe` block warning in `crates/memory/src/lib.rs`.
- Address `dead_code` warning for `brain` field in `crates/irowclaw/src/runtime.rs` (either use it or remove it).

### Basic ops docs
- Add a short `README.md` with:
  - how to run `ironclawd`
  - config file path/env vars
  - how to enable Firecracker feature and required host deps
- Added helper scripts:
  - `scripts/build-guest-rootfs.sh` (builds a minimal guest rootfs directory tree)
  - `scripts/smoke-local-guest.sh` (local websocket smoke test)
