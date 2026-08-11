use common::config::HostFarmConfig;
use farm::FarmRegistry;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct McpGateway {
    client: reqwest::Client,
    registry: Arc<FarmRegistry>,
    config: Arc<HostFarmConfig>,
    sessions: Arc<Mutex<HashMap<(String, String), Option<String>>>>,
}

impl McpGateway {
    pub fn new(registry: Arc<FarmRegistry>, config: HostFarmConfig) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|err| format!("MCP HTTP client failed: {err}"))?;
        Ok(Self {
            client,
            registry,
            config: Arc::new(config),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn call(
        &self,
        subject: &str,
        server_id: &str,
        tool: &str,
        arguments: Value,
        request_id: &str,
    ) -> Result<Value, String> {
        let record = self
            .registry
            .get(subject)
            .ok_or_else(|| format!("unknown agent: {subject}"))?;
        let server = record
            .manifest
            .mcp
            .iter()
            .find(|server| server.id == server_id)
            .ok_or_else(|| format!("MCP server {server_id} is not exposed to {subject}"))?;
        if !server.tools.iter().any(|allowed| allowed == tool) {
            return Err(format!(
                "MCP tool {server_id}/{tool} is not exposed to {subject}"
            ));
        }
        let token = self.resolve_token(server.credential.as_deref())?;
        let key = (subject.to_string(), server_id.to_string());
        let cached = self.sessions.lock().await.get(&key).cloned();
        let session = match cached {
            Some(session) => session,
            None => {
                let (response, session) = self
                    .post(
                        server,
                        token.as_deref(),
                        None,
                        json!({
                            "jsonrpc": "2.0",
                            "id": format!("init-{request_id}"),
                            "method": "initialize",
                            "params": {
                                "protocolVersion": server.protocol_version,
                                "capabilities": {},
                                "clientInfo": {"name": "ironclaw", "version": env!("CARGO_PKG_VERSION")}
                            }
                        }),
                    )
                    .await?;
                reject_jsonrpc_error(&response)?;
                self.post(
                    server,
                    token.as_deref(),
                    session.as_deref(),
                    json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                )
                .await?;
                self.sessions.lock().await.insert(key, session.clone());
                session
            }
        };

        let (response, _) = self
            .post(
                server,
                token.as_deref(),
                session.as_deref(),
                json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": "tools/call",
                    "params": {"name": tool, "arguments": arguments}
                }),
            )
            .await?;
        reject_jsonrpc_error(&response)?;
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    fn resolve_token(&self, credential: Option<&str>) -> Result<Option<String>, String> {
        let Some(credential) = credential else {
            return Ok(None);
        };
        let env_name = self
            .config
            .mcp_credential_env
            .get(credential)
            .ok_or_else(|| format!("MCP credential {credential} has no host broker mapping"))?;
        let value = std::env::var(env_name).map_err(|_| {
            format!("MCP credential environment variable {env_name} is unavailable")
        })?;
        if value.trim().is_empty() {
            return Err(format!("MCP credential {credential} is empty"));
        }
        Ok(Some(value))
    }

    async fn post(
        &self,
        server: &farm::manifest::McpServerAccess,
        token: Option<&str>,
        session: Option<&str>,
        body: Value,
    ) -> Result<(Value, Option<String>), String> {
        let mut request = self
            .client
            .post(&server.server)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", &server.protocol_version)
            .json(&body);
        if let Some(token) = token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(session) = session {
            request = request.header("MCP-Session-Id", session);
        }
        let response = request
            .send()
            .await
            .map_err(|err| format!("MCP request failed: {err}"))?;
        let status = response.status();
        let response_session = response
            .headers()
            .get("MCP-Session-Id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let text = response
            .text()
            .await
            .map_err(|err| format!("MCP response read failed: {err}"))?;
        if !status.is_success() {
            return Err(format!("MCP server returned {status}: {}", truncate(&text)));
        }
        if text.trim().is_empty() {
            return Ok((Value::Null, response_session));
        }
        let payload = parse_json_or_sse(&text)?;
        Ok((payload, response_session))
    }
}

fn parse_json_or_sse(input: &str) -> Result<Value, String> {
    if let Ok(value) = serde_json::from_str(input) {
        return Ok(value);
    }
    for line in input.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            if let Ok(value) = serde_json::from_str(data.trim()) {
                return Ok(value);
            }
        }
    }
    Err("MCP response was neither JSON nor a JSON SSE event".to_string())
}

fn reject_jsonrpc_error(response: &Value) -> Result<(), String> {
    match response.get("error") {
        Some(error) => Err(format!(
            "MCP JSON-RPC error: {}",
            truncate(&error.to_string())
        )),
        None => Ok(()),
    }
}

fn truncate(value: &str) -> String {
    value.chars().take(2000).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_and_sse_responses() {
        assert_eq!(parse_json_or_sse("{\"result\":1}").unwrap()["result"], 1);
        assert_eq!(
            parse_json_or_sse("event: message\ndata: {\"result\":2}\n\n").unwrap()["result"],
            2
        );
    }
}
