use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A message in a conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: String,
    pub text: String,
}

/// A tool call request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// A chat message from the provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// Request to the chat API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [ChatMessage],
    pub tools: Option<&'a [serde_json::Value]>,
}

/// Response from the chat API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub message: Option<ChatMessage>,
    pub error: Option<String>,
}

/// Stream chunk from the provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    pub content: Option<String>,
    pub done: bool,
}

/// Options for streaming
#[derive(Debug, Clone)]
pub struct StreamOptions {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            max_tokens: None,
            temperature: None,
        }
    }
}

/// The Provider trait - LLM backend
#[async_trait]
pub trait Provider: Send + Sync {
    /// Send a chat request and get a response
    async fn chat(&self, request: ChatRequest<'_>) -> anyhow::Result<ChatResponse>;

    /// Stream chat completions
    async fn chat_stream(
        &self,
        request: ChatRequest<'_>,
        options: StreamOptions,
        mut on_chunk: impl FnMut(StreamChunk) + Send,
    ) -> anyhow::Result<()>;
}
