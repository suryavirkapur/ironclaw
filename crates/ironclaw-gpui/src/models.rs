//! Data models mirroring the ironclawd farm control-plane REST API
//! (`/api/health`, `/api/farm/agents`, `/api/farm/tasks`).
//!
//! Some fields are retained to document the wire contract even when the UI does
//! not render them yet.
#![allow(dead_code)]

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Default)]
pub struct Health {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentSummary {
    pub id: String,
    pub name: String,
    pub role: String,
    #[serde(default)]
    pub reports_to: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub memory_engine: String,
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub wasm_tools: usize,
    #[serde(default)]
    pub mcp_servers: usize,
    #[serde(default)]
    pub a2a_skills: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Submitted,
    Working,
    InputRequired,
    Completed,
    Failed,
    Canceled,
    Rejected,
}

impl TaskState {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Canceled | Self::Rejected
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Working => "working",
            Self::InputRequired => "input required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct FarmTask {
    pub id: String,
    #[serde(default)]
    pub context_id: String,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    pub requester: String,
    pub assignee: String,
    pub skill: String,
    pub state: TaskState,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub output: Option<Value>,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
    #[serde(default)]
    pub delegation_depth: u8,
    #[serde(default)]
    pub created_at_ms: u64,
    #[serde(default)]
    pub updated_at_ms: u64,
}

impl FarmTask {
    /// Short one-line summary of the request, mirroring the web UI.
    pub fn summary(&self) -> String {
        if let Some(request) = self.input.get("request").and_then(Value::as_str) {
            return request.to_string();
        }
        if let Some(question) = self.input.get("question").and_then(Value::as_str) {
            return question.to_string();
        }
        if self.input.is_null() {
            String::new()
        } else {
            self.input.to_string()
        }
    }
}

/// Runtime state of an agent's sandbox, from `/api/farm/vms`.
#[derive(Clone, Debug, Deserialize, Default)]
pub struct VmState {
    pub agent_id: String,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub backend: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Capability {
    pub uri: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kind: String,
}
