use crate::transport::{LocalTransport, Transport};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct VmConfig {
    pub user_id: String,
    pub brain_path: PathBuf,
}

pub struct VmInstance {
    pub user_id: String,
    pub brain_path: PathBuf,
    pub transport: Box<dyn Transport>,
}

#[async_trait::async_trait]
pub trait VmManager: Send + Sync {
    async fn start_vm(&self, config: VmConfig) -> Result<VmInstance, VmError>;
    async fn stop_vm(&self, user_id: &str) -> Result<(), VmError>;
}

#[derive(Debug, thiserror::Error)]
#[error("vm error: {message}")]
pub struct VmError {
    message: String,
}

impl VmError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub struct StubVmManager {
    buffer: usize,
}

impl StubVmManager {
    pub fn new(buffer: usize) -> Self {
        Self { buffer }
    }

    pub fn make_transport_pair(&self) -> (LocalTransport, LocalTransport) {
        LocalTransport::pair(self.buffer)
    }

    pub fn start_vm_with_guest(
        &self,
        config: VmConfig,
    ) -> Result<(VmInstance, LocalTransport), VmError> {
        let (host_transport, guest_transport) = self.make_transport_pair();
        let instance = VmInstance {
            user_id: config.user_id,
            brain_path: config.brain_path,
            transport: Box::new(host_transport),
        };
        Ok((instance, guest_transport))
    }
}

#[async_trait::async_trait]
impl VmManager for StubVmManager {
    async fn start_vm(&self, config: VmConfig) -> Result<VmInstance, VmError> {
        let (host_transport, _guest_transport) = self.make_transport_pair();
        Ok(VmInstance {
            user_id: config.user_id,
            brain_path: config.brain_path,
            transport: Box::new(host_transport),
        })
    }

    async fn stop_vm(&self, _user_id: &str) -> Result<(), VmError> {
        Ok(())
    }
}

#[cfg(feature = "firecracker")]
use std::collections::HashMap;
#[cfg(feature = "firecracker")]
use std::sync::Arc;
#[cfg(feature = "firecracker")]
use tokio::sync::Mutex;

#[cfg(feature = "firecracker")]
pub struct FirecrackerManager {
    config: FirecrackerManagerConfig,
    handles: Arc<Mutex<HashMap<String, znskr_firecracker::runtime::handle::MicroVmHandle>>>,
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
}

#[cfg(feature = "firecracker")]
impl FirecrackerManager {
    pub fn new(config: FirecrackerManagerConfig) -> Self {
        Self {
            config,
            handles: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[cfg(feature = "firecracker")]
pub fn default_vsock_port() -> u32 {
    5000
}

#[cfg(feature = "firecracker")]
#[async_trait::async_trait]
impl VmManager for FirecrackerManager {
    async fn start_vm(&self, config: VmConfig) -> Result<VmInstance, VmError> {
        let user_id = config.user_id;

        std::fs::create_dir_all(&self.config.api_socket_dir)
            .map_err(|e| VmError::new(format!("create api socket dir failed: {e}")))?;
        std::fs::create_dir_all(&self.config.vsock_uds_dir)
            .map_err(|e| VmError::new(format!("create vsock uds dir failed: {e}")))?;

        // If one already exists, stop it first.
        let existing = { self.handles.lock().await.remove(&user_id) };
        if let Some(mut handle) = existing {
            let _ = handle.shutdown().await;
            let _ = handle.kill().await;
        }

        let api_socket = self.config.api_socket_dir.join(format!("{user_id}.sock"));
        let vsock_uds_path = self
            .config
            .vsock_uds_dir
            .join(format!("{user_id}.vsock.sock"));

        let listener = tokio::net::UnixListener::bind(&vsock_uds_path)
            .map_err(|e| VmError::new(format!("bind vsock uds failed: {e}")))?;

        let mut builder = znskr_firecracker::runtime::builder::MicroVmBuilder::<
            znskr_firecracker::network::slirp::SlirpNetBackend,
        >::new()
        .firecracker_bin(&self.config.firecracker_bin)
        .kernel(&self.config.kernel_path)
        .api_socket(&api_socket)
        .vm_id(user_id.clone())
        .vsock(3, &vsock_uds_path)
        .network(znskr_firecracker::network::slirp::SlirpNetBackend::default());

        if self.config.rootfs_path.is_dir() {
            builder = builder.rootfs_dir(&self.config.rootfs_path);
        } else {
            builder = builder.rootfs(&self.config.rootfs_path);
        }

        let handle = builder
        .build_and_start()
        .await
        .map_err(|e| VmError::new(format!("firecracker start failed: {e}")))?;

        // Wait for the guest to connect to the host vsock listener (UDS endpoint).
        let (stream, _) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            listener.accept(),
        )
        .await
        .map_err(|_| VmError::new("vsock accept timed out"))?
        .map_err(|e| VmError::new(format!("vsock accept failed: {e}")))?;

        self.handles.lock().await.insert(user_id.clone(), handle);

        let transport = crate::stream_transport::StreamTransport::new(stream);

        Ok(VmInstance {
            user_id,
            brain_path: config.brain_path,
            transport: Box::new(transport),
        })
    }

    async fn stop_vm(&self, user_id: &str) -> Result<(), VmError> {
        let handle = { self.handles.lock().await.remove(user_id) };
        if let Some(mut handle) = handle {
            let _ = handle.shutdown().await;
            handle
                .kill()
                .await
                .map_err(|e| VmError::new(format!("firecracker kill failed: {e}")))?;
        }
        Ok(())
    }
}
