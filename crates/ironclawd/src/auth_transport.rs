use common::proto::ironclaw::{message_envelope, MessageEnvelope};
use common::transport::{Transport, TransportError};

pub struct AuthenticatedTransport {
    inner: Box<dyn Transport>,
    cap_token: String,
}

impl AuthenticatedTransport {
    pub fn new(inner: Box<dyn Transport>, cap_token: String) -> Self {
        Self { inner, cap_token }
    }

    pub fn cap_token(&self) -> &str {
        &self.cap_token
    }

    fn validate_inbound(&self, msg: &MessageEnvelope) -> Result<(), TransportError> {
        match msg.payload {
            Some(message_envelope::Payload::AuthAck(ref ack)) => {
                if ack.cap_token != self.cap_token {
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

#[async_trait::async_trait]
impl Transport for AuthenticatedTransport {
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
