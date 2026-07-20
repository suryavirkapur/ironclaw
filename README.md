# ironclaw

Self-hosted AI agent platform with Firecracker VM isolation.

## Build
```bash
cargo +nightly-2025-12-26 build --release
```

## Run
```bash
cargo run -p ironclawd                    # Local mode
cargo run -p ironclawd --features firecracker  # VM mode
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
allowed_domains = ["api.openai.com", "pypi.org"]
```

Guest browser automation is gated by `allow_browser = true` in the guest config and requires
`agent-browser` plus a Chrome runtime to be installed wherever `irowclaw` runs.

## Rootfs (for Firecracker)
```bash
INCLUDE_PYTHON=1 INCLUDE_NODE=1 ./scripts/build-guest-rootfs.sh
```

## Tools
- `code_exec` - Execute Python/Node.js/Bash
- `tool_install` / `tool_call` - Custom tools
- `file_read` / `file_write` / `bash` / `browser` - Built-in

## Tests
```bash
cargo +nightly-2025-12-26 test  # 91 tests
```
