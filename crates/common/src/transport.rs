use crate::codec::ProtoCodec;
use crate::proto::ironclaw::MessageEnvelope;
use async_trait::async_trait;
use tokio::sync::mpsc;

#[async_trait]
pub trait Transport: Send {
    async fn send(&mut self, message: MessageEnvelope) -> Result<(), TransportError>;
    async fn recv(&mut self) -> Result<Option<MessageEnvelope>, TransportError>;
}

#[derive(Debug, thiserror::Error)]
#[error("transport error: {message}")]
pub struct TransportError {
    message: String,
}

impl TransportError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub struct LocalTransport {
    tx: mpsc::Sender<Vec<u8>>,
    rx: mpsc::Receiver<Vec<u8>>,
    codec: ProtoCodec,
}

impl LocalTransport {
    pub fn pair(buffer: usize) -> (Self, Self) {
        let (a_tx, a_rx) = mpsc::channel(buffer);
        let (b_tx, b_rx) = mpsc::channel(buffer);
        let a = Self {
            tx: a_tx,
            rx: b_rx,
            codec: ProtoCodec::new(),
        };
        let b = Self {
            tx: b_tx,
            rx: a_rx,
            codec: ProtoCodec::new(),
        };
        (a, b)
    }
}

#[async_trait]
impl Transport for LocalTransport {
    async fn send(&mut self, message: MessageEnvelope) -> Result<(), TransportError> {
        let bytes =
            ProtoCodec::encode(&message).map_err(|err| TransportError::new(err.to_string()))?;
        self.tx
            .send(bytes)
            .await
            .map_err(|_| TransportError::new("send failed"))
    }

    async fn recv(&mut self) -> Result<Option<MessageEnvelope>, TransportError> {
        loop {
            if let Some(frame) = self.codec.next_frame().map_err(map_codec_error)? {
                return Ok(Some(frame));
            }
            match self.rx.recv().await {
                Some(bytes) => {
                    self.codec.push_bytes(&bytes);
                }
                None => return Ok(None),
            }
        }
    }
}

fn map_codec_error(err: crate::codec::CodecError) -> TransportError {
    TransportError::new(err.to_string())
}
