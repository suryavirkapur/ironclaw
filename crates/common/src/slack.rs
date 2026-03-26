use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SlackError {
    #[error("slack error: {0}")]
    Message(String),
    #[error("validation failed: {0}")]
    Validation(String),
}

#[derive(Debug, Deserialize)]
pub struct SlackWebhookPayload {
    pub team_id: Option<String>,
    pub channel_id: Option<String>,
    pub user_id: Option<String>,
    pub text: Option<String>,
    pub command: Option<String>,
    pub response_url: Option<String>,
    #[serde(rename = "type")]
    pub event_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SlackUrlVerification {
    pub challenge: String,
    #[serde(rename = "type")]
    pub type_: String,
}

#[derive(Debug, Serialize)]
pub struct SlackResponse {
    pub text: String,
    pub response_type: String,
}

impl SlackResponse {
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_string(),
            response_type: "ephemeral".to_string(),
        }
    }

    pub fn in_channel(text: &str) -> Self {
        Self {
            text: text.to_string(),
            response_type: "in_channel".to_string(),
        }
    }
}

#[allow(dead_code)]
pub fn validate_slack_signature(
    _signing_secret: &str,
    _timestamp: &str,
    _body: &str,
    _signature: &str,
) -> Result<(), SlackError> {
    tracing::warn!("slack signature validation is stubbed - not secure for production");
    Ok(())
}

pub fn parse_slack_message(body: &str) -> Result<SlackWebhookPayload, SlackError> {
    serde_json::from_str(body).map_err(|e| SlackError::Message(format!("parse failed: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slack_response() {
        let r = SlackResponse::new("hello");
        assert_eq!(r.text, "hello");
        assert_eq!(r.response_type, "ephemeral");

        let r2 = SlackResponse::in_channel("hello");
        assert_eq!(r2.response_type, "in_channel");
    }

    #[test]
    fn test_parse_slack_message() {
        let json = r#"{"text": "hello", "user_id": "U123"}"#;
        let payload = parse_slack_message(json).unwrap();
        assert_eq!(payload.text, Some("hello".to_string()));
        assert_eq!(payload.user_id, Some("U123".to_string()));
    }
}
