use common::stream_transport::StreamTransport;
use common::transport::{Transport, TransportError};

pub struct VsockTransport {
    inner: StreamTransport<tokio_vsock::VsockStream>,
}

impl VsockTransport {
    /// Listen inside the guest and accept a single host-initiated connection.
    pub async fn accept(port: u32) -> Result<Self, TransportError> {
        use tokio_vsock::{VsockAddr, VsockListener, VMADDR_CID_ANY};

        let listener = VsockListener::bind(VsockAddr::new(VMADDR_CID_ANY, port))
            .map_err(|e| TransportError::new(format!("vsock bind failed: {e}")))?;

        let (stream, _peer) = listener
            .accept()
            .await
            .map_err(|e| TransportError::new(format!("vsock accept failed: {e}")))?;

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

#[cfg(test)]
#[path = "vsock_transport_test.rs"]
mod vsock_transport_test;
