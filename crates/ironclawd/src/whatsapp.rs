//! WhatsApp integration using the whatsapp-rust crate (Baileys/Whatsmeow protocol).
//!
//! Event-driven: Bot connects via WebSocket, incoming messages are forwarded
//! through a tokio mpsc channel to the main processing loop.

use crate::IronclawError;
use common::config::HostWhatsAppConfig;
use common::transport::Transport;
use serde::{Deserialize, Serialize};
use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::sync::watch;
use wacore::types::events::Event;
use wacore_binary::jid::Jid;
use waproto::whatsapp as wa;
use whatsapp_rust::bot::Bot;
use whatsapp_rust::Client;

/// An incoming WhatsApp text message forwarded from the event handler.
#[derive(Clone, Debug)]
pub struct IncomingWhatsAppMessage {
    pub sender_jid: String,
    pub text: String,
    pub timestamp: u64,
}

/// Start the WhatsApp bot and return the client + incoming message receiver.
///
/// On first run a QR code is printed to the terminal for pairing.
/// Subsequent runs restore the session from the SQLite database.
pub async fn start_whatsapp_bot(
    config: &HostWhatsAppConfig,
) -> Result<(Arc<Client>, mpsc::Receiver<IncomingWhatsAppMessage>), IronclawError> {
    if !config.auth_method.eq_ignore_ascii_case("md5") {
        return Err(IronclawError::new(format!(
            "unsupported whatsapp auth method: {}",
            config.auth_method
        )));
    }

    let (tx, rx) = mpsc::channel::<IncomingWhatsAppMessage>(256);
    let (auth_tx, mut auth_rx) = watch::channel(AuthState::Starting);

    let session_dir = resolve_session_dir(config);
    std::fs::create_dir_all(&session_dir)
        .map_err(|err| IronclawError::new(format!("whatsapp session dir create failed: {err}")))?;
    let db_path = session_db_path(&session_dir);

    if let Some(parent) = StdPath::new(&db_path).parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            IronclawError::new(format!("whatsapp session dir create failed: {err}"))
        })?;
    }

    let db_path_str = db_path
        .to_str()
        .ok_or_else(|| IronclawError::new("whatsapp session path is not valid utf-8"))?;
    let backend = Arc::new(
        whatsapp_rust::store::SqliteStore::new(db_path_str)
            .await
            .map_err(|err| IronclawError::new(format!("whatsapp sqlite store failed: {err}")))?,
    );

    let event_tx = tx.clone();
    let auth_event_tx = auth_tx.clone();

    let mut bot = Bot::builder()
        .with_backend(backend)
        .with_transport_factory(whatsapp_rust::transport::TokioWebSocketTransportFactory::new())
        .with_http_client(whatsapp_rust::transport::UreqHttpClient::new())
        .on_event(move |event, _client| {
            let tx = event_tx.clone();
            let auth_tx = auth_event_tx.clone();
            async move {
                match event {
                    Event::PairingQrCode { code, .. } => {
                        let _ = auth_tx.send(AuthState::QrShown);
                        tracing::info!("whatsapp: scan this qr code to login");
                        if let Err(err) = qr2term::print_qr(&code) {
                            tracing::error!("whatsapp qr render failed: {err}");
                            println!("QR data (scan manually): {code}");
                        }
                    }
                    Event::PairSuccess(ref pair_success) => {
                        let _ = auth_tx.send(AuthState::Paired);
                        tracing::info!(
                            "whatsapp: paired successfully with {} ({})",
                            pair_success.id,
                            pair_success.platform
                        );
                    }
                    Event::PairError(ref error) => {
                        let _ = auth_tx.send(AuthState::PairFailed(error.error.clone()));
                        tracing::error!("whatsapp pairing failed: {}", error.error);
                    }
                    Event::Connected(_) => {
                        let _ = auth_tx.send(AuthState::Connected);
                        tracing::info!("whatsapp: connected");
                    }
                    Event::Message(ref msg, ref info) => {
                        let text = extract_text_from_message(msg);
                        if let Some(text) = text {
                            let sender_jid = info.source.sender.to_string();
                            let timestamp = info.timestamp.timestamp() as u64;
                            let incoming = IncomingWhatsAppMessage {
                                sender_jid,
                                text,
                                timestamp,
                            };
                            if tx.send(incoming).await.is_err() {
                                tracing::warn!("whatsapp message channel closed");
                            }
                        }
                    }
                    _ => {}
                }
            }
        })
        .build()
        .await
        .map_err(|err| IronclawError::new(format!("whatsapp bot build failed: {err}")))?;

    let client = bot.client();

    // Spawn the bot event loop in the background.
    tokio::spawn(async move {
        match bot.run().await {
            Ok(handle) => {
                if let Err(err) = handle.await {
                    tracing::error!("whatsapp bot run handle failed: {err}");
                }
            }
            Err(err) => {
                tracing::error!("whatsapp bot run failed: {err}");
            }
        }
    });

    let qr_timeout = std::time::Duration::from_millis(config.qr_timeout_ms);
    let ready = tokio::time::timeout(qr_timeout, async {
        loop {
            let current = auth_rx.borrow().clone();
            match current {
                AuthState::Connected | AuthState::Paired => break Ok(()),
                AuthState::PairFailed(reason) => {
                    break Err(IronclawError::new(format!(
                        "whatsapp pairing failed: {reason}"
                    )));
                }
                AuthState::Starting | AuthState::QrShown => {
                    if auth_rx.changed().await.is_err() {
                        break Err(IronclawError::new("whatsapp auth event channel closed"));
                    }
                }
            }
        }
    })
    .await;

    match ready {
        Ok(result) => result?,
        Err(_) => {
            return Err(IronclawError::new(format!(
                "whatsapp qr login timed out after {} ms",
                config.qr_timeout_ms
            )))
        }
    }

    Ok((client, rx))
}

/// Extract text content from a WhatsApp message.
fn extract_text_from_message(msg: &wa::Message) -> Option<String> {
    if let Some(ref text) = msg.conversation {
        return Some(text.clone());
    }
    if let Some(ref ext) = msg.extended_text_message {
        if let Some(ref text) = ext.text {
            return Some(text.clone());
        }
    }
    None
}

/// Send a text message to a WhatsApp JID.
pub async fn send_whatsapp_message(
    client: &Client,
    recipient_jid: &str,
    text: &str,
) -> Result<(), IronclawError> {
    let jid: Jid = recipient_jid
        .parse()
        .map_err(|err| IronclawError::new(format!("whatsapp jid parse failed: {err}")))?;

    let chunks = split_whatsapp_chunks(text, 4096);
    for chunk in chunks {
        let message = wa::Message {
            conversation: Some(chunk),
            ..Default::default()
        };
        client
            .send_message(jid.clone(), message)
            .await
            .map_err(|err| IronclawError::new(format!("whatsapp send failed: {err}")))?;
    }
    Ok(())
}

/// Check if a sender JID is in the allowlist.
pub fn is_allowed(config: &HostWhatsAppConfig, sender_jid: &str) -> bool {
    if config.allowlist.is_empty() {
        return config.self_chat_enabled;
    }
    // Extract the phone number part from JID.
    // Example: "15551234567" from "15551234567@s.whatsapp.net".
    let sender_number = sender_jid
        .split('@')
        .next()
        .unwrap_or(sender_jid)
        .replace([' ', '-', '+'], "");

    config.allowlist.iter().any(|allowed| {
        let normalized_allowed = allowed.replace([' ', '-', '+'], "");
        sender_number.contains(&normalized_allowed) || normalized_allowed.contains(&sender_number)
    })
}

/// Check if WhatsApp should be enabled.
pub fn should_enable_whatsapp(config: &common::config::HostConfig) -> bool {
    if std::env::args().any(|arg| arg == "--whatsapp") {
        return true;
    }
    config.whatsapp.enabled
}

fn resolve_session_dir(config: &HostWhatsAppConfig) -> PathBuf {
    if let Ok(value) = std::env::var("WHATSAPP_SESSION_PATH") {
        if !value.trim().is_empty() {
            return PathBuf::from(value);
        }
    }
    PathBuf::from(&config.session_dir)
}

fn session_db_path(session_dir: &StdPath) -> PathBuf {
    if session_dir.extension().is_some_and(|ext| ext == "db") {
        return session_dir.to_path_buf();
    }
    session_dir.join("session.db")
}

#[derive(Clone, Debug)]
enum AuthState {
    Starting,
    QrShown,
    Paired,
    Connected,
    PairFailed(String),
}

// ---------------------------------------------------------------------------
// Session & transcript persistence
// ---------------------------------------------------------------------------

const WHATSAPP_TRANSCRIPT_MAX_TURNS: usize = 50;

/// WhatsApp conversation session.
pub struct WhatsAppSession {
    pub sender_jid: String,
    pub user_id: String,
    pub session_id: String,
    pub msg_id: u64,
    pub transport: Option<Box<dyn Transport>>,
    pub transcript: WhatsAppTranscript,
    pub transcript_path: PathBuf,
    pub runtime_banner_sent: bool,
    pub guest_sleeping: bool,
    pub transport_failures: u32,
    pub last_user_message_ms: u64,
}

impl WhatsAppSession {
    pub fn load(sender_jid: &str, users_root: &StdPath) -> Result<Self, IronclawError> {
        let session_id = jid_to_session_id(sender_jid);
        let transcript_path = whatsapp_transcript_path(users_root, &session_id);
        let transcript = load_whatsapp_transcript(&transcript_path)?;
        Ok(Self {
            sender_jid: sender_jid.to_string(),
            user_id: "owner".to_string(),
            session_id,
            msg_id: 1,
            transport: None,
            transcript,
            transcript_path,
            runtime_banner_sent: false,
            guest_sleeping: false,
            transport_failures: 0,
            last_user_message_ms: 0,
        })
    }

    pub fn append_user_message(
        &mut self,
        text: &str,
        timestamp_ms: u64,
    ) -> Result<(), IronclawError> {
        self.transcript.push("user", text, timestamp_ms);
        save_whatsapp_transcript(&self.transcript_path, &self.transcript)
    }

    pub fn append_assistant_message(
        &mut self,
        text: &str,
        timestamp_ms: u64,
    ) -> Result<(), IronclawError> {
        self.transcript.push("assistant", text, timestamp_ms);
        save_whatsapp_transcript(&self.transcript_path, &self.transcript)
    }

    pub fn prompt_history(&self) -> Vec<crate::llm_client::ConversationMessage> {
        self.transcript
            .messages
            .iter()
            .map(|m| crate::llm_client::ConversationMessage {
                role: m.role.clone(),
                text: m.text.clone(),
            })
            .collect()
    }

    pub fn user_messages(&self) -> Vec<String> {
        self.transcript
            .messages
            .iter()
            .filter(|m| m.role == "user")
            .map(|m| m.text.clone())
            .collect()
    }

    pub fn transport_backoff(&self) -> std::time::Duration {
        let shifted = self.transport_failures.min(6);
        let delay_ms = 250u64.saturating_mul(1u64 << shifted);
        std::time::Duration::from_millis(delay_ms)
    }
}

/// Convert a JID to a session ID slug.
fn jid_to_session_id(jid: &str) -> String {
    let slug = jid
        .split('@')
        .next()
        .unwrap_or(jid)
        .replace([' ', '-', '+'], "");
    format!("whatsapp-{slug}")
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct WhatsAppTranscript {
    pub messages: Vec<WhatsAppTranscriptMessage>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WhatsAppTranscriptMessage {
    pub role: String,
    pub text: String,
    pub timestamp_ms: u64,
}

impl WhatsAppTranscript {
    pub fn push(&mut self, role: &str, text: &str, timestamp_ms: u64) {
        self.messages.push(WhatsAppTranscriptMessage {
            role: role.to_string(),
            text: text.to_string(),
            timestamp_ms,
        });
        let max_messages = WHATSAPP_TRANSCRIPT_MAX_TURNS.saturating_mul(2);
        if self.messages.len() > max_messages {
            let trim = self.messages.len().saturating_sub(max_messages);
            self.messages.drain(0..trim);
        }
    }
}

fn whatsapp_transcript_path(users_root: &StdPath, session_id: &str) -> PathBuf {
    users_root.join(session_id).join("whatsapp.transcript.json")
}

fn load_whatsapp_transcript(path: &StdPath) -> Result<WhatsAppTranscript, IronclawError> {
    if !path.exists() {
        return Ok(WhatsAppTranscript::default());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|err| IronclawError::new(format!("whatsapp transcript read failed: {err}")))?;
    let transcript: WhatsAppTranscript = serde_json::from_str(&raw)
        .map_err(|err| IronclawError::new(format!("whatsapp transcript decode failed: {err}")))?;
    Ok(transcript)
}

fn save_whatsapp_transcript(
    path: &StdPath,
    transcript: &WhatsAppTranscript,
) -> Result<(), IronclawError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| IronclawError::new(format!("whatsapp transcript dir failed: {err}")))?;
    }
    let data = serde_json::to_string_pretty(transcript)
        .map_err(|err| IronclawError::new(format!("whatsapp transcript encode failed: {err}")))?;
    std::fs::write(path, format!("{data}\n"))
        .map_err(|err| IronclawError::new(format!("whatsapp transcript write failed: {err}")))
}

/// Split a long message into chunks.
fn split_whatsapp_chunks(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if current.len() + c.len_utf8() > max_chars && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_keeps_last_100_messages() {
        let mut transcript = WhatsAppTranscript::default();
        for idx in 0..120u64 {
            transcript.push("user", &format!("u{idx}"), idx);
            transcript.push("assistant", &format!("a{idx}"), idx);
        }
        let max = WHATSAPP_TRANSCRIPT_MAX_TURNS * 2;
        assert_eq!(transcript.messages.len(), max);
        assert_eq!(
            transcript.messages.first().map(|m| m.text.clone()),
            Some("u70".to_string())
        );
    }

    #[test]
    fn is_allowed_with_empty_allowlist_and_self_chat() {
        let config = HostWhatsAppConfig {
            allowlist: vec![],
            self_chat_enabled: true,
            ..Default::default()
        };
        assert!(is_allowed(&config, "15551234567@s.whatsapp.net"));
    }

    #[test]
    fn is_allowed_with_empty_allowlist_no_self_chat() {
        let config = HostWhatsAppConfig {
            allowlist: vec![],
            self_chat_enabled: false,
            ..Default::default()
        };
        assert!(!is_allowed(&config, "15551234567@s.whatsapp.net"));
    }

    #[test]
    fn is_allowed_with_allowlist() {
        let config = HostWhatsAppConfig {
            allowlist: vec!["+15551234567".to_string(), "+10987654321".to_string()],
            self_chat_enabled: false,
            ..Default::default()
        };
        assert!(is_allowed(&config, "15551234567@s.whatsapp.net"));
        assert!(!is_allowed(&config, "19999999999@s.whatsapp.net"));
    }

    #[test]
    fn jid_to_session_id_strips_domain() {
        assert_eq!(
            jid_to_session_id("15551234567@s.whatsapp.net"),
            "whatsapp-15551234567"
        );
    }

    #[test]
    fn split_chunks_large_text() {
        let text = "x".repeat(9000);
        let chunks = split_whatsapp_chunks(&text, 4096);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 4096);
        assert_eq!(chunks[1].len(), 4096);
        assert_eq!(chunks[2].len(), 808);
    }

    #[test]
    fn parse_message_conversation_text() {
        let msg = wa::Message {
            conversation: Some("hello".to_string()),
            ..Default::default()
        };
        assert_eq!(extract_text_from_message(&msg), Some("hello".to_string()));
    }

    #[test]
    fn parse_message_extended_text() {
        let msg = wa::Message {
            extended_text_message: Some(Box::new(wa::message::ExtendedTextMessage {
                text: Some("hello ext".to_string()),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert_eq!(
            extract_text_from_message(&msg),
            Some("hello ext".to_string())
        );
    }

    #[test]
    fn parse_message_missing_text() {
        let msg = wa::Message::default();
        assert_eq!(extract_text_from_message(&msg), None);
    }

    #[test]
    fn session_db_path_from_dir_and_file() {
        assert_eq!(
            session_db_path(StdPath::new("data/whatsapp")),
            PathBuf::from("data/whatsapp/session.db")
        );
        assert_eq!(
            session_db_path(StdPath::new("data/whatsapp/session.db")),
            PathBuf::from("data/whatsapp/session.db")
        );
    }
}
