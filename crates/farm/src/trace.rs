//! Append-only agentic traces for later specialist-model fine-tuning.
//!
//! Tools are the employee's job. Memory is the craft knowledge they accumulate.
//! Traces are the complete tool-use trajectories that will train a narrower
//! model for that role. Record them from the first production turn; do not wait
//! until a fine-tune pipeline exists.
//!
//! Layout:
//!
//! ```text
//! <users_root>/_farm/traces/<agent_id>/
//!   events.jsonl          one operational event per line
//!   trajectories.jsonl    one completed (or interrupted) turn per line
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_FIELD_CHARS: usize = 16_384;
const SCHEMA_VERSION: u32 = 1;

#[derive(Clone)]
pub struct TraceStore {
    root: Arc<PathBuf>,
    inner: Arc<Mutex<TraceState>>,
}

#[derive(Default)]
struct TraceState {
    turns: HashMap<TurnKey, OpenTurn>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TurnKey {
    agent_id: String,
    session_id: String,
}

#[derive(Clone, Debug)]
struct OpenTurn {
    user_fingerprint: String,
    trajectory: Trajectory,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TraceEvent {
    pub schema_version: u32,
    pub recorded_at_ms: u64,
    pub kind: String,
    pub trace_id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub payload: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TraceToolStep {
    pub iteration: usize,
    pub name: String,
    pub input: String,
    pub ok: bool,
    pub output: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Trajectory {
    pub schema_version: u32,
    pub trace_id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub channel: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill: Option<String>,
    pub model: String,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub outcome: String,
    pub planner: String,
    pub user: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub memory: String,
    #[serde(default)]
    pub steps: Vec<TraceToolStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// OpenAI-style chat messages ready to copy into a fine-tune JSONL.
    pub messages: Vec<Value>,
}

#[derive(Clone, Debug)]
pub struct PlanRecord {
    pub agent_id: String,
    pub role: Option<String>,
    pub session_id: String,
    pub task_id: Option<String>,
    pub skill: Option<String>,
    pub channel: String,
    pub model: String,
    pub user_text: String,
    pub memory: String,
    pub observations: Vec<TraceToolStep>,
    pub planner: String,
    pub plan_tool: Option<(String, String)>,
    pub plan_answer: Option<String>,
    pub error: Option<String>,
    pub recorded_at_ms: u64,
}

#[derive(Clone, Debug)]
pub struct ToolRecord {
    pub agent_id: String,
    pub role: Option<String>,
    pub session_id: String,
    pub task_id: Option<String>,
    pub skill: Option<String>,
    pub channel: String,
    pub model: Option<String>,
    pub tool: String,
    pub input: String,
    pub ok: bool,
    pub output: String,
    pub recorded_at_ms: u64,
}

#[derive(Clone, Debug)]
pub struct TaskRecord {
    pub agent_id: String,
    pub role: Option<String>,
    pub task_id: String,
    pub skill: String,
    pub requester: String,
    pub channel: String,
    pub kind: String,
    pub state: String,
    pub recorded_at_ms: u64,
    pub payload: Value,
}

impl TraceStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, TraceError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|err| TraceError::Io {
            path: root.clone(),
            message: err.to_string(),
        })?;
        Ok(Self {
            root: Arc::new(root),
            inner: Arc::new(Mutex::new(TraceState::default())),
        })
    }

    pub fn record_plan(&self, record: PlanRecord) -> Result<String, TraceError> {
        let mut state = self.inner.lock().map_err(|_| TraceError::Poisoned)?;
        let key = TurnKey {
            agent_id: record.agent_id.clone(),
            session_id: record.session_id.clone(),
        };
        let fingerprint = fingerprint(&record.user_text);
        let start_new_turn = match state.turns.get(&key) {
            None => true,
            Some(open) => open.user_fingerprint != fingerprint,
        };
        if start_new_turn {
            if let Some(open) = state.turns.remove(&key) {
                drop(state);
                self.persist_interrupted(open.trajectory, record.recorded_at_ms)?;
                state = self.inner.lock().map_err(|_| TraceError::Poisoned)?;
            }
            let trace_id = new_trace_id(record.recorded_at_ms);
            state.turns.insert(
                key.clone(),
                OpenTurn {
                    user_fingerprint: fingerprint,
                    trajectory: Trajectory {
                        schema_version: SCHEMA_VERSION,
                        trace_id: trace_id.clone(),
                        agent_id: record.agent_id.clone(),
                        role: record.role.clone(),
                        channel: record.channel.clone(),
                        session_id: record.session_id.clone(),
                        task_id: record.task_id.clone(),
                        skill: record.skill.clone(),
                        model: record.model.clone(),
                        started_at_ms: record.recorded_at_ms,
                        completed_at_ms: record.recorded_at_ms,
                        outcome: "working".to_string(),
                        planner: record.planner.clone(),
                        user: redact_and_truncate(&record.user_text),
                        memory: redact_and_truncate(&record.memory),
                        steps: Vec::new(),
                        answer: None,
                        error: None,
                        messages: Vec::new(),
                    },
                },
            );
        }

        let open = state
            .turns
            .get_mut(&key)
            .ok_or_else(|| TraceError::Invalid("active turn missing after insert".into()))?;
        open.trajectory.planner = record.planner.clone();
        if open.trajectory.memory.is_empty() && !record.memory.is_empty() {
            open.trajectory.memory = redact_and_truncate(&record.memory);
        }
        if record.task_id.is_some() {
            open.trajectory.task_id = record.task_id.clone();
        }
        if record.skill.is_some() {
            open.trajectory.skill = record.skill.clone();
        }
        open.trajectory.steps = record.observations.into_iter().map(sanitize_step).collect();

        let trace_id = open.trajectory.trace_id.clone();
        let payload = if let Some(error) = &record.error {
            serde_json::json!({
                "planner": record.planner,
                "error": redact_and_truncate(error),
                "observation_count": open.trajectory.steps.len(),
            })
        } else if let Some((tool, input)) = &record.plan_tool {
            serde_json::json!({
                "planner": record.planner,
                "action": "tool",
                "tool": tool,
                "input": redact_and_truncate(input),
                "observation_count": open.trajectory.steps.len(),
            })
        } else {
            serde_json::json!({
                "planner": record.planner,
                "action": "answer",
                "observation_count": open.trajectory.steps.len(),
            })
        };
        let event = TraceEvent {
            schema_version: SCHEMA_VERSION,
            recorded_at_ms: record.recorded_at_ms,
            kind: "plan".to_string(),
            trace_id: trace_id.clone(),
            agent_id: record.agent_id.clone(),
            role: record.role.clone(),
            channel: Some(record.channel.clone()),
            session_id: Some(record.session_id.clone()),
            task_id: record.task_id.clone(),
            skill: record.skill.clone(),
            model: Some(record.model.clone()),
            payload,
        };
        self.append_jsonl(&self.events_path(&record.agent_id)?, &event)?;

        let finished = record.plan_answer.is_some() || record.error.is_some();
        if finished {
            let mut trajectory = state
                .turns
                .remove(&key)
                .ok_or_else(|| TraceError::Invalid("active turn missing at close".into()))?
                .trajectory;
            trajectory.completed_at_ms = record.recorded_at_ms;
            if let Some(error) = record.error {
                trajectory.outcome = "error".to_string();
                trajectory.error = Some(redact_and_truncate(&error));
            } else {
                trajectory.outcome = "answer".to_string();
                trajectory.answer = record.plan_answer.map(|text| redact_and_truncate(&text));
            }
            trajectory.messages = finetune_messages(&trajectory);
            self.append_jsonl(&self.trajectories_path(&record.agent_id)?, &trajectory)?;
            self.append_jsonl(
                &self.events_path(&record.agent_id)?,
                &TraceEvent {
                    schema_version: SCHEMA_VERSION,
                    recorded_at_ms: record.recorded_at_ms,
                    kind: "turn_end".to_string(),
                    trace_id: trajectory.trace_id.clone(),
                    agent_id: record.agent_id,
                    role: record.role,
                    channel: Some(record.channel),
                    session_id: Some(record.session_id),
                    task_id: record.task_id,
                    skill: record.skill,
                    model: Some(record.model),
                    payload: serde_json::json!({
                        "outcome": trajectory.outcome,
                        "step_count": trajectory.steps.len(),
                    }),
                },
            )?;
        }
        Ok(trace_id)
    }

    pub fn record_tool(&self, record: ToolRecord) -> Result<(), TraceError> {
        let state = self.inner.lock().map_err(|_| TraceError::Poisoned)?;
        let key = TurnKey {
            agent_id: record.agent_id.clone(),
            session_id: record.session_id.clone(),
        };
        let trace_id = state
            .turns
            .get(&key)
            .map(|open| open.trajectory.trace_id.clone())
            .unwrap_or_else(|| new_trace_id(record.recorded_at_ms));
        drop(state);
        self.append_jsonl(
            &self.events_path(&record.agent_id)?,
            &TraceEvent {
                schema_version: SCHEMA_VERSION,
                recorded_at_ms: record.recorded_at_ms,
                kind: "tool".to_string(),
                trace_id,
                agent_id: record.agent_id,
                role: record.role,
                channel: Some(record.channel),
                session_id: Some(record.session_id),
                task_id: record.task_id,
                skill: record.skill,
                model: record.model,
                payload: serde_json::json!({
                    "tool": record.tool,
                    "input": redact_and_truncate(&record.input),
                    "ok": record.ok,
                    "output": redact_and_truncate(&record.output),
                }),
            },
        )
    }

    pub fn record_task(&self, record: TaskRecord) -> Result<(), TraceError> {
        self.append_jsonl(
            &self.events_path(&record.agent_id)?,
            &TraceEvent {
                schema_version: SCHEMA_VERSION,
                recorded_at_ms: record.recorded_at_ms,
                kind: record.kind.clone(),
                trace_id: format!("task-{}", record.task_id),
                agent_id: record.agent_id,
                role: record.role,
                channel: Some(record.channel),
                session_id: None,
                task_id: Some(record.task_id.clone()),
                skill: Some(record.skill),
                model: None,
                payload: serde_json::json!({
                    "requester": record.requester,
                    "state": record.state,
                    "detail": record.payload,
                }),
            },
        )
    }

    pub fn read_events(&self, agent_id: &str) -> Result<Vec<TraceEvent>, TraceError> {
        read_jsonl(&self.events_path(agent_id)?)
    }

    pub fn read_trajectories(&self, agent_id: &str) -> Result<Vec<Trajectory>, TraceError> {
        read_jsonl(&self.trajectories_path(agent_id)?)
    }

    fn persist_interrupted(
        &self,
        mut trajectory: Trajectory,
        recorded_at_ms: u64,
    ) -> Result<(), TraceError> {
        trajectory.completed_at_ms = recorded_at_ms;
        trajectory.outcome = "interrupted".to_string();
        trajectory.messages = finetune_messages(&trajectory);
        let agent_id = trajectory.agent_id.clone();
        self.append_jsonl(&self.trajectories_path(&agent_id)?, &trajectory)?;
        self.append_jsonl(
            &self.events_path(&agent_id)?,
            &TraceEvent {
                schema_version: SCHEMA_VERSION,
                recorded_at_ms,
                kind: "turn_end".to_string(),
                trace_id: trajectory.trace_id,
                agent_id,
                role: trajectory.role,
                channel: Some(trajectory.channel),
                session_id: Some(trajectory.session_id),
                task_id: trajectory.task_id,
                skill: trajectory.skill,
                model: Some(trajectory.model),
                payload: serde_json::json!({"outcome": "interrupted"}),
            },
        )
    }

    fn events_path(&self, agent_id: &str) -> Result<PathBuf, TraceError> {
        Ok(self.agent_dir(agent_id)?.join("events.jsonl"))
    }

    fn trajectories_path(&self, agent_id: &str) -> Result<PathBuf, TraceError> {
        Ok(self.agent_dir(agent_id)?.join("trajectories.jsonl"))
    }

    fn agent_dir(&self, agent_id: &str) -> Result<PathBuf, TraceError> {
        let safe = safe_agent_id(agent_id)?;
        let dir = self.root.join(safe);
        std::fs::create_dir_all(&dir).map_err(|err| TraceError::Io {
            path: dir.clone(),
            message: err.to_string(),
        })?;
        Ok(dir)
    }

    fn append_jsonl<T: Serialize>(&self, path: &Path, value: &T) -> Result<(), TraceError> {
        let encoded =
            serde_json::to_vec(value).map_err(|err| TraceError::Invalid(err.to_string()))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|err| TraceError::Io {
                path: path.to_path_buf(),
                message: err.to_string(),
            })?;
        file.write_all(&encoded).map_err(|err| TraceError::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
        file.write_all(b"\n").map_err(|err| TraceError::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
        Ok(())
    }
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, TraceError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = std::fs::read_to_string(path).map_err(|err| TraceError::Io {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    let mut items = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        items.push(
            serde_json::from_str(line)
                .map_err(|err| TraceError::Invalid(format!("line {}: {err}", index + 1)))?,
        );
    }
    Ok(items)
}

fn finetune_messages(trajectory: &Trajectory) -> Vec<Value> {
    let role = trajectory.role.as_deref().unwrap_or("employee");
    let mut system = format!(
        "You are an Ironclaw employee agent ({role}, id `{id}`). Tools replace the work of this role. Use tools to do the job. Memory is your specialized knowledge about this work.",
        id = trajectory.agent_id
    );
    if !trajectory.memory.is_empty() {
        system.push_str("\n\nSpecialized memory:\n");
        system.push_str(&trajectory.memory);
    }
    let mut messages = vec![
        serde_json::json!({"role": "system", "content": system}),
        serde_json::json!({"role": "user", "content": trajectory.user}),
    ];
    for step in &trajectory.steps {
        let call_id = format!("ironclaw-step-{}", step.iteration);
        messages.push(serde_json::json!({
            "role": "assistant",
            "tool_calls": [{
                "id": call_id,
                "type": "function",
                "function": {
                    "name": step.name,
                    "arguments": step.input
                }
            }]
        }));
        messages.push(serde_json::json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": serde_json::json!({"ok": step.ok, "output": step.output}).to_string()
        }));
    }
    if let Some(answer) = &trajectory.answer {
        messages.push(serde_json::json!({"role": "assistant", "content": answer}));
    }
    messages
}

fn sanitize_step(step: TraceToolStep) -> TraceToolStep {
    TraceToolStep {
        iteration: step.iteration,
        name: step.name,
        input: redact_and_truncate(&step.input),
        ok: step.ok,
        output: redact_and_truncate(&step.output),
    }
}

fn redact_and_truncate(input: &str) -> String {
    truncate(&redact(input))
}

fn truncate(input: &str) -> String {
    if input.chars().count() <= MAX_FIELD_CHARS {
        return input.to_string();
    }
    let mut truncated: String = input.chars().take(MAX_FIELD_CHARS).collect();
    truncated.push_str("…[truncated]");
    truncated
}

fn redact(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut remaining = input;
    while !remaining.is_empty() {
        if let Some(len) = secret_prefix_len(remaining) {
            output.push_str("[redacted]");
            remaining = &remaining[len..];
            continue;
        }
        let ch = remaining.chars().next().expect("remaining is non-empty");
        output.push(ch);
        remaining = &remaining[ch.len_utf8()..];
    }
    output
}

fn secret_prefix_len(input: &str) -> Option<usize> {
    for prefix in ["sk-", "rk-", "Bearer "] {
        if input.len() >= prefix.len() && input[..prefix.len()].eq_ignore_ascii_case(prefix) {
            let rest = &input.as_bytes()[prefix.len()..];
            let mut consumed = 0usize;
            while consumed < rest.len() && is_secret_byte(rest[consumed]) {
                consumed += 1;
            }
            if consumed > 8 {
                return Some(prefix.len() + consumed);
            }
        }
    }
    None
}

fn is_secret_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+' | b'/' | b'=')
}

fn safe_agent_id(agent_id: &str) -> Result<String, TraceError> {
    if agent_id.is_empty()
        || agent_id.len() > 64
        || !agent_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(TraceError::Invalid(format!(
            "agent id is not a safe trace directory name: {agent_id}"
        )));
    }
    Ok(agent_id.to_string())
}

fn fingerprint(text: &str) -> String {
    hex::encode(sha2::Sha256::digest(text.as_bytes()))
}

fn new_trace_id(recorded_at_ms: u64) -> String {
    format!("tr-{recorded_at_ms}-{:08x}", cheap_rand())
}

fn cheap_rand() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| (duration.as_nanos() as u32).rotate_left(7))
        .unwrap_or(1)
}

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("invalid agentic trace: {0}")]
    Invalid(String),
    #[error("agentic trace lock poisoned")]
    Poisoned,
    #[error("agentic trace I/O failed at {path}: {message}")]
    Io { path: PathBuf, message: String },
}

pub fn infer_channel(session_id: &str, task_id: Option<&str>) -> &'static str {
    if task_id.is_some() {
        return "a2a";
    }
    if session_id.starts_with("telegram") {
        return "telegram";
    }
    if session_id.starts_with("whatsapp") {
        return "whatsapp";
    }
    if session_id.starts_with("cli") {
        return "cli";
    }
    "websocket"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(user: &str, observations: Vec<TraceToolStep>, answer: Option<&str>) -> PlanRecord {
        PlanRecord {
            agent_id: "backend-engineer".to_string(),
            role: Some("Backend Engineer".to_string()),
            session_id: "context-1".to_string(),
            task_id: Some("task-1".to_string()),
            skill: Some("implement_backend".to_string()),
            channel: "a2a".to_string(),
            model: "minimax/minimax-m2.5".to_string(),
            user_text: user.to_string(),
            memory: "constellation code is amber-lantern-4821".to_string(),
            observations,
            planner: "llm".to_string(),
            plan_tool: None,
            plan_answer: answer.map(str::to_string),
            error: None,
            recorded_at_ms: 10,
        }
    }

    #[test]
    fn records_a_complete_tool_trajectory_for_finetuning() {
        let temp = tempfile::tempdir().unwrap();
        let store = TraceStore::open(temp.path()).unwrap();
        let mut first = plan("Implement metering", Vec::new(), None);
        first.plan_tool = Some(("file_read".into(), "src/api.rs".into()));
        store.record_plan(first).unwrap();

        store
            .record_tool(ToolRecord {
                agent_id: "backend-engineer".into(),
                role: Some("Backend Engineer".into()),
                session_id: "context-1".into(),
                task_id: Some("task-1".into()),
                skill: Some("implement_backend".into()),
                channel: "a2a".into(),
                model: None,
                tool: "file_read".into(),
                input: "src/api.rs".into(),
                ok: true,
                output: "fn meter() {}".into(),
                recorded_at_ms: 11,
            })
            .unwrap();

        let mut complete = plan(
            "Implement metering",
            vec![TraceToolStep {
                iteration: 1,
                name: "file_read".into(),
                input: "src/api.rs".into(),
                ok: true,
                output: "fn meter() {}".into(),
            }],
            Some("Metering endpoint added."),
        );
        complete.recorded_at_ms = 12;
        store.record_plan(complete).unwrap();

        let trajectories = store.read_trajectories("backend-engineer").unwrap();
        assert_eq!(trajectories.len(), 1);
        let trajectory = &trajectories[0];
        assert_eq!(trajectory.outcome, "answer");
        assert_eq!(trajectory.steps.len(), 1);
        assert_eq!(
            trajectory.answer.as_deref(),
            Some("Metering endpoint added.")
        );
        assert_eq!(trajectory.messages[0]["role"], "system");
        assert!(trajectory.messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("Tools replace the work"));
        assert_eq!(trajectory.messages[1]["role"], "user");
        assert_eq!(trajectory.messages[2]["role"], "assistant");
        assert_eq!(trajectory.messages[3]["role"], "tool");
        assert_eq!(trajectory.messages[4]["role"], "assistant");
        assert_eq!(
            store.read_events("backend-engineer").unwrap()[0].kind,
            "plan"
        );
    }

    #[test]
    fn redacts_provider_secrets_and_closes_interrupted_turns() {
        let temp = tempfile::tempdir().unwrap();
        let store = TraceStore::open(temp.path()).unwrap();
        let mut first = plan(
            "use key sk-proj-abcdefghijklmnopqrstuvwxyz012345",
            Vec::new(),
            None,
        );
        first.plan_tool = Some(("bash".into(), "echo Bearer abcdefghijklmnop".into()));
        store.record_plan(first).unwrap();

        let mut next = plan("a new request", Vec::new(), Some("done"));
        next.recorded_at_ms = 20;
        store.record_plan(next).unwrap();

        let trajectories = store.read_trajectories("backend-engineer").unwrap();
        assert_eq!(trajectories.len(), 2);
        assert_eq!(trajectories[0].outcome, "interrupted");
        assert!(trajectories[0].user.contains("[redacted]"));
        assert!(!trajectories[0].user.contains("sk-proj-"));
        assert_eq!(trajectories[1].outcome, "answer");
    }

    #[test]
    fn infer_channel_prefers_a2a_then_named_sessions() {
        assert_eq!(infer_channel("ws-1", Some("task")), "a2a");
        assert_eq!(infer_channel("telegram-7", None), "telegram");
        assert_eq!(infer_channel("whatsapp-jid", None), "whatsapp");
        assert_eq!(infer_channel("cli", None), "cli");
        assert_eq!(infer_channel("owner", None), "websocket");
    }
}
