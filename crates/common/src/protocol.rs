use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MessageEnvelope {
    pub user_id: String,
    pub session_id: String,
    pub msg_id: u64,
    pub timestamp_ms: u64,
    pub payload: MessagePayload,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum MessagePayload {
    UserMessage { text: String },
    ToolResult { tool: String, output: String, ok: bool },
    StreamDelta { delta: String, done: bool },
    JobTrigger { job_id: String },
    JobStatus { job_id: String, status: String },
    FileOpRequest { op: String, path: String, data: Option<String> },
}

#[derive(Clone, Debug)]
pub struct FrameCodec {
    buffer: Vec<u8>,
}

impl FrameCodec {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    pub fn encode(message: &MessageEnvelope) -> Result<Vec<u8>, ProtocolError> {
        let payload = serde_json::to_vec(message).map_err(ProtocolError::serialize)?;
        let len = u32::try_from(payload.len()).map_err(ProtocolError::length)?;
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&payload);
        Ok(out)
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    pub fn next_frame(&mut self) -> Result<Option<MessageEnvelope>, ProtocolError> {
        if self.buffer.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes(
            self.buffer[0..4]
                .try_into()
                .map_err(|_| ProtocolError::length(()))?,
        ) as usize;
        if self.buffer.len() < 4 + len {
            return Ok(None);
        }
        let payload = self.buffer[4..4 + len].to_vec();
        self.buffer.drain(0..4 + len);
        let message = serde_json::from_slice(&payload).map_err(ProtocolError::deserialize)?;
        Ok(Some(message))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("protocol error: {message}")]
pub struct ProtocolError {
    message: String,
}

impl ProtocolError {
    fn serialize(err: serde_json::Error) -> Self {
        Self {
            message: format!("serialize failed: {err}"),
        }
    }

    fn deserialize(err: serde_json::Error) -> Self {
        Self {
            message: format!("deserialize failed: {err}"),
        }
    }

    fn length<T>(_err: T) -> Self {
        Self {
            message: "frame length error".to_string(),
        }
    }
}
