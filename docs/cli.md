# Ironclaw CLI

The CLI connects to `ironclawd` over its local WebSocket and uses the same authenticated
daemon → Firecracker → guest-agent route as messaging channels. It rejects a daemon that
reports `host_only` execution.

## Start the Firecracker daemon

Build or refresh the guest image first:

```bash
./scripts/build-ubuntu-rootfs.sh
```

The image is Ubuntu 24.04 and starts `irowclaw` as uid 0. The agent can use `apt`,
Python, and normal Linux tooling inside the microVM. Firecracker TAP networking requires
`CAP_NET_ADMIN`; run the daemon as root or grant that capability through your service
manager. The filesystem holding `data/users` must support reflinks (Btrfs or suitably
configured XFS); Ironclaw refuses to fall back to a shared writable base or full mutable
copy. Then, in the first terminal:

```bash
set -a
source .env
set +a
sudo --preserve-env=OPENROUTER_API_KEY \
  IRONCLAWD_CONFIG=configs/ironclawd.cli.toml \
  cargo +nightly-2025-12-26 run -p ironclawd --features firecracker
```

## Verify the sandbox

In a second terminal, run the deterministic doctor. It authenticates, boots the guest,
writes a unique marker inside the guest workspace, reads it back, and fails non-zero if
any stage does not work:

```bash
cargo +nightly-2025-12-26 run -p ironclaw-cli -- doctor
```

This check does not call the LLM, so it isolates daemon, WebSocket, Firecracker, vsock,
guest runtime, authentication, and sandboxed file I/O failures.

## Chat

```bash
cargo +nightly-2025-12-26 run -p ironclaw-cli -- chat
```

Use `/doctor` inside a chat to recheck the sandbox, `/file <PATH>` to upload a local file
for analysis, and `/quit` to exit. Normal messages use the configured LLM while tool
execution remains inside Firecracker.

For a one-shot file request:

```bash
cargo +nightly-2025-12-26 run -p ironclaw-cli -- \
  ask --file ./draft.tex "Review the structure and find broken references"
```

PDF, TeX, source, archive, and binary files share the same generic upload path. Uploads
are limited to 8 MiB and written under `workspace/uploads` inside the user's Firecracker
disk. File bytes are not parsed on the host.

Generated images and documents sent with `publish_artifact` are saved under the local,
ignored `artifacts/` directory.

One message may use several tools. Each result is returned to the planner as an
observation, allowing it to verify work or repair a failed step. The native tool loop
continues until it produces an answer, artifact, or real tool/provider error.

## Per-user root disks

`build-ubuntu-rootfs.sh` checksum-stamps the base image and changes it to mode `0444`.
At first boot, Ironclaw creates a mode `0600` copy-on-write root disk under that user's
ignored runtime directory. Package installs, schedules, tools, and workspace files persist
only in that disk. WebSocket closure sends a guest shutdown command, syncs and remounts
ext4 read-only, then powers off the microVM.

Existing users remain pinned to the base checksum recorded when their disk was created.
If the base is rebuilt, Ironclaw preserves the existing user disk and logs a version
warning; upgrading or resetting it must be an explicit, backed-up migration.

For scripts or CI, send one message and exit:

```bash
cargo +nightly-2025-12-26 run -p ironclaw-cli -- \
  ask "Reply with exactly: ironclaw is ready"
```

After installing the binary with `cargo install --path crates/ironclaw-cli`, the commands
are simply `ironclaw doctor`, `ironclaw chat`, and `ironclaw ask "..."`.

## Scheduler and live weather

Inside `ironclaw chat`, scheduling requests use Ironclaw's own `schedule_job` tool. They
do not depend on an OS `crontab`:

```text
Schedule a job named heartbeat with schedule * * * * * that runs date +%s > /mnt/brain/workspace/heartbeat.txt.
```

Current Dubai and Delhi weather uses a live request to `api.open-meteo.com` from the
Firecracker guest. Redirects, unsupported cities, network errors, and malformed responses
fail closed with no cached or model-knowledge fallback.

The root-agent profile has unrestricted outbound TAP/NAT connectivity. Firecracker is the
machine boundary, not an egress firewall; apply host firewall rules if deployment policy
requires destination restrictions.
