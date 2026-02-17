use serde::Deserialize;
use utoipa::ToSchema;

use crate::ChannelError;

/// whatsapp cloud api webhook payload
#[derive(Debug, Deserialize, ToSchema)]
pub struct WhatsAppWebhook {
    pub object: String,
    pub entry: Vec<WhatsAppEntry>,
}

/// whatsapp webhook entry
#[derive(Debug, Deserialize, ToSchema)]
pub struct WhatsAppEntry {
    pub id: String,
    pub changes: Vec<WhatsAppChange>,
}

/// whatsapp change payload
#[derive(Debug, Deserialize, ToSchema)]
pub struct WhatsAppChange {
    pub field: String,
    pub value: WhatsAppValue,
}

/// whatsapp value containing messages
#[derive(Debug, Deserialize, ToSchema)]
pub struct WhatsAppValue {
    pub messaging_product: String,
    pub metadata: WhatsAppMetadata,
    #[serde(default)]
    pub messages: Vec<WhatsAppInboundMessage>,
}

/// whatsapp metadata
#[derive(Debug, Deserialize, ToSchema)]
pub struct WhatsAppMetadata {
    pub display_phone_number: String,
    pub phone_number_id: String,
}

/// whatsapp inbound message
#[derive(Debug, Deserialize, ToSchema)]
pub struct WhatsAppInboundMessage {
    pub from: String,
    pub id: String,
    pub timestamp: String,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub text: Option<WhatsAppText>,
}

/// whatsapp text content
#[derive(Debug, Deserialize, ToSchema)]
pub struct WhatsAppText {
    pub body: String,
}

/// parse whatsapp cloud api webhook into inbound messages
pub fn parse_webhook(
    webhook: WhatsAppWebhook,
) -> Result<Vec<super::telegram::InboundMessage>, ChannelError> {
    let mut messages = Vec::new();
    for entry in webhook.entry {
        for change in entry.changes {
            if change.field != "messages" {
                continue;
            }
            for msg in change.value.messages {
                if msg.msg_type != "text" {
                    continue;
                }
                let text = msg
                    .text
                    .ok_or_else(|| ChannelError::ParseFailed("text message missing body".into()))?;
                let ts: i64 = msg.timestamp.parse().unwrap_or(0);
                messages.push(super::telegram::InboundMessage {
                    channel: "whatsapp".into(),
                    sender_id: msg.from,
                    text: text.body,
                    timestamp: ts,
                });
            }
        }
    }
    if messages.is_empty() {
        return Err(ChannelError::ParseFailed(
            "no text messages in webhook".into(),
        ));
    }
    Ok(messages)
}
