use common::config::GuestConfig;
use common::logging::{init_logging, LoggingConfig};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), irowclaw::runtime::IrowclawError> {
    let config_path = std::env::var("IROWCLAW_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/mnt/brain/config/irowclaw.toml"));
    let guest_config = load_guest_config_for_logging(&config_path);
    let logging_result = init_logging(LoggingConfig {
        level: guest_config.log_level,
        log_file: None,
        rotate_keep: 5,
        rotate_max_bytes: 10 * 1024 * 1024,
    });
    if let Err(err) = logging_result {
        eprintln!("irowclaw: logging init failed: {err}");
    }

    // Firecracker best path: guest connects to host over vsock.
    // Firecracker reserves CID 2 for the host.
    let use_vsock = std::env::var("IRONCLAW_VSOCK")
        .ok()
        .as_deref()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if use_vsock {
        let port = std::env::var("IRONCLAW_VSOCK_PORT")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(5000);

        tracing::info!("irowclaw: starting (vsock accept on port {port})");

        let transport = irowclaw::vsock_transport::VsockTransport::accept(port)
            .await
            .map_err(|e| irowclaw::runtime::IrowclawError::new(e.to_string()))?;

        tracing::info!("irowclaw: vsock accepted, entering loop");

        return irowclaw::runtime::run_with_transport(transport, config_path).await;
    }

    let transport = local_stdio_transport();
    irowclaw::runtime::run_with_transport(transport, config_path).await
}

fn local_stdio_transport() -> StdioTransport {
    StdioTransport::new()
}

struct StdioTransport {
    codec: common::codec::ProtoCodec,
}

impl StdioTransport {
    fn new() -> Self {
        Self {
            codec: common::codec::ProtoCodec::new(),
        }
    }
}

fn load_guest_config_for_logging(config_path: &std::path::Path) -> GuestConfig {
    match std::fs::read_to_string(config_path) {
        Ok(contents) => match toml::from_str::<GuestConfig>(&contents) {
            Ok(config) => config,
            Err(_) => GuestConfig::default(),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => GuestConfig::default(),
        Err(_) => GuestConfig::default(),
    }
}

#[async_trait::async_trait]
impl common::transport::Transport for StdioTransport {
    async fn send(
        &mut self,
        message: common::proto::ironclaw::MessageEnvelope,
    ) -> Result<(), common::transport::TransportError> {
        let bytes = common::codec::ProtoCodec::encode(&message)
            .map_err(|err| common::transport::TransportError::new(err.to_string()))?;
        use tokio::io::AsyncWriteExt;
        let mut stdout = tokio::io::stdout();
        stdout
            .write_all(&bytes)
            .await
            .map_err(|_| common::transport::TransportError::new("stdout write failed"))?;
        stdout
            .flush()
            .await
            .map_err(|_| common::transport::TransportError::new("stdout flush failed"))?;
        Ok(())
    }

    async fn recv(
        &mut self,
    ) -> Result<Option<common::proto::ironclaw::MessageEnvelope>, common::transport::TransportError>
    {
        use tokio::io::AsyncReadExt;
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 1024];
        let n = stdin
            .read(&mut buf)
            .await
            .map_err(|_| common::transport::TransportError::new("stdin read failed"))?;
        if n == 0 {
            return Ok(None);
        }
        self.codec.push_bytes(&buf[..n]);
        let frame = self
            .codec
            .next_frame()
            .map_err(|err| common::transport::TransportError::new(err.to_string()))?;
        Ok(frame)
    }
}
