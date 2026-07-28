use crate::transport::{LocalTransport, Transport};
#[cfg(feature = "firecracker")]
use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(feature = "firecracker")]
use std::sync::Arc;
#[cfg(feature = "firecracker")]
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct VmConfig {
    pub user_id: String,
    pub brain_path: PathBuf,
    pub allowed_domains: Vec<String>,
}

pub struct VmInstance {
    pub user_id: String,
    pub brain_path: PathBuf,
    pub transport: Box<dyn Transport>,
    pub allowed_tools: Vec<String>,
}

#[async_trait::async_trait]
pub trait VmManager: Send + Sync {
    async fn start_vm(&self, config: VmConfig) -> Result<VmInstance, VmError>;
    async fn stop_vm(&self, user_id: &str) -> Result<(), VmError>;
    async fn stop_all(&self) -> Result<(), VmError>;
    async fn is_vm_running(&self, user_id: &str) -> Result<bool, VmError>;
}

#[derive(Debug)]
pub struct VmError {
    message: String,
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "vm error: {}", self.message)
    }
}

impl std::error::Error for VmError {}

impl VmError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub struct StubVmManager {
    buffer: usize,
    running_users: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

impl StubVmManager {
    pub fn new(buffer: usize) -> Self {
        Self {
            buffer,
            running_users: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
        }
    }

    pub fn make_transport_pair(&self) -> (LocalTransport, LocalTransport) {
        LocalTransport::pair(self.buffer)
    }

    pub fn start_vm_with_guest(
        &self,
        config: VmConfig,
    ) -> Result<(VmInstance, LocalTransport), VmError> {
        let user_id = config.user_id.clone();
        let (host_transport, guest_transport) = self.make_transport_pair();
        let instance = VmInstance {
            user_id: config.user_id,
            brain_path: config.brain_path,
            transport: Box::new(host_transport),
            allowed_tools: vec![],
        };
        if let Ok(mut running_users) = self.running_users.lock() {
            running_users.insert(user_id);
        }
        Ok((instance, guest_transport))
    }
}

#[async_trait::async_trait]
impl VmManager for StubVmManager {
    async fn start_vm(&self, config: VmConfig) -> Result<VmInstance, VmError> {
        self.running_users
            .lock()
            .map_err(|_| VmError::new("stub vm manager lock poisoned"))?
            .insert(config.user_id.clone());
        let (host_transport, _guest_transport) = self.make_transport_pair();
        Ok(VmInstance {
            user_id: config.user_id,
            brain_path: config.brain_path,
            transport: Box::new(host_transport),
            allowed_tools: vec![],
        })
    }

    async fn stop_vm(&self, user_id: &str) -> Result<(), VmError> {
        self.running_users
            .lock()
            .map_err(|_| VmError::new("stub vm manager lock poisoned"))?
            .remove(user_id);
        Ok(())
    }

    async fn stop_all(&self) -> Result<(), VmError> {
        self.running_users
            .lock()
            .map_err(|_| VmError::new("stub vm manager lock poisoned"))?
            .clear();
        Ok(())
    }

    async fn is_vm_running(&self, user_id: &str) -> Result<bool, VmError> {
        let running_users = self
            .running_users
            .lock()
            .map_err(|_| VmError::new("stub vm manager lock poisoned"))?;
        Ok(running_users.contains(user_id))
    }
}

#[cfg(feature = "firecracker")]
pub struct FirecrackerManager {
    config: FirecrackerManagerConfig,
    handles: Arc<Mutex<HashMap<String, znskr_firecracker::runtime::handle::MicroVmHandle>>>,
    storage_lock: Arc<Mutex<()>>,
}

#[cfg(feature = "firecracker")]
#[derive(Clone, Debug)]
pub struct FirecrackerManagerConfig {
    pub firecracker_bin: PathBuf,
    pub kernel_path: PathBuf,
    pub rootfs_path: PathBuf,
    pub api_socket_dir: PathBuf,
    /// Directory for Firecracker vsock UDS endpoints.
    pub vsock_uds_dir: PathBuf,
    /// Guest listens/connects on this vsock port.
    pub vsock_port: u32,
    /// Number of vCPUs for each VM.
    pub vcpus: u8,
    /// Memory in MiB for each VM.
    pub memory_mib: u32,
    /// Attach a TAP network device. Disable for offline workloads.
    pub enable_network: bool,
}

#[cfg(feature = "firecracker")]
impl FirecrackerManager {
    pub fn new(config: FirecrackerManagerConfig) -> Self {
        Self {
            config,
            handles: Arc::new(Mutex::new(HashMap::new())),
            storage_lock: Arc::new(Mutex::new(())),
        }
    }
}

#[cfg(feature = "firecracker")]
pub fn default_vsock_port() -> u32 {
    5000
}

#[cfg(feature = "firecracker")]
impl FirecrackerManager {
    async fn is_running_inner(&self, user_id: &str) -> bool {
        self.handles.lock().await.contains_key(user_id)
    }

    fn prepare_instance_rootfs(
        base_rootfs: &std::path::Path,
        brain_path: &std::path::Path,
    ) -> Result<PathBuf, VmError> {
        use std::os::unix::fs::PermissionsExt;

        let metadata = std::fs::metadata(base_rootfs)
            .map_err(|err| VmError::new(format!("base rootfs metadata failed: {err}")))?;
        if !metadata.is_file() {
            return Err(VmError::new("base rootfs must be a regular ext4 image"));
        }
        if metadata.permissions().mode() & 0o222 != 0 {
            return Err(VmError::new(format!(
                "base rootfs must be immutable (chmod 0444 {}); refusing to attach a writable base",
                base_rootfs.display()
            )));
        }

        let user_dir = brain_path
            .parent()
            .ok_or_else(|| VmError::new("brain path has no per-user parent"))?;
        let vm_dir = user_dir.join("vm");
        std::fs::create_dir_all(&vm_dir)
            .map_err(|err| VmError::new(format!("create per-user vm dir failed: {err}")))?;

        let instance_path = vm_dir.join("rootfs.ext4");
        let origin_path = vm_dir.join("rootfs.origin");
        let base_identity = Self::base_rootfs_identity(base_rootfs, &metadata)?;

        if instance_path.exists() {
            let origin = std::fs::read_to_string(&origin_path).map_err(|err| {
                VmError::new(format!(
                    "per-user rootfs exists without readable origin metadata: {err}; \
                     preserve or remove {} explicitly",
                    instance_path.display()
                ))
            })?;
            if origin.trim() != base_identity {
                tracing::warn!(
                    "per-user rootfs {} was created from a different base; preserving it",
                    instance_path.display()
                );
            }
            return Ok(instance_path);
        }
        if origin_path.exists() {
            return Err(VmError::new(format!(
                "rootfs origin metadata exists without {}; remove the incomplete metadata explicitly",
                instance_path.display()
            )));
        }

        let temp_suffix = format!("tmp-{}", std::process::id());
        let temp_instance = vm_dir.join(format!("rootfs.ext4.{temp_suffix}"));
        let temp_origin = vm_dir.join(format!("rootfs.origin.{temp_suffix}"));
        if temp_instance.exists() || temp_origin.exists() {
            return Err(VmError::new(format!(
                "stale rootfs preparation files exist under {}; remove them explicitly",
                vm_dir.display()
            )));
        }

        let prepare_result = (|| -> Result<(), VmError> {
            let copy = std::process::Command::new("cp")
                .arg("--reflink=always")
                .arg("--sparse=auto")
                .arg("--")
                .arg(base_rootfs)
                .arg(&temp_instance)
                .output()
                .map_err(|err| VmError::new(format!("rootfs clone command failed: {err}")))?;
            if !copy.status.success() {
                let stderr = String::from_utf8_lossy(&copy.stderr);
                return Err(VmError::new(format!(
                    "copy-on-write rootfs clone failed with status {}: {}; \
                     the per-user storage filesystem must support reflinks",
                    copy.status,
                    stderr.trim()
                )));
            }
            std::fs::set_permissions(&temp_instance, std::fs::Permissions::from_mode(0o600))
                .map_err(|err| {
                    VmError::new(format!("set instance rootfs permissions failed: {err}"))
                })?;
            std::fs::write(&temp_origin, format!("{base_identity}\n"))
                .map_err(|err| VmError::new(format!("write rootfs origin failed: {err}")))?;
            std::fs::rename(&temp_instance, &instance_path)
                .map_err(|err| VmError::new(format!("publish instance rootfs failed: {err}")))?;
            std::fs::rename(&temp_origin, &origin_path)
                .map_err(|err| VmError::new(format!("publish rootfs origin failed: {err}")))?;
            Ok(())
        })();
        if let Err(err) = prepare_result {
            let _ = std::fs::remove_file(&temp_instance);
            let _ = std::fs::remove_file(&temp_origin);
            let _ = std::fs::remove_file(&instance_path);
            return Err(err);
        }

        tracing::info!(
            "created isolated per-user rootfs {} from immutable base {}",
            instance_path.display(),
            base_rootfs.display()
        );
        Ok(instance_path)
    }

    fn base_rootfs_identity(
        base_rootfs: &std::path::Path,
        metadata: &std::fs::Metadata,
    ) -> Result<String, VmError> {
        let sidecar = PathBuf::from(format!("{}.sha256", base_rootfs.display()));
        if sidecar.exists() {
            let value = std::fs::read_to_string(&sidecar)
                .map_err(|err| VmError::new(format!("read rootfs checksum failed: {err}")))?;
            let hash = value.split_whitespace().next().unwrap_or_default();
            if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(VmError::new("base rootfs checksum sidecar is malformed"));
            }
            return Ok(format!("sha256:{hash}"));
        }

        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        Ok(format!("metadata:{}:{modified}", metadata.len()))
    }
}

#[cfg(feature = "firecracker")]
#[async_trait::async_trait]
impl VmManager for FirecrackerManager {
    async fn start_vm(&self, config: VmConfig) -> Result<VmInstance, VmError> {
        let user_id = config.user_id.clone();

        std::fs::create_dir_all(&self.config.api_socket_dir)
            .map_err(|e| VmError::new(format!("create api socket dir failed: {e}")))?;
        std::fs::create_dir_all(&self.config.vsock_uds_dir)
            .map_err(|e| VmError::new(format!("create vsock uds dir failed: {e}")))?;

        if let Err(e) = crate::network_firewall::setup_vm_network(&user_id, &config.allowed_domains)
        {
            tracing::warn!("failed to setup network firewall for {}: {}", user_id, e);
        }

        // If one already exists, stop it first.
        let existing = { self.handles.lock().await.remove(&user_id) };
        if let Some(mut handle) = existing {
            let _ = handle.shutdown().await;
            let _ = handle.kill().await;
        }

        let instance_rootfs = {
            let _storage_guard = self.storage_lock.lock().await;
            Self::prepare_instance_rootfs(&self.config.rootfs_path, &config.brain_path)?
        };

        let api_socket = self.config.api_socket_dir.join(format!("{user_id}.sock"));
        let vsock_uds_path = self
            .config
            .vsock_uds_dir
            .join(format!("{user_id}.vsock.sock"));

        if vsock_uds_path.exists() {
            let _ = std::fs::remove_file(&vsock_uds_path);
        }

        let mut builder = znskr_firecracker::runtime::builder::MicroVmBuilder::<
            znskr_firecracker::network::tap_backend::TapNetBackend,
        >::new()
        .firecracker_bin(&self.config.firecracker_bin)
        .kernel(&self.config.kernel_path)
        .api_socket(&api_socket)
        .vm_id(user_id.clone())
        .vsock(3, &vsock_uds_path)
        .vcpus(self.config.vcpus)
        .memory_mib(self.config.memory_mib)
        .rootfs(&instance_rootfs);
        if self.config.enable_network {
            builder =
                builder.network(znskr_firecracker::network::tap_backend::TapNetBackend::default());
        }

        let handle = builder
            .build_and_start()
            .await
            .map_err(|e| VmError::new(format!("firecracker start failed: {e}")))?;

        // Firecracker creates and listens on the host-side UDS endpoint for vsock.
        // Host-initiated connection flow (Firecracker docs):
        // - connect(uds_path)
        // - write: "CONNECT <port>\n"
        // - read:  "OK <host_port>\n" (must be consumed before we start framing)
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // The guest may not be listening yet when the VM first starts.
        // Firecracker will close the UDS connection if nobody is listening on the requested port.
        // So we retry the entire host-initiated handshake until we get an OK.
        let stream = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                // 1) connect to uds_path (wait for Firecracker to create it)
                let mut stream = loop {
                    match tokio::net::UnixStream::connect(&vsock_uds_path).await {
                        Ok(stream) => break stream,
                        Err(err) => {
                            if err.kind() == std::io::ErrorKind::NotFound
                                || err.kind() == std::io::ErrorKind::ConnectionRefused
                            {
                                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                                continue;
                            }
                            return Err(err);
                        }
                    }
                };

                // 2) send CONNECT
                let connect_cmd = format!("CONNECT {}\n", self.config.vsock_port);
                if let Err(_e) = stream.write_all(connect_cmd.as_bytes()).await {
                    // treat as transient
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
                let _ = stream.flush().await;

                // 3) read OK line
                let mut line = Vec::with_capacity(64);
                let mut buf = [0u8; 1];
                loop {
                    match stream.read(&mut buf).await {
                        Ok(0) => {
                            // guest likely not listening yet
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            break;
                        }
                        Ok(_) => {
                            line.push(buf[0]);
                            if buf[0] == b'\n' || line.len() > 256 {
                                break;
                            }
                        }
                        Err(_) => {
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            break;
                        }
                    }
                }

                let s = String::from_utf8_lossy(&line);
                if s.starts_with("OK ") {
                    return Ok(stream);
                }

                // Not acknowledged yet, retry.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await
        .map_err(|_| VmError::new("vsock connect timed out"))?
        .map_err(|e| VmError::new(format!("vsock connect failed: {e}")))?;

        self.handles.lock().await.insert(user_id.clone(), handle);

        let transport = crate::stream_transport::StreamTransport::new(stream);

        Ok(VmInstance {
            user_id,
            brain_path: config.brain_path,
            transport: Box::new(transport),
            allowed_tools: vec![],
        })
    }

    async fn stop_vm(&self, user_id: &str) -> Result<(), VmError> {
        let handle = { self.handles.lock().await.remove(user_id) };
        if let Some(mut handle) = handle {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let _ = handle.kill().await;
        }

        if let Err(e) = crate::network_firewall::cleanup_vm_network(user_id) {
            tracing::warn!("failed to cleanup network firewall for {}: {}", user_id, e);
        }

        if let Err(e) = crate::cgroup::cleanup_cgroup(user_id) {
            tracing::warn!("failed to cleanup cgroup for {}: {}", user_id, e);
        }

        Ok(())
    }

    async fn stop_all(&self) -> Result<(), VmError> {
        let handles = {
            let mut handles = self.handles.lock().await;
            handles.drain().collect::<Vec<_>>()
        };
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        for (user_id, mut handle) in handles {
            if let Err(err) = handle.kill().await {
                tracing::debug!("firecracker {} already stopped: {}", user_id, err);
            }
            if let Err(err) = crate::network_firewall::cleanup_vm_network(&user_id) {
                tracing::warn!(
                    "failed to cleanup network firewall for {}: {}",
                    user_id,
                    err
                );
            }
            if let Err(err) = crate::cgroup::cleanup_cgroup(&user_id) {
                tracing::warn!("failed to cleanup cgroup for {}: {}", user_id, err);
            }
        }
        Ok(())
    }

    async fn is_vm_running(&self, user_id: &str) -> Result<bool, VmError> {
        Ok(self.is_running_inner(user_id).await)
    }
}

#[cfg(all(test, feature = "firecracker"))]
mod tests {
    use super::FirecrackerManager;
    use std::os::unix::fs::PermissionsExt;

    fn test_root(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "ironclaw-rootfs-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ))
    }

    #[test]
    fn immutable_base_is_cloned_and_each_user_keeps_private_changes() {
        let root = test_root("isolated");
        let base = root.join("base.ext4");
        std::fs::create_dir_all(&root).expect("create test root");
        std::fs::write(&base, b"clean-base").expect("write base");
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o444))
            .expect("make base immutable");

        let alice_brain = root.join("alice/brain.ext4");
        let bob_brain = root.join("bob/brain.ext4");
        let alice =
            FirecrackerManager::prepare_instance_rootfs(&base, &alice_brain).expect("clone alice");
        let bob =
            FirecrackerManager::prepare_instance_rootfs(&base, &bob_brain).expect("clone bob");
        assert_ne!(alice, bob);
        assert_eq!(std::fs::read(&alice).expect("read alice"), b"clean-base");
        assert_eq!(std::fs::read(&bob).expect("read bob"), b"clean-base");

        std::fs::write(&alice, b"alice-private").expect("modify alice clone");
        let alice_reused =
            FirecrackerManager::prepare_instance_rootfs(&base, &alice_brain).expect("reuse alice");
        assert_eq!(alice_reused, alice);
        assert_eq!(
            std::fs::read(&alice_reused).expect("read alice private"),
            b"alice-private"
        );
        assert_eq!(std::fs::read(&bob).expect("read bob again"), b"clean-base");
        assert_eq!(
            std::fs::read(&base).expect("read immutable base"),
            b"clean-base"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn writable_base_is_rejected() {
        let root = test_root("writable");
        let base = root.join("base.ext4");
        std::fs::create_dir_all(&root).expect("create test root");
        std::fs::write(&base, b"mutable").expect("write base");
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o644))
            .expect("make base writable");

        let result =
            FirecrackerManager::prepare_instance_rootfs(&base, &root.join("user/brain.ext4"));
        assert!(result.is_err());
        assert!(result
            .expect_err("writable base should fail")
            .to_string()
            .contains("must be immutable"));

        let _ = std::fs::remove_dir_all(root);
    }
}
