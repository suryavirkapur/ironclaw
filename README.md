# ironclaw

Self-hosted AI agent platform with Firecracker VM isolation.

## Build
```bash
cargo +nightly-2025-12-26 build --release
```

## Run
```bash
cargo run -p ironclawd --features firecracker
```

Agent execution is Firecracker-only; the daemon refuses WebSocket agent sessions when
Firecracker is unavailable or when `host_only` execution is configured.

For an end-to-end Telegram test with all agent execution isolated in Firecracker, follow
[the Telegram setup guide](docs/telegram_setup.md).

To run the five-person Product Manager, Engineering Lead, Backend, Frontend, and QA demo through
Telegram and the web workspace, use the [engineering-team walkthrough](demos/engineering-team/README.md):

```bash
./scripts/run-engineering-team-demo.sh
```

For the faster local development loop, use the [Firecracker CLI](docs/cli.md):

```bash
cargo run -p ironclaw-cli -- doctor  # deterministic sandbox check
cargo run -p ironclaw-cli -- chat    # interactive agent chat
```

## Config (`~/.config/ironclaw/ironclawd.toml`)
```toml
execution_mode = "guest_tools"
[firecracker]
enabled = true
vcpus = 2
memory_mib = 2048
disk_quota_mb = 512
[security.network]
allowed_domains = []
```

The Ubuntu guest runs the agent as root and has direct TAP/NAT network access. The
`allowed_domains` list is intentionally empty in the supplied root-agent configs because
domain filtering is not enforced on this path. Database permissions and remote service
credentials must therefore provide their own least-privilege boundary.

Each user boots from an immutable, checksum-stamped Ubuntu base. Ironclaw creates a
private Btrfs/XFS reflink root disk for that user, so package installs and workspace data
persist without modifying or being shared through the base image.

## Rootfs (for Firecracker)
```bash
./scripts/build-ubuntu-rootfs.sh
```

## Tools
- `code_exec` - Execute Python/Node.js/Bash
- `tool_install` / `tool_call` - Custom tools
- `file_read` / `file_write` / `bash` / `browser` - Built-in
- `schedule_job` / `list_jobs` - Create and inspect guest scheduler jobs
- `weather` - Live, allowlisted Open-Meteo access for Dubai and Delhi
- `publish_artifact` - Return generated PNG/JPEG/SVG/PDF files to CLI or Telegram
- inbound files - Analyze owner-sent Telegram documents or CLI `ask --file` uploads inside Firecracker

Guest tool mode uses an observe–act–verify loop. A single request can execute and repair
multiple dependent tool calls before returning an answer or artifact.

The local PostgreSQL analyst and chart test is documented in
[docs/data_analyst_e2e.md](docs/data_analyst_e2e.md).

## Tests
```bash
cargo +nightly-2025-12-26 test --workspace
```
