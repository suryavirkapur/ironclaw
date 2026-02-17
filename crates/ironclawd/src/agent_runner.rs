//! Simplified Agent Runner - ZeroClaw-compatible interface
//! Uses IronClaw's existing tools and LLM client

use crate::llm_client::{ConversationMessage, LlmClient};
use crate::providers::{ChatMessage, ChatRequest, Provider};
use crate::tools::{Tool, ToolResult, ToolSpec};
use std::sync::Arc;
use async_trait::async_trait;

/// Simple in-memory memory implementation
pub struct SimpleMemory {
    messages: Vec<ConversationMessage>,
}

impl SimpleMemory {
    pub fn new() -> Self {
        Self { messages: Vec::new() }
    }
    
    pub fn add_message(&mut self, role: String, text: String) {
        self.messages.push(ConversationMessage { role, text });
    }
    
    pub fn get_history(&self) -> &[ConversationMessage] {
        &self.messages
    }
}

impl Default for SimpleMemory {
    fn default() -> Self {
        Self::new()
    }
}

/// Agent configuration
pub struct AgentConfig {
    pub model: String,
    pub temperature: f64,
    pub max_history: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: "minimax/minimax-m2.5".to_string(),
            temperature: 0.7,
            max_history: 20,
        }
    }
}

/// Main agent that matches ZeroClaw's interface
pub struct Agent {
    provider: Arc<LlmClient>,
    tools: Vec<Box<dyn Tool>>,
    memory: SimpleMemory,
    config: AgentConfig,
}

impl Agent {
    pub fn new(provider: Arc<LlmClient>, tools: Vec<Box<dyn Tool>>, config: AgentConfig) -> Self {
        Self {
            provider,
            tools,
            memory: SimpleMemory::new(),
            config,
        }
    }
    
    /// Process a message and get a response (ZeroClaw-compatible signature)
    pub async fn process_message(&mut self, message: &str) -> anyhow::Result<String> {
        // Add user message to history
        self.memory.add_message("user".to_string(), message.to_string());
        
        // Build messages for LLM
        let mut chat_messages: Vec<ChatMessage> = self.memory
            .get_history()
            .iter()
            .map(|m| ChatMessage {
                role: m.role.clone(),
                content: Some(m.text.clone()),
                tool_calls: None,
            })
            .collect();
        
        // Add tools if available
        let tools: Vec<serde_json::Value> = self.tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters_schema()
                    }
                })
            })
            .collect();
        
        let request = ChatRequest {
            model: &self.config.model,
            messages: &chat_messages,
            tools: if tools.is_empty() { None } else { Some(&tools) },
        };
        
        // Call provider
        let response = self.provider.chat(request).await?;
        
        if let Some(msg) = response.message {
            if let Some(content) = msg.content {
                self.memory.add_message("assistant".to_string(), content.clone());
                return Ok(content);
            }
        }
        
        Ok("No response".to_string())
    }
}

impl crate::providers::Provider for LlmClient {
    async fn chat(&self, request: ChatRequest<'_>) -> anyhow::Result<crate::providers::ChatResponse> {
        // Build prompt from messages
        let prompt = request.messages
            .iter()
            .filter_map(|m| m.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        
        let result = self.complete(&prompt).await?;
        
        Ok(crate::providers::ChatResponse {
            message: Some(crate::providers::ChatMessage {
                role: "assistant".to_string(),
                content: Some(result),
                tool_calls: None,
            }),
            error: None,
        })
    }
    
    async fn chat_stream(
        &self,
        request: ChatRequest<'_>,
        _options: crate::providers::StreamOptions,
        mut on_chunk: impl FnMut(crate::providers::StreamChunk) + Send,
    ) -> anyhow::Result<()> {
        let prompt = request.messages
            .iter()
            .filter_map(|m| m.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        
        let result = self.complete(&prompt).await?;
        
        on_chunk(crate::providers::StreamChunk {
            content: Some(result),
            done: true,
        });
        
        Ok(())
    }
}
