use crate::proto::ironclaw::{message_envelope, AuthAck, AuthChallenge, MessageEnvelope};
use crate::transport::{Transport, TransportError};
use async_trait::async_trait;

/// Wraps a transport with a capability token.
///
/// - Outbound messages get `cap_token` injected.
/// - Inbound messages are validated.
pub struct AuthTransport<T> {
    inner: T,
    cap_token: String,
}

impl<T> AuthTransport<T> {
    pub fn new(inner: T, cap_token: String) -> Self {
        Self { inner, cap_token }
    }

    pub fn cap_token(&self) -> &str {
        &self.cap_token
    }

    pub fn into_inner(self) -> T {
        self.inner
    }

    pub fn make_challenge(
        user_id: String,
        session_id: String,
        msg_id: u64,
        timestamp_ms: u64,
        cap_token: String,
        allowed_tools: Vec<String>,
    ) -> MessageEnvelope {
        MessageEnvelope {
            user_id,
            session_id,
            msg_id,
            timestamp_ms,
            cap_token: cap_token.clone(),
            payload: Some(message_envelope::Payload::AuthChallenge(AuthChallenge {
                cap_token,
                allowed_tools,
            })),
        }
    }

    pub fn validate_inbound(&self, msg: &MessageEnvelope) -> Result<(), TransportError> {
        match msg.payload {
            Some(message_envelope::Payload::AuthAck(AuthAck { ref cap_token })) => {
                if cap_token != &self.cap_token {
                    return Err(TransportError::new("auth ack token mismatch"));
                }
                Ok(())
            }
            Some(message_envelope::Payload::AuthChallenge(_)) => {
                Err(TransportError::new("unexpected auth challenge from guest"))
            }
            _ => {
                if msg.cap_token != self.cap_token {
                    return Err(TransportError::new("cap token mismatch"));
                }
                Ok(())
            }
        }
    }
}

#[async_trait]
impl<T> Transport for AuthTransport<T>
where
    T: Transport + Send,
{
    async fn send(&mut self, mut message: MessageEnvelope) -> Result<(), TransportError> {
        message.cap_token = self.cap_token.clone();
        self.inner.send(message).await
    }

    async fn recv(&mut self) -> Result<Option<MessageEnvelope>, TransportError> {
        let msg = self.inner.recv().await?;
        if let Some(ref m) = msg {
            self.validate_inbound(m)?;
        }
        Ok(msg)
    }
}
