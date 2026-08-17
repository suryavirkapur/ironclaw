# ironclaw

Self-hosted AI agents, each in its own Firecracker microVM.

Install the daemon and the CLI. The first time `ironclawd` starts and the host
looks usable, it writes a config home and then runs from that home.

## Install

Linux x86_64 with KVM. From a clone:

```bash
cargo install --path crates/ironclawd --features firecracker --locked
cargo install --path crates/ironclaw-cli --locked
```

That puts `ironclawd` and `ironclaw` on your PATH. You also need:

- [Firecracker](https://github.com/firecracker-microvm/firecracker/releases) as `firecracker`
- read/write access to `/dev/kvm`
- `CAP_NET_ADMIN` or root, so the daemon can create TAP devices
- a reflink-capable filesystem for guest disks (Btrfs, or XFS configured for reflinks)

Set an OpenRouter-compatible key (the default config calls `https://openrouter.ai/api/v1`):

```bash
export OPENAI_API_KEY=...
```

## First start

```bash
sudo --preserve-env=OPENAI_API_KEY ironclawd
```

If the default config is missing, this creates:

```text
~/.config/ironclaw/
  ironclawd.toml
  users/
  agents/assistant.agent.toml
  kernels/          # put vmlinux.bin here
  rootfs/           # put ubuntu-24.04.ext4 here
  run/
```

Then it reads `ironclawd.toml`. Paths in that file that are not absolute are
relative to `~/.config/ironclaw/`. Later starts only read; they do not rewrite
the file.

Place a Firecracker kernel at `kernels/vmlinux.bin` and an Ubuntu guest image at
`rootfs/ubuntu-24.04.ext4`. From this repository:

```bash
# after building the guest image in a source checkout
./scripts/build-ubuntu-rootfs.sh
cp kernels/firecracker/vmlinux-6.1.155.bin ~/.config/ironclaw/kernels/vmlinux.bin
cp rootfs/build/ubuntu-24.04.ext4 ~/.config/ironclaw/rootfs/ubuntu-24.04.ext4
```

Check the sandbox, then chat:

```bash
ironclaw doctor
ironclaw chat
```

The workspace is at [http://127.0.0.1:9938/ui](http://127.0.0.1:9938/ui).

To write the home and exit without listening:

```bash
ironclawd init
```

## Another config, another root

`--config FILE` (or `IRONCLAWD_CONFIG`) selects a different home. The directory
that contains `FILE` is the root for every relative path in it. If `FILE` does
not exist, Ironclaw writes the same layout there.

```bash
mkdir -p ~/teams/acme
sudo --preserve-env=OPENAI_API_KEY ironclawd --config ~/teams/acme/ironclawd.toml
```

That uses `~/teams/acme/users`, `~/teams/acme/agents`, `~/teams/acme/kernels`,
and so on. Keep one directory per deployment.

Repo examples work the same way: relative kernel, rootfs, data, and agent paths
are resolved from `configs/`, not from the current working directory.

```bash
sudo --preserve-env=OPENAI_API_KEY ironclawd --config configs/ironclawd.cli.toml
sudo --preserve-env=OPENAI_API_KEY ironclawd --config configs/ironclawd.farm.toml
```

## Multiple agents

Each `*.toml` file in the home's `agents/` directory is one agent, with its own
VM, workspace, and memory. The default home ships a single assistant. Turn the
farm on in `ironclawd.toml`:

```toml
[farm]
enabled = true
manifests_dir = "agents"
public_base_url = "http://127.0.0.1:9938"
entry_agent = "assistant"
```

Add another teammate next to `assistant.agent.toml` and restart:

```toml
id = "researcher"
name = "Ada"
role = "Researcher"
reports_to = "assistant"

[model]
provider = "openrouter"
model = "minimax/minimax-m2.5"

[a2a]
accept_from = ["assistant"]

[[skills]]
id = "research"
description = "Research a question and return a sourced brief."
```

Open [http://127.0.0.1:9938/ui](http://127.0.0.1:9938/ui) and pick an agent.
Delegation only happens when both `delegate_to` and `accept_from` allow it.

The five-person engineering demo is the same idea, with Telegram optional:

```bash
ironclawd --config configs/ironclawd.farm.toml
./scripts/run-engineering-team-demo.sh
```

Walkthrough: [demos/engineering-team/README.md](demos/engineering-team/README.md).
Control plane: [docs/agent-farm.md](docs/agent-farm.md). Telegram:
[docs/telegram_setup.md](docs/telegram_setup.md). CLI flags: [docs/cli.md](docs/cli.md).

## Tools

Two layers.

**Built-in guest tools** are native Rust inside `irowclaw` (`crates/tools`):

- `file_read` / `file_write`
- `bash` / `code_exec`
- `browser` (needs `BRAVE_API_KEY`)
- `schedule_job` / `list_jobs`
- `weather`
- `publish_artifact`

**Agent-owned custom tools are Wasm only.** Farm agents do not get `tool_install`
or `tool_call`. Compile a `.wasm` module, put it in that agent's tools
directory, and declare it:

```toml
[wasm]
tools_dir = "tools"

[[wasm_tools]]
id = "cluster_logs"
module = "cluster_logs.wasm"
description = "Normalize and cluster related log records."
```

The guest runs those modules with Wasmtime. There is no WASI and no host
imports. Input and output are UTF-8 JSON through:

```text
memory
ironclaw_alloc(len: i32) -> i32
ironclaw_run(input_ptr: i32, input_len: i32) -> i64
```

`ironclaw_run` packs `(output_ptr << 32) | output_len`. Example:
`cargo build --target wasm32-unknown-unknown --release`.

MCP (`mcp_call`) and other agents (`delegate_task` / `await_task`) stay
host-mediated.

Tools are the employee's job. Memory is the specialized knowledge they accumulate
inside their own VM. Every planner step and tool result is appended as an agentic
trace under `<users_root>/_farm/traces/<agent_id>/` so those trajectories can later
fine-tune a narrower model per role. See [docs/agent-farm.md](docs/agent-farm.md).

## Tests

```bash
cargo test --workspace
cargo test -p farm --features wasm-runtime
```
