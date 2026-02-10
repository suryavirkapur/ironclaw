use common::stream_transport::StreamTransport;
use common::transport::{Transport, TransportError};

pub struct VsockTransport {
    inner: StreamTransport<tokio_vsock::VsockStream>,
}

impl VsockTransport {
    pub async fn connect(host_cid: u32, port: u32) -> Result<Self, TransportError> {
        let addr = tokio_vsock::VsockAddr::new(host_cid, port);
        let stream = tokio_vsock::VsockStream::connect(addr)
            .await
            .map_err(|e| TransportError::new(format!("vsock connect failed: {e}")))?;
        Ok(Self {
            inner: StreamTransport::new(stream),
        })
    }
}

#[async_trait::async_trait]
impl Transport for VsockTransport {
    async fn send(
        &mut self,
        message: common::proto::ironclaw::MessageEnvelope,
    ) -> Result<(), TransportError> {
        self.inner.send(message).await
    }

    async fn recv(
        &mut self,
    ) -> Result<Option<common::proto::ironclaw::MessageEnvelope>, TransportError> {
        self.inner.recv().await
    }
}
