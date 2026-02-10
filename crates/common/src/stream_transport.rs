use crate::codec::ProtoCodec;
use crate::proto::ironclaw::MessageEnvelope;
use crate::transport::{Transport, TransportError};
use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub struct StreamTransport<S> {
    stream: S,
    codec: ProtoCodec,
    read_buf: Vec<u8>,
}

impl<S> StreamTransport<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            codec: ProtoCodec::new(),
            read_buf: vec![0u8; 8192],
        }
    }

    pub fn into_inner(self) -> S {
        self.stream
    }
}

#[async_trait]
impl<S> Transport for StreamTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    async fn send(&mut self, message: MessageEnvelope) -> Result<(), TransportError> {
        let bytes = ProtoCodec::encode(&message)
            .map_err(|err| TransportError::new(err.to_string()))?;
        self.stream
            .write_all(&bytes)
            .await
            .map_err(|err| TransportError::new(format!("write failed: {err}")))?;
        self.stream
            .flush()
            .await
            .map_err(|err| TransportError::new(format!("flush failed: {err}")))?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Option<MessageEnvelope>, TransportError> {
        loop {
            if let Some(frame) = self
                .codec
                .next_frame()
                .map_err(|err| TransportError::new(err.to_string()))?
            {
                return Ok(Some(frame));
            }

            let n = self
                .stream
                .read(&mut self.read_buf)
                .await
                .map_err(|err| TransportError::new(format!("read failed: {err}")))?;
            if n == 0 {
                return Ok(None);
            }
            self.codec.push_bytes(&self.read_buf[..n]);
        }
    }
}
