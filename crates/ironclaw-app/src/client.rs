use crate::model::{FarmAgent, HealthStatus, Snapshot, WsTicket};
use farm::{Capability, FarmTask};
use serde_json::{json, Value};

#[derive(Clone, Debug)]
pub struct DaemonClient {
    pub base_url: String,
    pub token: Option<String>,
}

impl DaemonClient {
    pub fn from_env() -> Self {
        let base_url = std::env::var("IRONCLAW_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:9938".to_string());
        let token = std::env::var("IRONCLAW_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        Self { base_url, token }
    }

    pub fn websocket_url(&self, agent_id: &str, ticket: &str) -> Result<String, String> {
        let mut url = url::Url::parse(&self.base_url).map_err(|err| err.to_string())?;
        let scheme = match url.scheme() {
            "https" => "wss",
            _ => "ws",
        };
        url.set_scheme(scheme)
            .map_err(|_| "could not convert daemon URL to websocket".to_string())?;
        url.set_path("/ws");
        url.set_query(None);
        url.query_pairs_mut()
            .append_pair("user_id", agent_id)
            .append_pair("session_id", &format!("workspace-{agent_id}"))
            .append_pair("ticket", ticket);
        Ok(url.to_string())
    }

    pub fn refresh(&self) -> Result<Snapshot, String> {
        let health: HealthStatus = self.get_json("/api/health")?;
        let agents: Vec<FarmAgent> = self.get_json("/api/farm/agents")?;
        let tasks: Vec<FarmTask> = self.get_json("/api/farm/tasks")?;
        Ok(Snapshot {
            health: format!("{} · live", health.status),
            agents,
            tasks,
        })
    }

    pub fn capabilities(&self, agent_id: &str) -> Result<Vec<Capability>, String> {
        self.get_json(&format!(
            "/api/farm/agents/{}/capabilities",
            encode_path(agent_id)
        ))
    }

    pub fn create_task(
        &self,
        requester: &str,
        assignee: &str,
        skill: &str,
        request: &str,
    ) -> Result<FarmTask, String> {
        self.post_json(
            "/api/farm/tasks",
            json!({
                "requester": requester,
                "assignee": assignee,
                "skill": skill,
                "input": { "request": request, "source": "workspace" }
            }),
        )
    }

    pub fn ws_ticket(&self, agent_id: &str) -> Result<WsTicket, String> {
        self.post_json("/api/auth/ws-ticket", json!({ "agent_id": agent_id }))
    }

    fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        self.send("GET", path, None)
    }

    fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: Value,
    ) -> Result<T, String> {
        self.send("POST", path, Some(body))
    }

    fn send<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<T, String> {
        let url = format!("{}{path}", self.base_url.trim_end_matches('/'));
        let mut request = ureq::request(method, &url);
        if let Some(token) = &self.token {
            request = request.set("authorization", &format!("Bearer {token}"));
        }
        let response = if let Some(body) = body {
            request.send_json(body)
        } else {
            request.call()
        };
        match response {
            Ok(response) => response
                .into_json()
                .map_err(|err| format!("decode {path} failed: {err}")),
            Err(ureq::Error::Status(code, response)) => {
                let detail = response.into_string().unwrap_or_default();
                Err(format!("{code} {path}: {detail}"))
            }
            Err(err) => Err(format!("daemon request failed: {err}")),
        }
    }
}

fn encode_path(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_url_uses_ticket_and_agent() {
        let client = DaemonClient {
            base_url: "http://127.0.0.1:9938".into(),
            token: None,
        };
        let url = client.websocket_url("product-manager", "abc").unwrap();
        assert!(url.starts_with("ws://127.0.0.1:9938/ws?"));
        assert!(url.contains("user_id=product-manager"));
        assert!(url.contains("ticket=abc"));
    }

    #[test]
    fn refresh_reads_health_agents_and_tasks() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for expected in ["/api/health", "/api/farm/agents", "/api/farm/tasks"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = std::io::Read::read(&mut stream, &mut buf);
                let request = String::from_utf8_lossy(&buf);
                assert!(request.contains(expected), "{request}");
                let body = match expected {
                    "/api/health" => r#"{"status":"ok","version":"test"}"#,
                    "/api/farm/agents" => {
                        r#"[{"id":"maya","name":"Maya","role":"Product Manager"}]"#
                    }
                    _ => r#"[]"#,
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
            }
        });
        let client = DaemonClient {
            base_url: format!("http://127.0.0.1:{port}"),
            token: None,
        };
        let snapshot = client.refresh().unwrap();
        assert_eq!(snapshot.health, "ok · live");
        assert_eq!(snapshot.agents[0].name, "Maya");
        assert!(snapshot.tasks.is_empty());
        server.join().unwrap();
    }
}
