# Sandbox backends (Firecracker, WSL2, macOS)

Every agent runs its tools inside an isolated sandbox. Today the only real
sandbox is Firecracker (Linux + KVM); this document explains the seam that makes
sandboxing pluggable and how **WSL2** (Windows) and **Apple Virtualization**
(macOS) backends slot in.

## The seam: `VmManager`

All sandboxing goes through one trait (`crates/common/src/firecracker.rs`):

```rust
#[async_trait]
pub trait VmManager: Send + Sync {
    async fn start_vm(&self, config: VmConfig) -> Result<VmInstance, VmError>;
    async fn stop_vm(&self, user_id: &str) -> Result<(), VmError>;
    async fn stop_all(&self) -> Result<(), VmError>;
    async fn is_vm_running(&self, user_id: &str) -> Result<bool, VmError>;
}

pub struct VmConfig   { pub user_id: String, pub brain_path: PathBuf, pub allowed_domains: Vec<String> }
pub struct VmInstance { pub user_id: String, pub brain_path: PathBuf, pub transport: Box<dyn Transport>, pub allowed_tools: Vec<String> }
```

Two implementations exist: `FirecrackerManager` (feature `firecracker`) and
`StubVmManager` (host, no isolation — used for local/dev). The daemon picks one
at startup and stores it as `Arc<dyn VmManager>`. The control-plane endpoints
`POST /api/farm/agents/{id}/boot`, `POST /api/farm/agents/{id}/stop`, and
`GET /api/farm/vms` (surfaced in the GPUI workspace as Boot/Stop buttons) call
straight through this trait, so **any backend that implements `VmManager` works
with no changes to the API or UI**.

Two invariants a backend must uphold:

1. **Transport.** The guest runs the same `irowclaw` agent and speaks
   length-prefixed protobuf (`crates/common/proto/ironclaw.proto`) over a
   `Transport`. Firecracker uses virtio **vsock**; a backend just has to hand
   back *some* `Box<dyn Transport>` connected to the guest process.
2. **Per-agent isolation + persistence.** `brain_path` is the agent's private,
   persistent disk (`workspace/`, memory, schedules). A backend maps it into the
   guest read-write and keeps it isolated from other agents.

Selection is config-driven. Add a `[sandbox]` section:

```toml
[sandbox]
backend = "auto"   # auto | firecracker | wsl2 | apple-vz | host-stub
```

`auto` resolves to `firecracker` on Linux+KVM, `apple-vz` on macOS,
`wsl2` on Windows, else `host-stub`. The active value is already reported in the
`backend` field of `/api/farm/vms`.

---

## WSL2 backend (Windows)

WSL2 is a real lightweight utility VM (Hyper-V) running a Microsoft Linux
kernel; each *distro* is an isolated Linux userspace on that kernel. Model each
agent as its own imported distro instance.

- **Image / persistence.** Ship the same Ubuntu rootfs we build for Firecracker
  as a tarball. On first boot, `wsl --import ironclaw-<agent> <state_dir>
  base.tar --version 2`. The per-agent `<state_dir>` (a `.vhdx`) is the isolated,
  persistent disk; bind `brain_path` to `/mnt/brain` inside it.
- **Boot / stop / status.**
  - `start_vm`: import the distro if missing, then launch the guest agent:
    `wsl -d ironclaw-<agent> -u ironclaw -- /usr/local/bin/irowclaw`.
  - `stop_vm`: `wsl --terminate ironclaw-<agent>`.
  - `is_vm_running`: parse `wsl -l --running`.
- **Transport.** WSL2 supports **AF_VSOCK over Hyper-V sockets (HvSocket)**, so
  the existing vsock transport maps over almost unchanged — the host connects to
  the guest on a well-known vsock port. Fallbacks if HvSocket is awkward:
  (a) stdio pipes to the `wsl.exe` child, or (b) a TCP port on WSL2's
  localhost-forwarded loopback.
- **Networking / egress.** WSL2 gives NAT'd outbound by default; enforce the
  `allowed_domains` policy with `nftables` inside the distro (as on Firecracker)
  or a host proxy.
- **Isolation trade-off.** A per-agent distro is a real VM boundary but shares
  one WSL2 kernel across agents (unlike a fresh microVM per boot). For stronger
  separation, give each agent its own Hyper-V VM (heavier) or add Linux
  user/namespace/cgroup confinement inside the distro.
- **Requirements.** Windows 10/11 with WSL2 enabled, the base rootfs tarball,
  and `wsl.exe`. Gate with `#[cfg(target_os = "windows")]` + `--features wsl2`.

Skeleton:

```rust
#[cfg(all(target_os = "windows", feature = "wsl2"))]
pub struct Wsl2Manager { base_tar: PathBuf, state_root: PathBuf, vsock_port: u32 }

#[cfg(all(target_os = "windows", feature = "wsl2"))]
#[async_trait]
impl VmManager for Wsl2Manager {
    async fn start_vm(&self, cfg: VmConfig) -> Result<VmInstance, VmError> {
        self.ensure_imported(&cfg.user_id)?;                 // wsl --import (once)
        self.launch_guest(&cfg.user_id, &cfg.brain_path)?;   // wsl -d ... -- irowclaw
        let transport = self.connect_hvsocket(&cfg.user_id)?; // AF_VSOCK/HvSocket
        Ok(VmInstance { user_id: cfg.user_id, brain_path: cfg.brain_path,
                        transport: Box::new(transport), allowed_tools: vec![] })
    }
    // stop_vm -> `wsl --terminate`, is_vm_running -> parse `wsl -l --running`
}
```

---

## macOS backend

macOS has no KVM/Firecracker, so there are two viable strategies.

### Primary: Apple Virtualization.framework (VZ)

The direct analog to Firecracker: a real, lightweight per-agent Linux VM.

- **VM.** `VZVirtualMachine` + `VZLinuxBootLoader` (our `vmlinux` + initrd) +
  `VZDiskImageStorageDeviceAttachment` pointing at the agent's ext4 rootfs
  (`brain_path` as a second attached disk for persistence).
- **Transport.** VZ exposes **virtio-vsock** via `VZVirtioSocketDevice`, so the
  vsock transport maps ~1:1 with Firecracker — the biggest reason to prefer VZ.
- **Networking.** `VZNATNetworkDeviceAttachment` for NAT'd egress; enforce
  `allowed_domains` in-guest as elsewhere.
- **Lifecycle.** `start()`/`stop()` on the VM object; `is_vm_running` tracks the
  VZ VM state. One VM per agent, keyed by `user_id`.
- **Requirements.** The `com.apple.security.virtualization` entitlement, a
  Linux kernel + rootfs (ARM64 on Apple Silicon), and VZ bindings — either the
  `objc2`/`objc2-virtualization` crates or a tiny Obj-C/Swift helper that the
  Rust manager drives. Gate with `#[cfg(target_os = "macos")]` +
  `--features apple-vz`.

```rust
#[cfg(all(target_os = "macos", feature = "apple-vz"))]
pub struct AppleVzManager { kernel: PathBuf, base_rootfs: PathBuf,
                            vms: Mutex<HashMap<String, VZVirtualMachine>> }
// start_vm: build VZVirtualMachineConfiguration (bootloader + rootfs disk +
// brain disk + vsock + NAT), start(), then open a vsock connection for Transport.
```

### Fallback: `sandbox-exec` (Seatbelt) process backend

Where VZ entitlements aren't available (e.g. plain dev machines), run the guest
agent as a host process confined by a macOS Seatbelt profile:

- Launch `sandbox-exec -f ironclaw.sb /usr/local/bin/irowclaw` with a profile
  that denies all but the agent's per-agent working dir and required syscalls.
- Transport over a unix-domain socket or stdio; `brain_path` is the allowed
  writable directory.
- Weaker than a VM (shared kernel, same OS user), so it is a dev/CI fallback,
  not a multi-tenant boundary.

---

## Summary

| Backend      | Boundary                     | Transport          | Availability |
| ------------ | ---------------------------- | ------------------ | ------------ |
| firecracker  | microVM per boot (KVM)       | virtio-vsock       | Linux + KVM  |
| apple-vz     | Linux VM per agent (VZ)      | virtio-vsock       | macOS        |
| wsl2         | WSL2 distro per agent        | HvSocket / stdio   | Windows      |
| host-stub    | none (in-process)            | in-memory pipe     | any (dev)    |

All backends run the **same guest agent and wire protocol** and implement the
**same `VmManager` trait**, so `/api/farm/*` and the workspace UI (including the
Boot/Stop controls) are identical across platforms. Implementing WSL2 and VZ is
therefore additive: build the backend struct, implement four async methods, and
register it in the startup selector — no API, protocol, or UI changes required.
