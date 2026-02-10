use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), irowclaw::runtime::IrowclawError> {
    let config_path = std::env::var("IROWCLAW_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/mnt/brain/config/irowclaw.toml"));
    let transport = local_stdio_transport();
    irowclaw::runtime::run_with_transport(transport, config_path).await
}

fn local_stdio_transport() -> StdioTransport {
    StdioTransport::new()
}

struct StdioTransport {
    codec: common::protocol::FrameCodec,
}

impl StdioTransport {
    fn new() -> Self {
        Self {
            codec: common::protocol::FrameCodec::new(),
        }
    }
}

#[async_trait::async_trait]
impl common::transport::Transport for StdioTransport {
    async fn send(
        &mut self,
        message: common::protocol::MessageEnvelope,
    ) -> Result<(), common::transport::TransportError> {
        let bytes = common::protocol::FrameCodec::encode(&message)
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
    ) -> Result<Option<common::protocol::MessageEnvelope>, common::transport::TransportError> {
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
