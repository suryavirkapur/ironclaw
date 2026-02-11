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

        if vsock_uds_path.exists() {
            let _ = std::fs::remove_file(&vsock_uds_path);
        }

        let mut builder = znskr_firecracker::runtime::builder::MicroVmBuilder::<
            znskr_firecracker::network::slirp::SlirpNetBackend,
        >::new()
        .firecracker_bin(&self.config.firecracker_bin)
        .kernel(&self.config.kernel_path)
        .api_socket(&api_socket)
        .vm_id(user_id.clone())
        .vsock(3, &vsock_uds_path);

        if self.config.rootfs_path.is_dir() {
            builder = builder.rootfs_dir(&self.config.rootfs_path);
        } else {
            builder = builder.rootfs(&self.config.rootfs_path);
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
