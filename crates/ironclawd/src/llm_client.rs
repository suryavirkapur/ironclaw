use serde::{Deserialize, Serialize};

use common::config::{HostLlmApi, HostLlmConfig};

#[derive(Clone)]
pub struct LlmClient {
    config: HostLlmConfig,
    http_client: reqwest::Client,
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
    use super::parse_llm_response;
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

}
