use common::config::{GuestConfig, JobsConfig};
use common::proto::ironclaw::{message_envelope, MessageEnvelope};
use common::transport::Transport;
use memory::{hybrid_fusion, index_chunks, initialize_schema, lexical_search, Chunk};
use rusqlite::Connection;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tools::{FileReadTool, FileWriteTool, RestrictedBashTool, ToolRegistry, ToolResult};

#[derive(Debug, thiserror::Error)]
#[error("irowclaw error: {message}")]
pub struct IrowclawError {
    message: String,
}

impl IrowclawError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub struct Runtime {
    config: GuestConfig,
    _brain: BrainPaths,
    db: Connection,
    tool_registry: ToolRegistry,
    safety: SafetyLayer,
}

impl Runtime {
    pub fn load(config_path: &Path) -> Result<Self, IrowclawError> {
        let config = load_guest_config(config_path)?;
        let brain = BrainPaths::new(default_brain_root());
        brain.ensure_dirs()?;
        let db = Connection::open(&brain.db_path)
            .map_err(|err| IrowclawError::new(format!("db open failed: {err}")))?;
        initialize_schema(&db)
            .map_err(|err| IrowclawError::new(format!("db schema failed: {err}")))?;

        let workspace_root = brain.root.join("workspace");
        std::fs::create_dir_all(&workspace_root)
            .map_err(|err| IrowclawError::new(format!("workspace create failed: {err}")))?;

        let default_allowed = default_allowed_tools(&config);
        let mut tool_registry = ToolRegistry::new(&default_allowed);
        tool_registry.register(
            "file_read",
            Box::new(FileReadTool::new(workspace_root.clone())),
        );
        tool_registry.register("file_write", Box::new(FileWriteTool::new(workspace_root)));
        tool_registry.register(
            "bash",
            Box::new(RestrictedBashTool::new(config.tools.allow_bash)),
        );

        Ok(Self {
            config,
            _brain: brain,
            db,
            tool_registry,
            safety: SafetyLayer::new(),
        })
    }

    pub fn load_default() -> Result<Self, IrowclawError> {
        let config_path = PathBuf::from("/mnt/brain/config/irowclaw.toml");
        Self::load(&config_path)
    }

    pub fn set_allowed_tools(&mut self, tools: &[String]) {
        self.tool_registry.set_allowed_tools(tools);
    }

    pub fn load_jobs(&self) -> Result<JobsConfig, IrowclawError> {
        let path = &self.config.scheduler.jobs_path;
        let contents = std::fs::read_to_string(path)
            .map_err(|err| IrowclawError::new(format!("jobs read failed: {err}")))?;
        toml::from_str(&contents)
            .map_err(|err| IrowclawError::new(format!("jobs parse failed: {err}")))
    }

    pub fn index_markdown(&self, path: &str, contents: &str) -> Result<Vec<Chunk>, IrowclawError> {
        let chunks = memory::chunk_markdown(path, contents, self.config.indexing.max_chunk_bytes);
        index_chunks(&self.db, &chunks)
            .map_err(|err| IrowclawError::new(format!("index failed: {err}")))?;
        Ok(chunks)
    }

    pub fn hybrid_retrieval(&self, query: &str, limit: usize) -> Result<String, IrowclawError> {
        let lexical = lexical_search(&self.db, query, limit)
            .map_err(|err| IrowclawError::new(format!("lexical search failed: {err}")))?;
        let semantic = memory::semantic_search_stub(&self.db, &[], limit)
            .map_err(|err| IrowclawError::new(format!("semantic search failed: {err}")))?;
        let results = hybrid_fusion(
            lexical,
            semantic,
            self.config.indexing.semantic_weight,
            self.config.indexing.lexical_weight,
        );
        let mut summary = String::new();
        for result in results {
            summary.push_str(&format!(
                "{path}::{heading}::{ordinal} score={score:.3}\n",
                path = result.chunk.path,
                heading = result.chunk.heading,
                ordinal = result.chunk.ordinal,
                score = result.score
            ));
        }
        Ok(summary)
    }

    pub fn execute_tool_checked(&self, tool: &str, input: &str) -> ToolResult {
        if self.safety.scan_prompt_injection(input).is_some() {
            return ToolResult {
                output: "blocked by policy: tool input matched injection heuristic".to_string(),
                ok: false,
            };
        }
        if let PolicyDecision::Deny(reason) = self.safety.evaluate_policy(input) {
            return ToolResult {
                output: format!("blocked by policy: {reason}"),
                ok: false,
            };
        }

        let raw = self.tool_registry.execute(tool, input);
        let mut result = match raw {
            Ok(value) => value,
            Err(err) => ToolResult {
                output: err.to_string(),
                ok: false,
            },
        };

        if let Some(reason) = self.safety.scan_leak(&result.output) {
            result.output = format!("blocked by leak detector: {reason}");
            result.ok = false;
        }

        result
    }
}

pub async fn run_with_transport<T: Transport + 'static>(
    mut transport: T,
    config_path: PathBuf,
) -> Result<(), IrowclawError> {
    let mut runtime = Runtime::load(&config_path)?;

    // auth handshake: wait for host challenge and reply with ack.
    let (cap_token, mode) = match transport.recv().await {
        Ok(Some(message)) => match message.payload {
            Some(message_envelope::Payload::AuthChallenge(ch)) => {
                let token = ch.cap_token.clone();
                runtime.set_allowed_tools(&ch.allowed_tools);
                let mode = GuestExecutionMode::from_wire(&ch.execution_mode);
                transport
                    .send(MessageEnvelope {
                        user_id: message.user_id,
                        session_id: message.session_id,
                        msg_id: message.msg_id,
                        timestamp_ms: now_ms()?,
                        cap_token: token.clone(),
                        payload: Some(message_envelope::Payload::AuthAck(
                            common::proto::ironclaw::AuthAck {
                                cap_token: token.clone(),
                            },
                        )),
                    })
                    .await
                    .map_err(|err| IrowclawError::new(err.to_string()))?;
                (token, mode)
            }
            other => {
                return Err(IrowclawError::new(format!(
                    "expected authchallenge, got {other:?}"
                )));
            }
        },
        Ok(None) => return Ok(()),
        Err(err) => return Err(IrowclawError::new(err.to_string())),
    };

    let mut internal_call_id = 1u64;

    loop {
        let message = match transport.recv().await {
            Ok(Some(message)) => message,
            Ok(None) => break,
            Err(err) => return Err(IrowclawError::new(err.to_string())),
        };

        let response = match message.payload.clone() {
            Some(message_envelope::Payload::UserMessage(um)) => {
                let text = um.text.trim().to_string();

                if runtime.safety.scan_prompt_injection(&text).is_some() {
                    build_user_reply(
                        &message,
                        &cap_token,
                        "blocked by policy: prompt injection detected",
                        &runtime,
                    )?
                } else {
                    let mut blocked = None;
                    match runtime.safety.evaluate_policy(&text) {
                        PolicyDecision::Deny(reason) => {
                            blocked = Some(format!("blocked by policy: {reason}"));
                        }
                        PolicyDecision::RequireConfirmation(reason) => {
                            if !text.contains("[confirm]") {
                                blocked = Some(format!(
                                    "confirmation required: {reason}. append [confirm] to proceed."
                                ));
                            }
                        }
                        PolicyDecision::Allow => {}
                    }

                    if let Some(message_text) = blocked {
                        build_user_reply(&message, &cap_token, &message_text, &runtime)?
                    } else {
                        let action = match mode {
                            GuestExecutionMode::HostOnly => GuestPlan::Answer {
                                text: format!("stub: {text}"),
                            },
                            GuestExecutionMode::GuestTools => {
                                request_host_plan(
                                    &mut transport,
                                    &cap_token,
                                    &message,
                                    &text,
                                    &mut internal_call_id,
                                )
                                .await?
                            }
                            GuestExecutionMode::GuestAutonomous => plan_autonomous(&text),
                        };

                        let response_text = match action {
                            GuestPlan::Answer { text } => text,
                            GuestPlan::Tool { tool, input } => {
                                let tool_result = runtime.execute_tool_checked(&tool, &input);
                                if tool_result.ok {
                                    format!("guest tool {tool}: {}", tool_result.output)
                                } else {
                                    format!("guest tool {tool} failed: {}", tool_result.output)
                                }
                            }
                        };
                        build_user_reply(&message, &cap_token, &response_text, &runtime)?
                    }
                }
            }
            Some(message_envelope::Payload::ToolCallRequest(req)) => {
                let tool_result = runtime.execute_tool_checked(&req.tool, &req.input);
                Some(MessageEnvelope {
                    user_id: message.user_id,
                    session_id: message.session_id,
                    msg_id: message.msg_id,
                    timestamp_ms: now_ms()?,
                    cap_token: cap_token.clone(),
                    payload: Some(message_envelope::Payload::ToolCallResponse(
                        common::proto::ironclaw::ToolCallResponse {
                            call_id: req.call_id,
                            ok: tool_result.ok,
                            output: runtime.safety.sanitize_outbound(&tool_result.output),
                        },
                    )),
                })
            }
            Some(message_envelope::Payload::FileOpRequest(req)) => {
                let tool = if req.op == "read" {
                    "file_read"
                } else if req.op == "write" {
                    "file_write"
                } else {
                    ""
                };
                let input = if tool == "file_read" {
                    req.path
                } else if tool == "file_write" {
                    format!("{}\n{}", req.path, req.data.unwrap_or_default())
                } else {
                    String::new()
                };
                let tool_result = if tool.is_empty() {
                    ToolResult {
                        output: "unknown file op".to_string(),
                        ok: false,
                    }
                } else {
                    runtime.execute_tool_checked(tool, &input)
                };
                Some(MessageEnvelope {
                    user_id: message.user_id,
                    session_id: message.session_id,
                    msg_id: message.msg_id,
                    timestamp_ms: now_ms()?,
                    cap_token: cap_token.clone(),
                    payload: Some(message_envelope::Payload::ToolResult(
                        common::proto::ironclaw::ToolResult {
                            tool: tool.to_string(),
                            output: runtime.safety.sanitize_outbound(&tool_result.output),
                            ok: tool_result.ok,
                        },
                    )),
                })
            }
            _ => None,
        };

        if let Some(response) = response {
            transport
                .send(response)
                .await
                .map_err(|err| IrowclawError::new(err.to_string()))?;
        }
    }
    Ok(())
}

fn build_user_reply(
    source: &MessageEnvelope,
    cap_token: &str,
    text: &str,
    runtime: &Runtime,
) -> Result<Option<MessageEnvelope>, IrowclawError> {
    let safe = runtime.safety.sanitize_outbound(text);
    Ok(Some(build_stream_delta(source, cap_token, safe, true)?))
}

async fn request_host_plan<T: Transport>(
    transport: &mut T,
    cap_token: &str,
    source: &MessageEnvelope,
    user_text: &str,
    internal_call_id: &mut u64,
) -> Result<GuestPlan, IrowclawError> {
    let call_id = *internal_call_id;
    *internal_call_id = internal_call_id.saturating_add(1);

    transport
        .send(MessageEnvelope {
            user_id: source.user_id.clone(),
            session_id: source.session_id.clone(),
            msg_id: source.msg_id,
            timestamp_ms: now_ms()?,
            cap_token: cap_token.to_string(),
            payload: Some(message_envelope::Payload::ToolCallRequest(
                common::proto::ironclaw::ToolCallRequest {
                    call_id,
                    tool: "host_plan".to_string(),
                    input: user_text.to_string(),
                },
            )),
        })
        .await
        .map_err(|err| IrowclawError::new(format!("host plan send failed: {err}")))?;

    let response = tokio::time::timeout(std::time::Duration::from_secs(10), transport.recv())
        .await
        .map_err(|_| IrowclawError::new("host plan timed out"))?
        .map_err(|err| IrowclawError::new(format!("host plan recv failed: {err}")))?;

    let Some(envelope) = response else {
        return Err(IrowclawError::new("host plan channel closed"));
    };

    match envelope.payload {
        Some(message_envelope::Payload::ToolCallResponse(resp)) => {
            if resp.call_id != call_id {
                return Err(IrowclawError::new("host plan call id mismatch"));
            }
            if !resp.ok {
                return Err(IrowclawError::new(format!(
                    "host plan failed: {}",
                    resp.output
                )));
            }
            parse_guest_plan(&resp.output)
        }
        _ => Err(IrowclawError::new("host plan response missing")),
    }
}

fn plan_autonomous(text: &str) -> GuestPlan {
    if let Some(path) = text.strip_prefix("read ") {
        return GuestPlan::Tool {
            tool: "file_read".to_string(),
            input: path.trim().to_string(),
        };
    }
    if let Some(path) = text.strip_prefix("!read ") {
        return GuestPlan::Tool {
            tool: "file_read".to_string(),
            input: path.trim().to_string(),
        };
    }

    if let Some(rest) = text.strip_prefix("write ") {
        return parse_write_plan(rest);
    }
    if let Some(rest) = text.strip_prefix("!write ") {
        return parse_write_plan(rest);
    }

    GuestPlan::Answer {
        text: format!("guest: {text}"),
    }
}

fn parse_write_plan(rest: &str) -> GuestPlan {
    let mut parts = rest.splitn(2, '\n');
    let path = parts.next().unwrap_or("").trim();
    let contents = parts.next().unwrap_or("");
    if path.is_empty() {
        return GuestPlan::Answer {
            text: "missing path for write".to_string(),
        };
    }
    GuestPlan::Tool {
        tool: "file_write".to_string(),
        input: format!("{path}\n{contents}"),
    }
}

#[derive(Clone, Debug)]
enum GuestExecutionMode {
    HostOnly,
    GuestTools,
    GuestAutonomous,
}

impl GuestExecutionMode {
    fn from_wire(value: &str) -> Self {
        match value {
            "host_only" => Self::HostOnly,
            "guest_autonomous" => Self::GuestAutonomous,
            "guest_tools" => Self::GuestTools,
            _ => Self::GuestTools,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum GuestPlan {
    Tool { tool: String, input: String },
    Answer { text: String },
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
enum RawGuestPlan {
    Tool { tool: String, input: String },
    Answer { text: String },
}

fn parse_guest_plan(raw: &str) -> Result<GuestPlan, IrowclawError> {
    let json = extract_json_object(raw).ok_or_else(|| IrowclawError::new("missing json plan"))?;
    let parsed: RawGuestPlan = serde_json::from_str(json)
        .map_err(|err| IrowclawError::new(format!("host plan parse failed: {err}")))?;
    match parsed {
        RawGuestPlan::Tool { tool, input } => Ok(GuestPlan::Tool {
            tool: tool.trim().to_string(),
            input,
        }),
        RawGuestPlan::Answer { text } => Ok(GuestPlan::Answer { text }),
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

#[derive(Clone)]
struct SafetyLayer {
    injection_patterns: Vec<&'static str>,
    deny_patterns: Vec<&'static str>,
    confirm_patterns: Vec<&'static str>,
    leak_patterns: Vec<&'static str>,
}

impl SafetyLayer {
    fn new() -> Self {
        Self {
            injection_patterns: vec![
                "ignore previous instructions",
                "reveal system prompt",
                "bypass policy",
                "disable safety",
                "developer mode",
            ],
            deny_patterns: vec![
                "steal secrets",
                "exfiltrate",
                "cat /etc/shadow",
                "private key",
            ],
            confirm_patterns: vec!["rm -rf", "drop table", "delete all", "format disk"],
            leak_patterns: vec![
                "sk-",
                "api_key=",
                "authorization: bearer",
                "-----begin private key-----",
                "fake_secret_",
            ],
        }
    }

    fn scan_prompt_injection(&self, input: &str) -> Option<String> {
        let lowered = input.to_lowercase();
        self.injection_patterns
            .iter()
            .find(|pattern| lowered.contains(**pattern))
            .map(|pattern| (*pattern).to_string())
    }

    fn evaluate_policy(&self, input: &str) -> PolicyDecision {
        let lowered = input.to_lowercase();
        if let Some(pattern) = self
            .deny_patterns
            .iter()
            .find(|pattern| lowered.contains(**pattern))
        {
            return PolicyDecision::Deny((*pattern).to_string());
        }
        if let Some(pattern) = self
            .confirm_patterns
            .iter()
            .find(|pattern| lowered.contains(**pattern))
        {
            return PolicyDecision::RequireConfirmation((*pattern).to_string());
        }
        PolicyDecision::Allow
    }

    fn scan_leak(&self, output: &str) -> Option<String> {
        let lowered = output.to_lowercase();
        self.leak_patterns
            .iter()
            .find(|pattern| lowered.contains(**pattern))
            .map(|pattern| (*pattern).to_string())
    }

    fn sanitize_outbound(&self, output: &str) -> String {
        if let Some(reason) = self.scan_leak(output) {
            return format!("blocked by leak detector: {reason}");
        }
        output.to_string()
    }
}

enum PolicyDecision {
    Allow,
    RequireConfirmation(String),
    Deny(String),
}

fn build_stream_delta(
    source: &MessageEnvelope,
    cap_token: &str,
    delta: String,
    done: bool,
) -> Result<MessageEnvelope, IrowclawError> {
    Ok(MessageEnvelope {
        user_id: source.user_id.clone(),
        session_id: source.session_id.clone(),
        msg_id: source.msg_id,
        timestamp_ms: now_ms()?,
        cap_token: cap_token.to_string(),
        payload: Some(message_envelope::Payload::StreamDelta(
            common::proto::ironclaw::StreamDelta { delta, done },
        )),
    })
}

fn load_guest_config(path: &Path) -> Result<GuestConfig, IrowclawError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents)
            .map_err(|err| IrowclawError::new(format!("config parse failed: {err}"))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(GuestConfig::default()),
        Err(err) => Err(IrowclawError::new(format!("config read failed: {err}"))),
    }
}

fn default_brain_root() -> PathBuf {
    if let Ok(root) = std::env::var("IRONCLAW_BRAIN_ROOT") {
        return PathBuf::from(root);
    }
    PathBuf::from("/mnt/brain")
}

fn now_ms() -> Result<u64, IrowclawError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| IrowclawError::new(format!("time error: {err}")))
        .map(|duration| duration.as_millis() as u64)
}

fn default_allowed_tools(config: &GuestConfig) -> Vec<String> {
    let mut tools = Vec::new();
    if config.tools.allow_file {
        tools.push("file_read".to_string());
        tools.push("file_write".to_string());
    }
    if config.tools.allow_bash {
        tools.push("bash".to_string());
    }
    tools
}

#[derive(Clone, Debug)]
pub struct BrainPaths {
    pub root: PathBuf,
    pub soul: PathBuf,
    pub identity: PathBuf,
    pub instructions: PathBuf,
    pub memory: PathBuf,
    pub logs: PathBuf,
    pub cron: PathBuf,
    pub config: PathBuf,
    pub db: PathBuf,
    pub db_path: PathBuf,
}

impl BrainPaths {
    pub fn new(root: PathBuf) -> Self {
        let soul = root.join("soul.md");
        let identity = root.join("identity.md");
        let instructions = root.join("instructions");
        let memory = root.join("memory.md");
        let logs = root.join("logs");
        let cron = root.join("cron");
        let config = root.join("config");
        let db = root.join("db");
        let db_path = db.join("ironclaw.db");
        Self {
            root,
            soul,
            identity,
            instructions,
            memory,
            logs,
            cron,
            config,
            db,
            db_path,
        }
    }

    pub fn ensure_dirs(&self) -> Result<(), IrowclawError> {
        std::fs::create_dir_all(&self.instructions)
            .map_err(|err| IrowclawError::new(format!("create instructions failed: {err}")))?;
        std::fs::create_dir_all(&self.logs)
            .map_err(|err| IrowclawError::new(format!("create logs failed: {err}")))?;
        std::fs::create_dir_all(&self.cron)
            .map_err(|err| IrowclawError::new(format!("create cron failed: {err}")))?;
        std::fs::create_dir_all(&self.config)
            .map_err(|err| IrowclawError::new(format!("create config failed: {err}")))?;
        std::fs::create_dir_all(&self.db)
            .map_err(|err| IrowclawError::new(format!("create db failed: {err}")))?;
        self.ensure_file(&self.soul, "")?;
        self.ensure_file(&self.identity, "")?;
        self.ensure_file(&self.memory, "")?;
        let jobs_path = self.cron.join("jobs.toml");
        self.ensure_file(&jobs_path, "jobs = []\n")?;
        Ok(())
    }

    fn ensure_file(&self, path: &Path, contents: &str) -> Result<(), IrowclawError> {
        if path.exists() {
            return Ok(());
        }
        std::fs::write(path, contents)
            .map_err(|err| IrowclawError::new(format!("write file failed: {err}")))
    }
}

#[cfg(test)]
#[path = "runtime_loop_test.rs"]
mod runtime_loop_test;
#[cfg(test)]
#[path = "safety_test.rs"]
mod safety_test;
