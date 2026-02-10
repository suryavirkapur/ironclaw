use common::config::{GuestConfig, JobsConfig};
use common::protocol::{MessageEnvelope, MessagePayload};
use common::transport::Transport;
use memory::{hybrid_fusion, index_chunks, initialize_schema, lexical_search, Chunk};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tools::{BashTool, FileTool, StubBashTool, StubFileTool, ToolResult};

#[derive(Debug, thiserror::Error)]
#[error("irowclaw error: {message}")]
pub struct IrowclawError {
    message: String,
}

impl IrowclawError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub struct Runtime {
    config: GuestConfig,
    _brain: BrainPaths,
    db: Connection,
    bash_tool: Box<dyn BashTool>,
    file_tool: Box<dyn FileTool>,
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
        let bash_tool: Box<dyn BashTool> = Box::new(StubBashTool);
        let file_tool: Box<dyn FileTool> = Box::new(StubFileTool::new(brain.root.clone()));
        Ok(Self {
            config,
            _brain: brain,
            db,
            bash_tool,
            file_tool,
        })
    }

    pub fn load_default() -> Result<Self, IrowclawError> {
        let config_path = PathBuf::from("/mnt/brain/config/irowclaw.toml");
        Self::load(&config_path)
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

    pub fn handle_tool_request(
        &self,
        tool: &str,
        input: &str,
    ) -> Result<ToolResult, IrowclawError> {
        match tool {
            "bash" => self
                .bash_tool
                .run(input)
                .map_err(|err| IrowclawError::new(err.to_string())),
            "file_read" => {
                let data = self
                    .file_tool
                    .read(Path::new(input))
                    .map_err(|err| IrowclawError::new(err.to_string()))?;
                Ok(ToolResult {
                    output: data,
                    ok: true,
                })
            }
            _ => Err(IrowclawError::new("unknown tool")),
        }
    }
}

pub async fn run_with_transport<T: Transport + 'static>(
    mut transport: T,
    config_path: PathBuf,
) -> Result<(), IrowclawError> {
    let runtime = Runtime::load(&config_path)?;
    loop {
        let message = match transport.recv().await {
            Ok(Some(message)) => message,
            Ok(None) => break,
            Err(err) => return Err(IrowclawError::new(err.to_string())),
        };
        let response = handle_message(&runtime, message)?;
        if let Some(response) = response {
            transport
                .send(response)
                .await
                .map_err(|err| IrowclawError::new(err.to_string()))?;
        }
    }
    Ok(())
}

fn handle_message(
    runtime: &Runtime,
    message: MessageEnvelope,
) -> Result<Option<MessageEnvelope>, IrowclawError> {
    match message.payload {
        MessagePayload::UserMessage { ref text } => {
            let reply = format!("stub: {text}");
            let response = build_stream_delta(&message, reply, true)?;
            Ok(Some(response))
        }
        MessagePayload::FileOpRequest { op, path, data } => {
            let result = match op.as_str() {
                "read" => runtime
                    .file_tool
                    .read(Path::new(&path))
                    .map_err(|err| err.to_string())
                    .map(|data| ToolResult { output: data, ok: true }),
                "write" => {
                    let contents = data.unwrap_or_default();
                    runtime
                        .file_tool
                        .write(Path::new(&path), &contents)
                        .map_err(|err| err.to_string())
                        .map(|_| ToolResult {
                            output: "ok".to_string(),
                            ok: true,
                        })
                }
                _ => Err("unknown file op".to_string()),
            };
            let (output, ok) = match result {
                Ok(result) => (result.output, result.ok),
                Err(err) => (err, false),
            };
            Ok(Some(MessageEnvelope {
                user_id: message.user_id,
                session_id: message.session_id,
                msg_id: message.msg_id,
                timestamp_ms: now_ms()?,
                payload: MessagePayload::ToolResult {
                    tool: format!("file_{op}"),
                    output,
                    ok,
                },
            }))
        }
        _ => Ok(None),
    }
}

fn build_stream_delta(
    source: &MessageEnvelope,
    delta: String,
    done: bool,
) -> Result<MessageEnvelope, IrowclawError> {
    Ok(MessageEnvelope {
        user_id: source.user_id.clone(),
        session_id: source.session_id.clone(),
        msg_id: source.msg_id,
        timestamp_ms: now_ms()?,
        payload: MessagePayload::StreamDelta { delta, done },
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
