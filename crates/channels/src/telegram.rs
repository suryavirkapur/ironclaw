use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::ChannelError;

/// incoming telegram webhook update
#[derive(Debug, Deserialize, ToSchema)]
pub struct TelegramUpdate {
    pub update_id: i64,
    pub message: Option<TelegramMessage>,
}

/// telegram message payload
#[derive(Debug, Deserialize, ToSchema)]
pub struct TelegramMessage {
    pub message_id: i64,
    pub chat: TelegramChat,
    pub text: Option<String>,
    pub date: i64,
}

/// telegram chat identifier
#[derive(Debug, Deserialize, ToSchema)]
pub struct TelegramChat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
}

/// validated inbound message ready for guest routing
#[derive(Debug, Clone, Serialize)]
pub struct InboundMessage {
    pub channel: String,
    pub sender_id: String,
    pub text: String,
    pub timestamp: i64,
}

/// parse and validate a raw telegram update into an inbound message
pub fn parse_update(update: TelegramUpdate) -> Result<InboundMessage, ChannelError> {
    let msg = update
        .message
        .ok_or_else(|| ChannelError::ParseFailed("no message in update".into()))?;
    let text = msg
        .text
        .ok_or_else(|| ChannelError::ParseFailed("no text in message".into()))?;
    Ok(InboundMessage {
        channel: "telegram".into(),
        sender_id: msg.chat.id.to_string(),
        text,
        timestamp: msg.date,
    })
}
