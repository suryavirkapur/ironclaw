//! WSL2 sandbox backend (Windows).
//!
//! Each agent is its own imported WSL2 distro instance (a real lightweight
//! Hyper-V VM sharing the WSL2 kernel). This manager orchestrates the distro
//! lifecycle via `wsl.exe`. It is gated behind the `wsl2` feature; it only does
//! anything useful on Windows where `wsl.exe` and a base rootfs are present,
//! but it is written with portable `std` so it compile-checks anywhere.
//!
//! Transport note: the recommended transport is AF_VSOCK over Hyper-V sockets
//! (HvSocket); a stdio/TCP transport is a simpler fallback. Wiring the transport
//! requires Windows-specific socket code and is intentionally left as the
//! integration point (`connect_transport`).

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use crate::firecracker::{VmConfig, VmError, VmInstance, VmManager};
use crate::transport::{LocalTransport, Transport};

pub struct Wsl2Manager {
    /// Base rootfs tarball used to `wsl --import` a fresh per-agent distro.
    base_tar: PathBuf,
    /// Directory under which each agent's distro state (`.vhdx`) is stored.
    state_root: PathBuf,
    /// Distros known to have been imported (best-effort cache).
    imported: Arc<Mutex<std::collections::HashSet<String>>>,
    buffer: usize,
}

impl Wsl2Manager {
    pub fn new(base_tar: PathBuf, state_root: PathBuf) -> Self {
        Self {
            base_tar,
            state_root,
            imported: Arc::new(Mutex::new(std::collections::HashSet::new())),
            buffer: 32,
        }
    }

    fn distro(agent_id: &str) -> String {
        format!("ironclaw-{agent_id}")
    }

    fn wsl(args: &[&str]) -> Result<std::process::Output, VmError> {
        Command::new("wsl.exe")
            .args(args)
            .output()
            .map_err(|err| VmError::new(format!("wsl.exe failed: {err}")))
    }

    fn ensure_imported(&self, agent_id: &str) -> Result<(), VmError> {
        let distro = Self::distro(agent_id);
        if self.imported.lock().map(|s| s.contains(&distro)).unwrap_or(false) {
            return Ok(());
        }
        let dir = self.state_root.join(&distro);
        let _ = std::fs::create_dir_all(&dir);
        // `wsl --import <distro> <dir> <base.tar> --version 2`
        let out = Self::wsl(&[
            "--import",
            &distro,
            &dir.to_string_lossy(),
            &self.base_tar.to_string_lossy(),
            "--version",
            "2",
        ])?;
        // Import fails if it already exists; treat that as success.
        if out.status.success() || String::from_utf8_lossy(&out.stderr).contains("already exists") {
            if let Ok(mut set) = self.imported.lock() {
                set.insert(distro);
            }
            Ok(())
        } else {
            Err(VmError::new(format!(
                "wsl --import failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )))
        }
    }

    /// Integration point: connect a transport to the guest agent running in the
    /// distro (AF_VSOCK/HvSocket on Windows, or stdio/TCP fallback).
    fn connect_transport(&self) -> Box<dyn Transport> {
        let (host, _guest) = LocalTransport::pair(self.buffer);
        Box::new(host)
    }
}

#[async_trait::async_trait]
impl VmManager for Wsl2Manager {
    async fn start_vm(&self, config: VmConfig) -> Result<VmInstance, VmError> {
        self.ensure_imported(&config.user_id)?;
        let distro = Self::distro(&config.user_id);
        // Launch the guest agent inside the distro. `--cd` sets the working dir;
        // the brain is bind-mounted / accessed under /mnt/brain by the guest.
        Self::wsl(&[
            "-d",
            &distro,
            "-u",
            "ironclaw",
            "--",
            "/usr/local/bin/irowclaw",
        ])?;
        Ok(VmInstance {
            user_id: config.user_id,
            brain_path: config.brain_path,
            transport: self.connect_transport(),
            allowed_tools: vec![],
        })
    }

    async fn stop_vm(&self, user_id: &str) -> Result<(), VmError> {
        let distro = Self::distro(user_id);
        Self::wsl(&["--terminate", &distro])?;
        Ok(())
    }

    async fn stop_all(&self) -> Result<(), VmError> {
        // Shut down the whole WSL2 lightweight VM (all distros).
        Self::wsl(&["--shutdown"])?;
        Ok(())
    }

    async fn is_vm_running(&self, user_id: &str) -> Result<bool, VmError> {
        let distro = Self::distro(user_id);
        let out = Self::wsl(&["-l", "--running", "--quiet"])?;
        // `wsl` emits UTF-16LE; decode leniently and check for the distro name.
        let text = String::from_utf8_lossy(&out.stdout);
        let running = text.lines().any(|line| line.trim() == distro)
            || decode_utf16le(&out.stdout).lines().any(|line| line.trim() == distro);
        Ok(running)
    }
}

/// Best-effort UTF-16LE decode (wsl.exe output encoding on Windows).
fn decode_utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}
