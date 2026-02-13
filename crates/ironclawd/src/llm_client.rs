use serde::{Deserialize, Serialize};

use common::config::{HostLlmApi, HostLlmConfig};

#[derive(Clone)]
pub struct LlmClient {
    config: HostLlmConfig,
    http_client: reqwest::Client,
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

impl LlmClient {
    pub fn new(config: HostLlmConfig) -> Result<Self, LlmClientError> {
        let http_client = reqwest::Client::builder()
            .build()
            .map_err(|err| LlmClientError::new(format!("http client init failed: {err}")))?;
        Ok(Self {
            config,
            http_client,
        })
    }

    pub async fn complete(&self, prompt: &str) -> Result<String, LlmClientError> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| LlmClientError::new("missing openai_api_key"))?;
        let trimmed_base = self.config.base_url.trim_end_matches('/');
        let url = format!("{trimmed_base}{}", self.config.api.path());
        let payload = match self.config.api {
            HostLlmApi::ChatCompletions => serde_json::to_value(ChatCompletionsRequest {
                model: self.config.model.clone(),
                messages: vec![ChatMessage {
                    role: "user".to_string(),
                    content: prompt.to_string(),
                }],
            })
            .map_err(|err| LlmClientError::new(format!("json encode failed: {err}")))?,
            HostLlmApi::Responses => serde_json::to_value(ResponsesRequest {
                model: self.config.model.clone(),
                input: prompt.to_string(),
            })
            .map_err(|err| LlmClientError::new(format!("json encode failed: {err}")))?,
        };

        let response = self
            .http_client
            .post(url)
            .bearer_auth(api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|err| LlmClientError::new(format!("llm request failed: {err}")))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| LlmClientError::new(format!("response decode failed: {err}")))?;
        if !status.is_success() {
            return Err(LlmClientError::new(format!(
                "llm request failed with status {}: {body}",
                status.as_u16()
            )));
        }

        parse_llm_response(self.config.api, &body)
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

fn parse_llm_response(api: HostLlmApi, body: &str) -> Result<String, LlmClientError> {
    match api {
        HostLlmApi::ChatCompletions => {
            let response: ChatCompletionsResponse = serde_json::from_str(body)
                .map_err(|err| LlmClientError::new(format!("parse chat response failed: {err}")))?;
            response
                .choices
                .first()
                .map(|choice| choice.message.content.clone())
                .filter(|content| !content.is_empty())
                .ok_or_else(|| LlmClientError::new("chat response did not contain text"))
        }
        HostLlmApi::Responses => {
            let response: ResponsesResponse = serde_json::from_str(body).map_err(|err| {
                LlmClientError::new(format!("parse responses response failed: {err}"))
            })?;
            if let Some(output_text) = response.output_text.filter(|value| !value.is_empty()) {
                return Ok(output_text);
            }

            for item in response.output {
                for content in item.content {
                    if let Some(text) = content.text.filter(|value| !value.is_empty()) {
                        return Ok(text);
                    }
                }
            }
            Err(LlmClientError::new("responses output did not contain text"))
        }
    }
}

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
    format!(
        "you are a host planner. choose exactly one action.\n\
         output valid json only.\n\
         schema:\n\
         - tool action: {{\"action\":\"tool\",\"tool\":\"<name>\",\"input\":\"<text>\"}}\n\
         - answer action: {{\"action\":\"answer\",\"text\":\"<response>\"}}\n\
         allowed tools: [{tools}]\n\
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

#[derive(Serialize)]
struct ChatCompletionsRequest {
    model: String,
    messages: Vec<ChatMessage>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatCompletionsResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

#[derive(Serialize)]
struct ResponsesRequest {
    model: String,
    input: String,
}

#[derive(Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    output_text: Option<String>,
    #[serde(default)]
    output: Vec<ResponsesOutputItem>,
}

#[derive(Deserialize)]
struct ResponsesOutputItem {
    #[serde(default)]
    content: Vec<ResponsesContentItem>,
}

#[derive(Deserialize)]
struct ResponsesContentItem {
    #[serde(default)]
    text: Option<String>,
}

#[cfg(test)]
mod llm_client_test {
    use super::{
        build_tool_plan_prompt, parse_llm_response, parse_tool_plan, ConversationMessage, ToolPlan,
    };
    use common::config::HostLlmApi;

    #[test]
    fn parses_chat_completions_text() {
        let body = r#"{"choices":[{"message":{"content":"hello"}}]}"#;
        let result = parse_llm_response(HostLlmApi::ChatCompletions, body);
        assert!(result.is_ok());
        assert_eq!(result.unwrap_or_default(), "hello");
    }

    #[test]
    fn parses_responses_output_text_fallback() {
        let body = r#"{"output":[{"content":[{"text":"hello from responses"}]}]}"#;
        let result = parse_llm_response(HostLlmApi::Responses, body);
        assert!(result.is_ok());
        assert_eq!(result.unwrap_or_default(), "hello from responses");
    }

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
