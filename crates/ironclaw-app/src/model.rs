use farm::{Capability, CapabilityKind, FarmTask, TaskState};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceView {
    Delivery,
    Team,
    Architecture,
    Chat,
    Marketplace,
}

impl WorkspaceView {
    pub fn title(self) -> &'static str {
        match self {
            Self::Delivery => "delivery",
            Self::Team => "team",
            Self::Architecture => "architecture",
            Self::Chat => "conversations",
            Self::Marketplace => "marketplace",
        }
    }

    pub fn prefix(self) -> &'static str {
        match self {
            Self::Chat => "●",
            _ => "#",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Delivery => "Live A2A work across the engineering team",
            Self::Team => "Isolated agents with private memory and capabilities",
            Self::Architecture => "One shared control plane, private agent VMs",
            Self::Chat => "Choose an agent from the sidebar",
            Self::Marketplace => "Install Wasm, MCP, and A2A tools by rewriting agent manifests",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct FarmAgent {
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

#[derive(Clone, Debug, Deserialize)]
pub struct HealthStatus {
    pub status: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WsTicket {
    pub ticket: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChatMessage {
    pub id: String,
    pub at_ms: u64,
    pub role: ChatRole,
    pub text: String,
    pub task_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Agent,
    System,
}

impl ChatRole {
    pub fn label(self, agent_name: &str) -> String {
        match self {
            Self::User => "You".into(),
            Self::Agent => agent_name.into(),
            Self::System => "Workspace".into(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub health: String,
    pub agents: Vec<FarmAgent>,
    pub tasks: Vec<FarmTask>,
}

#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub active: usize,
    pub completed: usize,
    pub failed: usize,
    pub agents: usize,
}

pub fn metrics(agents: &[FarmAgent], tasks: &[FarmTask]) -> Metrics {
    Metrics {
        active: tasks.iter().filter(|task| !task.state.terminal()).count(),
        completed: tasks
            .iter()
            .filter(|task| task.state == TaskState::Completed)
            .count(),
        failed: tasks
            .iter()
            .filter(|task| matches!(task.state, TaskState::Failed | TaskState::Rejected))
            .count(),
        agents: agents.len(),
    }
}

pub fn active_task_count(tasks: &[FarmTask], agent_id: &str) -> usize {
    tasks
        .iter()
        .filter(|task| task.assignee == agent_id && !task.state.terminal())
        .count()
}

pub fn task_summary(task: &FarmTask) -> String {
    task.input
        .get("request")
        .or_else(|| task.input.get("question"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| task.input.to_string())
}

pub fn skill_label(skill: &str) -> String {
    skill.replace('_', " ")
}

pub fn a2a_assignments(capabilities: &[Capability]) -> Vec<Capability> {
    capabilities
        .iter()
        .filter(|capability| capability.kind == CapabilityKind::A2aSkill)
        .cloned()
        .collect()
}

pub fn parse_a2a_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix("agent://")?;
    let (agent, skill) = rest.split_once('/')?;
    if agent.is_empty() || skill.is_empty() {
        return None;
    }
    Some((agent.to_string(), skill.to_string()))
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn format_timestamp(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let minutes = (secs / 60) % 60;
    let hours = (secs / 3600) % 24;
    format!("{hours:02}:{minutes:02}")
}

pub fn push_thread_message(
    threads: &mut HashMap<String, Vec<ChatMessage>>,
    agent_id: &str,
    message: ChatMessage,
) {
    let thread = threads.entry(agent_id.to_string()).or_default();
    thread.push(message);
    let overflow = thread.len().saturating_sub(100);
    if overflow > 0 {
        thread.drain(..overflow);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_task(state: TaskState, assignee: &str) -> FarmTask {
        FarmTask {
            id: "t1".into(),
            context_id: "c1".into(),
            parent_task_id: None,
            requester: "maya".into(),
            assignee: assignee.into(),
            skill: "implement_backend".into(),
            state,
            input: json!({"request": "Ship the API"}),
            output: None,
            artifact_ids: Vec::new(),
            delegation_depth: 0,
            created_at_ms: 1,
            updated_at_ms: 2,
        }
    }

    #[test]
    fn parses_farm_agents() {
        let agents: Vec<FarmAgent> = serde_json::from_str(
            r#"[{"id":"product-manager","name":"Maya","role":"Product Manager","a2a_skills":2}]"#,
        )
        .unwrap();
        assert_eq!(agents[0].name, "Maya");
        assert_eq!(agents[0].a2a_skills, 2);
    }

    #[test]
    fn metrics_count_active_and_failed() {
        let agents = vec![FarmAgent {
            id: "nora".into(),
            name: "Nora".into(),
            role: "Backend Engineer".into(),
            reports_to: None,
            enabled: true,
            memory_engine: "sqlite".into(),
            revision: "1".into(),
            wasm_tools: 0,
            mcp_servers: 0,
            a2a_skills: 1,
        }];
        let tasks = vec![
            sample_task(TaskState::Working, "nora"),
            sample_task(TaskState::Completed, "nora"),
            sample_task(TaskState::Failed, "nora"),
        ];
        let counts = metrics(&agents, &tasks);
        assert_eq!(counts.active, 1);
        assert_eq!(counts.completed, 1);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.agents, 1);
    }

    #[test]
    fn parses_a2a_route() {
        assert_eq!(
            parse_a2a_uri("agent://backend-engineer/implement_backend"),
            Some(("backend-engineer".into(), "implement_backend".into()))
        );
        assert_eq!(parse_a2a_uri("not-a-uri"), None);
    }

    #[test]
    fn marketplace_view_explains_tool_loading() {
        assert_eq!(WorkspaceView::Marketplace.title(), "marketplace");
        assert!(WorkspaceView::Marketplace
            .description()
            .contains("rewriting agent manifests"));
    }
}
