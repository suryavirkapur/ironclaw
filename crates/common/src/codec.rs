use crate::proto::ironclaw::MessageEnvelope;
use prost::Message;

#[derive(Clone, Debug, Default)]
pub struct ProtoCodec {
    buffer: Vec<u8>,
}

impl ProtoCodec {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    pub fn encode(message: &MessageEnvelope) -> Result<Vec<u8>, CodecError> {
        let mut payload = Vec::with_capacity(message.encoded_len());
        message
            .encode(&mut payload)
            .map_err(|e| CodecError::new(format!("encode failed: {e}")))?;

        let len =
            u32::try_from(payload.len()).map_err(|_| CodecError::new("frame length overflow"))?;
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&payload);
        Ok(out)
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    pub fn next_frame(&mut self) -> Result<Option<MessageEnvelope>, CodecError> {
        if self.buffer.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes(self.buffer[0..4].try_into().unwrap()) as usize;
        if self.buffer.len() < 4 + len {
            return Ok(None);
        }
        let payload = self.buffer[4..4 + len].to_vec();
        self.buffer.drain(0..4 + len);

        let message = MessageEnvelope::decode(payload.as_slice())
            .map_err(|e| CodecError::new(format!("decode failed: {e}")))?;
        Ok(Some(message))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("codec error: {message}")]
pub struct CodecError {
    message: String,
}

impl CodecError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
