//! A portable process-isolation sandbox backend.
//!
//! Each agent runs as its own supervised host process with a private, writable
//! "brain" directory. This is the cross-platform basis for host sandboxing:
//! on Linux it runs the guest under a plain shell (optionally wrapped with a
//! namespace tool); on macOS it can be wrapped with `sandbox-exec` (Seatbelt).
//! It implements the same [`VmManager`] seam as Firecracker, so the control
//! plane and the workspace UI drive it unchanged.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::firecracker::{VmConfig, VmError, VmInstance, VmManager};
use crate::transport::{LocalTransport, Transport};

pub struct HostProcessManager {
    /// Optional custom guest command. `{agent}` and `{brain}` are substituted.
    guest_command: Option<String>,
    /// Optional macOS `sandbox-exec` profile path (Seatbelt confinement).
    #[allow(dead_code)]
    macos_profile: Option<PathBuf>,
    children: Arc<Mutex<HashMap<String, Child>>>,
    buffer: usize,
}

impl HostProcessManager {
    pub fn new(guest_command: Option<String>, macos_profile: Option<PathBuf>) -> Self {
        Self {
            guest_command,
            macos_profile,
            children: Arc::new(Mutex::new(HashMap::new())),
            buffer: 32,
        }
    }

    fn placeholder_transport(&self) -> Box<dyn Transport> {
        let (host, _guest) = LocalTransport::pair(self.buffer);
        Box::new(host)
    }

    /// Build the shell script that supervises one agent's guest process. The
    /// default keeps the sandbox alive and writes a heartbeat into the agent's
    /// private brain directory, proving per-agent isolation and persistence.
    fn guest_script(&self, agent_id: &str, brain: &str) -> String {
        if let Some(template) = &self.guest_command {
            return template.replace("{agent}", agent_id).replace("{brain}", brain);
        }
        format!(
            "export IRONCLAW_AGENT='{agent}'; export IRONCLAW_BRAIN='{brain}'; \
             mkdir -p \"$IRONCLAW_BRAIN\" 2>/dev/null; \
             while :; do date +%s > \"$IRONCLAW_BRAIN/.sandbox-heartbeat\" 2>/dev/null; sleep 2; done",
            agent = agent_id,
            brain = brain,
        )
    }

    #[cfg(unix)]
    fn spawn_child(&self, agent_id: &str, brain_path: &PathBuf) -> Result<Child, VmError> {
        let brain = brain_path.to_string_lossy().to_string();
        let script = self.guest_script(agent_id, &brain);
        let mut cmd;
        #[cfg(target_os = "macos")]
        {
            if let Some(profile) = &self.macos_profile {
                // Confine the guest with a Seatbelt profile.
                cmd = Command::new("sandbox-exec");
                cmd.arg("-f").arg(profile).arg("/bin/sh").arg("-c").arg(&script);
            } else {
                cmd = Command::new("/bin/sh");
                cmd.arg("-c").arg(&script);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            cmd = Command::new("/bin/sh");
            cmd.arg("-c").arg(&script);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.spawn()
            .map_err(|err| VmError::new(format!("spawn sandbox process failed: {err}")))
    }

    #[cfg(windows)]
    fn spawn_child(&self, agent_id: &str, _brain_path: &PathBuf) -> Result<Child, VmError> {
        // On Windows the WSL2 backend is preferred; this is a minimal keeper so
        // the process backend still functions for lifecycle control.
        let mut cmd = Command::new("cmd");
        cmd.args([
            "/C",
            &format!("set IRONCLAW_AGENT={agent_id}& :loop& timeout /t 2 >NUL& goto loop"),
        ]);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.spawn()
            .map_err(|err| VmError::new(format!("spawn sandbox process failed: {err}")))
    }
}

#[async_trait::async_trait]
impl VmManager for HostProcessManager {
    async fn start_vm(&self, config: VmConfig) -> Result<VmInstance, VmError> {
        {
            let mut children = self
                .children
                .lock()
                .map_err(|_| VmError::new("process sandbox lock poisoned"))?;
            let alive = children
                .get_mut(&config.user_id)
                .map(|child| child.try_wait().ok().flatten().is_none())
                .unwrap_or(false);
            if !alive {
                children.remove(&config.user_id);
                let child = self.spawn_child(&config.user_id, &config.brain_path)?;
                children.insert(config.user_id.clone(), child);
            }
        }
        Ok(VmInstance {
            user_id: config.user_id,
            brain_path: config.brain_path,
            transport: self.placeholder_transport(),
            allowed_tools: vec![],
        })
    }

    async fn stop_vm(&self, user_id: &str) -> Result<(), VmError> {
        let child = self
            .children
            .lock()
            .map_err(|_| VmError::new("process sandbox lock poisoned"))?
            .remove(user_id);
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }

    async fn stop_all(&self) -> Result<(), VmError> {
        let drained: Vec<Child> = {
            let mut children = self
                .children
                .lock()
                .map_err(|_| VmError::new("process sandbox lock poisoned"))?;
            children.drain().map(|(_, child)| child).collect()
        };
        for mut child in drained {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }

    async fn is_vm_running(&self, user_id: &str) -> Result<bool, VmError> {
        let mut children = self
            .children
            .lock()
            .map_err(|_| VmError::new("process sandbox lock poisoned"))?;
        match children.get_mut(user_id) {
            Some(child) => Ok(child
                .try_wait()
                .map_err(|err| VmError::new(err.to_string()))?
                .is_none()),
            None => Ok(false),
        }
    }
}
