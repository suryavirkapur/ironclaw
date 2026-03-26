use serde::Deserialize;
use std::sync::Arc;

use common::config::{HostLlmApi, HostLlmConfig};

use rig::client::CompletionClient;
use rig::completion::message::AssistantContent;
use rig::completion::{CompletionError, CompletionModel};

// ---------------------------------------------------------------------------
// Provider backend – wraps a Rig CompletionModel behind a dyn-compatible trait
// ---------------------------------------------------------------------------

trait ProviderComplete: Send + Sync {
    fn complete(
        &self,
        prompt: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, CompletionError>> + Send>>;
}

struct CompleteFn<M: CompletionModel> {
    model: M,
}

impl<M: CompletionModel + 'static> ProviderComplete for CompleteFn<M> {
    fn complete(
        &self,
        prompt: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, CompletionError>> + Send>>
    {
        let model = self.model.clone();
        let prompt = prompt.to_string();
        Box::pin(async move {
            let request = model
                .completion_request(&prompt)
                .max_tokens_opt(Some(1024))
                .build();
            let response = model.completion(request).await?;
            for item in response.choice.into_iter() {
                match item {
                    AssistantContent::Text(t) => return Ok(t.text),
                    _ => continue,
                }
            }
            Err(CompletionError::ResponseError(
                "response did not contain text content".into(),
            ))
        })
    }
}

enum ProviderBackend {
    Completions(Arc<dyn ProviderComplete>), // OpenAI Chat Completions
    Responses(Arc<dyn ProviderComplete>),   // OpenAI Responses API
    Anthropic(Arc<dyn ProviderComplete>),   // Anthropic Messages API
}

impl Clone for ProviderBackend {
    fn clone(&self) -> Self {
        match self {
            Self::Completions(c) => Self::Completions(c.clone()),
            Self::Responses(c) => Self::Responses(c.clone()),
            Self::Anthropic(c) => Self::Anthropic(c.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct LlmClient {
    backend: ProviderBackend,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationMessage {
    pub role: String,
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
#[error("llm client error: {message}")]
pub struct LlmClientError {
    message: String,
}

impl LlmClientError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// LlmClient implementation
// ---------------------------------------------------------------------------

impl LlmClient {
    pub fn new(config: HostLlmConfig) -> Result<Self, LlmClientError> {
        let backend = match config.api {
            HostLlmApi::ChatCompletions => {
                let api_key = env_key("OPENAI_API_KEY")?;
                let client = rig::providers::openai::CompletionsClient::builder()
                    .api_key(&api_key)
                    .base_url(config.base_url.trim_end_matches('/'))
                    .build()
                    .map_err(|e| LlmClientError::new(format!("openai client init failed: {e}")))?;
                let model = client.completion_model(&config.model);
                ProviderBackend::Completions(Arc::new(CompleteFn { model }))
            }
            HostLlmApi::Responses => {
                let api_key = env_key("OPENAI_API_KEY")?;
                let client = rig::providers::openai::Client::builder()
                    .api_key(&api_key)
                    .base_url(config.base_url.trim_end_matches('/'))
                    .build()
                    .map_err(|e| {
                        LlmClientError::new(format!("openai responses client init failed: {e}"))
                    })?;
                let model = client.completion_model(&config.model);
                ProviderBackend::Responses(Arc::new(CompleteFn { model }))
            }
            HostLlmApi::Message => {
                tracing::warn!(
                    "llm api variant 'message' is deprecated, using chat_completions path"
                );
                let api_key = env_key("OPENAI_API_KEY")?;
                let client = rig::providers::openai::CompletionsClient::builder()
                    .api_key(&api_key)
                    .base_url(config.base_url.trim_end_matches('/'))
                    .build()
                    .map_err(|e| LlmClientError::new(format!("openai client init failed: {e}")))?;
                let model = client.completion_model(&config.model);
                ProviderBackend::Completions(Arc::new(CompleteFn { model }))
            }
            HostLlmApi::Anthropic => {
                let api_key = env_key("ANTHROPIC_API_KEY")?;
                let client = rig::providers::anthropic::Client::builder()
                    .api_key(&api_key)
                    .base_url(config.base_url.trim_end_matches('/'))
                    .build()
                    .map_err(|e| {
                        LlmClientError::new(format!("anthropic client init failed: {e}"))
                    })?;
                let model = client.completion_model(&config.model);
                ProviderBackend::Anthropic(Arc::new(CompleteFn { model }))
            }
        };
        Ok(Self { backend })
    }

    pub async fn complete(&self, prompt: &str) -> Result<String, LlmClientError> {
        let result = match &self.backend {
            ProviderBackend::Completions(c) => c.complete(prompt).await,
            ProviderBackend::Responses(c) => c.complete(prompt).await,
            ProviderBackend::Anthropic(c) => c.complete(prompt).await,
        };
        result.map_err(|err| LlmClientError::new(format!("{err}")))
    }

    pub async fn plan_tool_or_answer(
        &self,
        user_text: &str,
        allowed_tools: &[String],
        memory_block: Option<&str>,
        history: Option<&[ConversationMessage]>,
    ) -> Result<ToolPlan, LlmClientError> {
        let prompt = build_tool_plan_prompt(user_text, allowed_tools, memory_block, history);
        let raw = self.complete(&prompt).await?;
        parse_tool_plan(&raw, allowed_tools)
    }

    pub async fn finalize_with_tool_output(
        &self,
        user_text: &str,
        tool: &str,
        input: &str,
        tool_ok: bool,
        tool_output: &str,
        memory_block: Option<&str>,
        history: Option<&[ConversationMessage]>,
    ) -> Result<String, LlmClientError> {
        let prompt = build_tool_finalize_prompt(
            user_text,
            tool,
            input,
            tool_ok,
            tool_output,
            memory_block,
            history,
        );
        self.complete(&prompt).await
    }
}

fn env_key(name: &str) -> Result<String, LlmClientError> {
    std::env::var(name).map_err(|_| LlmClientError::new(format!("missing {name}")))
}

// ---------------------------------------------------------------------------
// Tool plan types & parsing (unchanged from prior implementation)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolPlan {
    Tool { tool: String, input: String },
    Answer { text: String },
}

fn build_tool_plan_prompt(
    user_text: &str,
    allowed_tools: &[String],
    memory_block: Option<&str>,
    history: Option<&[ConversationMessage]>,
) -> String {
    let tools = allowed_tools.join(", ");
    let memory = build_memory_block(memory_block);
    let history_block = build_history_block(history);

    let code_exec_hint = if allowed_tools.iter().any(|t| t == "code_exec") {
        "\n- for code execution, use tool code_exec with input: {\"language\":\"python\",\"code\":\"...\"}"
    } else {
        ""
    };

    let tool_install_hint = if allowed_tools.iter().any(|t| t == "tool_install") {
        "\n- to create a reusable tool, use tool_install with input: {\"name\":\"...\",\"language\":\"python\",\"code\":\"...\",\"description\":\"...\"}"
    } else {
        ""
    };

    let tool_call_hint = if allowed_tools.iter().any(|t| t == "tool_call") {
        "\n- to call an installed tool, use tool_call with input: \"<tool_name> <args>\""
    } else {
        ""
    };

    let browser_hint = if allowed_tools.iter().any(|t| t == "browser") {
        "\n- for browser automation, use tool browser with json input like: {\"command\":\"snapshot\",\"args\":[\"-i\",\"-c\"]}"
    } else {
        ""
    };

    let browser_action_hint = if allowed_tools.iter().any(|t| t == "browser_action") {
        "\n- for simplified browser actions, use tool browser_action with json like: {\"action\":\"navigate\",\"url\":\"https://...\"} or {\"action\":\"click\",\"ref\":\"e5\"} or {\"action\":\"snapshot\",\"interactive\":true}"
    } else {
        ""
    };

    format!(
        "you are a host planner. choose exactly one action.\n\
         output valid json only.\n\
         schema:\n\
         - tool action: {{\"action\":\"tool\",\"tool\":\"<name>\",\"input\":\"<text>\"}}\n\
         - answer action: {{\"action\":\"answer\",\"text\":\"<response>\"}}\n\
         allowed tools: [{tools}]{code_exec_hint}{tool_install_hint}{tool_call_hint}{browser_hint}{browser_action_hint}\n\
         rules:\n\
         - if a tool is needed, choose action tool.\n\
         - if no tool is needed, choose action answer.\n\
         - do not include markdown.\n\
         {memory}\
         {history_block}\
         user message:\n\
         {user_text}"
    )
}

fn build_tool_finalize_prompt(
    user_text: &str,
    tool: &str,
    input: &str,
    tool_ok: bool,
    tool_output: &str,
    memory_block: Option<&str>,
    history: Option<&[ConversationMessage]>,
) -> String {
    let status = if tool_ok { "ok" } else { "error" };
    let memory = build_memory_block(memory_block);
    let history_block = build_history_block(history);
    format!(
        "you are a host assistant. write the final user-facing response.\n\
         use the tool result below.\n\
         {memory}\
         {history_block}\
         user message:\n\
         {user_text}\n\
         tool used: {tool}\n\
         tool input:\n\
         {input}\n\
         tool status: {status}\n\
         tool output:\n\
         {tool_output}\n\
         response rules:\n\
         - answer directly.\n\
         - if tool failed, explain failure briefly and suggest next step.\n\
         - no markdown code fences unless the user requested them."
    )
}

fn build_history_block(history: Option<&[ConversationMessage]>) -> String {
    let Some(history) = history else {
        return String::new();
    };
    if history.is_empty() {
        return String::new();
    }

    let mut block = String::from("conversation history:\n");
    for message in history {
        let role = if message.role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        block.push_str(&format!("{role}: {}\n", message.text));
    }
    block
}

fn build_memory_block(memory_block: Option<&str>) -> String {
    let Some(memory_block) = memory_block else {
        return String::new();
    };
    if memory_block.trim().is_empty() {
        return String::new();
    }
    format!("{memory_block}\n")
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
enum RawToolPlan {
    Tool { tool: String, input: String },
    Answer { text: String },
}

pub fn parse_tool_plan(raw: &str, allowed_tools: &[String]) -> Result<ToolPlan, LlmClientError> {
    let json = extract_json_object(raw).ok_or_else(|| LlmClientError::new("missing json plan"))?;
    let parsed: RawToolPlan = serde_json::from_str(json)
        .map_err(|err| LlmClientError::new(format!("tool plan parse failed: {err}")))?;
    match parsed {
        RawToolPlan::Tool { tool, input } => {
            let tool = tool.trim().to_string();
            if tool.is_empty() {
                return Err(LlmClientError::new("tool plan missing tool"));
            }
            if !allowed_tools.iter().any(|name| name == &tool) {
                return Err(LlmClientError::new(format!(
                    "tool not allowed in plan: {tool}"
                )));
            }

            let input = input.trim().to_string();
            if input.is_empty() {
                return Err(LlmClientError::new("tool plan missing input"));
            }
            Ok(ToolPlan::Tool { tool, input })
        }
        RawToolPlan::Answer { text } => {
            let text = text.trim().to_string();
            if text.is_empty() {
                return Err(LlmClientError::new("answer plan missing text"));
            }
            Ok(ToolPlan::Answer { text })
        }
    }
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }

    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, byte) in raw.char_indices() {
        match byte {
            '"' if !escaped => {
                in_string = !in_string;
            }
            '\\' if in_string => {
                escaped = !escaped;
                continue;
            }
            '{' if !in_string => {
                if start.is_none() {
                    start = Some(idx);
                }
                depth += 1;
            }
            '}' if !in_string => {
                if depth == 0 {
                    continue;
                }
                depth -= 1;
                if depth == 0 {
                    if let Some(begin) = start {
                        return Some(raw[begin..=idx].trim());
                    }
                }
            }
            _ => {}
        }
        if byte != '\\' {
            escaped = false;
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod llm_client_test {
    use super::{build_tool_plan_prompt, parse_tool_plan, ConversationMessage, ToolPlan};

    #[test]
    fn parses_answer_plan_json() {
        let allowed_tools = vec!["bash".to_string()];
        let raw = r#"{"action":"answer","text":"hello"}"#;
        let result = parse_tool_plan(raw, &allowed_tools);
        assert!(matches!(
            result,
            Ok(ToolPlan::Answer { text }) if text == "hello"
        ));
    }

    #[test]
    fn parses_tool_plan_from_wrapped_text() {
        let allowed_tools = vec!["bash".to_string(), "file_read".to_string()];
        let raw = r#"plan follows: {"action":"tool","tool":"bash","input":"ls -la"} thanks"#;
        let result = parse_tool_plan(raw, &allowed_tools);
        assert!(matches!(
            result,
            Ok(ToolPlan::Tool { tool, input }) if tool == "bash" && input == "ls -la"
        ));
    }

    #[test]
    fn rejects_unknown_tool_in_plan() {
        let allowed_tools = vec!["file_read".to_string()];
        let raw = r#"{"action":"tool","tool":"bash","input":"pwd"}"#;
        let result = parse_tool_plan(raw, &allowed_tools);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_empty_answer_text() {
        let allowed_tools = vec!["bash".to_string()];
        let raw = r#"{"action":"answer","text":"   "}"#;
        let result = parse_tool_plan(raw, &allowed_tools);
        assert!(result.is_err());
    }

    #[test]
    fn planning_prompt_includes_history_before_user_message() {
        let allowed_tools = vec!["bash".to_string()];
        let history = vec![
            ConversationMessage {
                role: "user".to_string(),
                text: "first".to_string(),
            },
            ConversationMessage {
                role: "assistant".to_string(),
                text: "second".to_string(),
            },
        ];
        let prompt =
            build_tool_plan_prompt("current message", &allowed_tools, None, Some(&history));

        let history_pos = prompt.find("conversation history:");
        let current_pos = prompt.find("user message:\ncurrent message");
        assert!(history_pos.is_some());
        assert!(current_pos.is_some());
        assert!(history_pos < current_pos);
    }

    #[test]
    fn planning_prompt_places_memory_before_history() {
        let allowed_tools = vec!["bash".to_string()];
        let history = vec![ConversationMessage {
            role: "user".to_string(),
            text: "recent message".to_string(),
        }];
        let prompt = build_tool_plan_prompt(
            "current message",
            &allowed_tools,
            Some("memory context:\n- id=1 text=prefers rust"),
            Some(&history),
        );

        let memory_pos = prompt.find("memory context:");
        let history_pos = prompt.find("conversation history:");
        let current_pos = prompt.find("user message:\ncurrent message");
        assert!(memory_pos.is_some());
        assert!(history_pos.is_some());
        assert!(current_pos.is_some());
        assert!(memory_pos < history_pos);
        assert!(history_pos < current_pos);
    }
}
