use crate::logging::LogLevel;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostConfig {
    pub server: HostServerConfig,
    pub ui: HostUiConfig,
    pub firecracker: HostFirecrackerConfig,
    pub storage: HostStorageConfig,
    pub llm: HostLlmConfig,
    #[serde(default)]
    pub telegram: HostTelegramConfig,
    #[serde(default)]
    pub whatsapp: HostWhatsAppConfig,
    #[serde(default = "default_execution_mode")]
    pub execution_mode: HostExecutionMode,
    #[serde(default = "default_idle_timeout_minutes")]
    pub idle_timeout_minutes: u64,
    #[serde(default = "default_log_level")]
    pub log_level: LogLevel,
    #[serde(default)]
    pub daemon: HostDaemonConfig,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostExecutionMode {
    Auto,
    HostOnly,
    GuestTools,
    GuestAutonomous,
}

fn default_execution_mode() -> HostExecutionMode {
    HostExecutionMode::Auto
}

fn default_idle_timeout_minutes() -> u64 {
    5
}

fn default_log_level() -> LogLevel {
    LogLevel::Info
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostLlmApi {
    ChatCompletions,
    Responses,
}

impl HostLlmApi {
    pub fn path(self) -> &'static str {
        match self {
            Self::ChatCompletions => "/chat/completions",
            Self::Responses => "/responses",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostLlmConfig {
    pub model: String,
    pub base_url: String,
    #[serde(default = "default_host_llm_api")]
    pub api: HostLlmApi,
}

fn default_host_llm_api() -> HostLlmApi {
    HostLlmApi::Responses
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostServerConfig {
    pub bind: String,
    pub port: u16,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostUiConfig {
    pub mount: String,
    pub index_file: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostFirecrackerConfig {
    pub enabled: bool,
    pub kernel_path: PathBuf,
    pub rootfs_path: PathBuf,
    pub api_socket_dir: PathBuf,
    /// Directory for Firecracker vsock UDS endpoints.
    #[serde(default)]
    pub vsock_uds_dir: Option<PathBuf>,
    /// Guest port used for host-guest transport.
    #[serde(default)]
    pub vsock_port: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostStorageConfig {
    pub users_root: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostDaemonConfig {
    #[serde(default)]
    pub pid_file: Option<PathBuf>,
    #[serde(default)]
    pub log_file: Option<PathBuf>,
    #[serde(default = "default_graceful_timeout_ms")]
    pub graceful_timeout_ms: u64,
    #[serde(default = "default_log_rotate_keep")]
    pub log_rotate_keep: usize,
    #[serde(default = "default_log_rotate_max_bytes")]
    pub log_rotate_max_bytes: u64,
}

fn default_graceful_timeout_ms() -> u64 {
    5000
}

fn default_log_rotate_keep() -> usize {
    5
}

fn default_log_rotate_max_bytes() -> u64 {
    10 * 1024 * 1024
}

impl Default for HostDaemonConfig {
    fn default() -> Self {
        Self {
            pid_file: None,
            log_file: None,
            graceful_timeout_ms: default_graceful_timeout_ms(),
            log_rotate_keep: default_log_rotate_keep(),
            log_rotate_max_bytes: default_log_rotate_max_bytes(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostTelegramConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token: Option<String>,
    #[serde(default)]
    pub owner_chat_id: Option<i64>,
    #[serde(default = "default_telegram_poll_timeout_seconds")]
    pub poll_timeout_seconds: u64,
}

fn default_telegram_poll_timeout_seconds() -> u64 {
    30
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostWhatsAppConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_whatsapp_session_db_path")]
    pub session_db_path: String,
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default = "default_whatsapp_self_chat")]
    pub self_chat_enabled: bool,
}

fn default_whatsapp_session_db_path() -> String {
    "data/whatsapp_session.db".to_string()
}

fn default_whatsapp_self_chat() -> bool {
    true
}

impl Default for HostWhatsAppConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            session_db_path: default_whatsapp_session_db_path(),
            allowlist: vec![],
            self_chat_enabled: default_whatsapp_self_chat(),
        }
    }
}

impl Default for HostTelegramConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: None,
            owner_chat_id: None,
            poll_timeout_seconds: default_telegram_poll_timeout_seconds(),
        }
    }
}

impl HostConfig {
    pub fn default_for_local(users_root: PathBuf) -> Self {
        Self {
            server: HostServerConfig {
                bind: "127.0.0.1".to_string(),
                port: 9938,
            },
            ui: HostUiConfig {
                mount: "/ui".to_string(),
                index_file: "index.html".to_string(),
            },
            firecracker: HostFirecrackerConfig {
                enabled: false,
                kernel_path: PathBuf::from("kernels/vmlinux"),
                rootfs_path: PathBuf::from("rootfs/ironclaw.ext4"),
                api_socket_dir: PathBuf::from("/tmp/ironclaw-fc"),
                vsock_uds_dir: Some(PathBuf::from("/tmp/ironclaw/vsock")),
                vsock_port: Some(5000),
            },
            storage: HostStorageConfig { users_root },
            llm: HostLlmConfig {
                model: "gpt-5-mini".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                api: HostLlmApi::Responses,
            },
            telegram: HostTelegramConfig::default(),
            whatsapp: HostWhatsAppConfig::default(),
            execution_mode: HostExecutionMode::Auto,
            idle_timeout_minutes: default_idle_timeout_minutes(),
            log_level: default_log_level(),
            daemon: HostDaemonConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GuestConfig {
    pub default_agent: String,
    pub tools: GuestToolsConfig,
    pub indexing: GuestIndexingConfig,
    pub scheduler: GuestSchedulerConfig,
    #[serde(default = "default_log_level")]
    pub log_level: LogLevel,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GuestToolsConfig {
    pub allow_bash: bool,
    pub allow_file: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GuestIndexingConfig {
    #[serde(default = "default_max_chunk_bytes")]
    pub max_chunk_bytes: usize,
    #[serde(default = "default_embedding_model", alias = "embedding_model_name")]
    pub embedding_model: String,
    #[serde(default = "default_vector_weight", alias = "semantic_weight")]
    pub vector_weight: f32,
    #[serde(default = "default_keyword_weight", alias = "lexical_weight")]
    pub keyword_weight: f32,
    #[serde(default = "default_embedding_cache_size")]
    pub embedding_cache_size: usize,
}

fn default_max_chunk_bytes() -> usize {
    2048
}

fn default_embedding_model() -> String {
    "text-embedding-3-small".to_string()
}

fn default_vector_weight() -> f32 {
    0.7
}

fn default_keyword_weight() -> f32 {
    0.3
}

fn default_embedding_cache_size() -> usize {
    1000
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GuestSchedulerConfig {
    pub jobs_path: PathBuf,
}

impl Default for GuestConfig {
    fn default() -> Self {
        Self {
            default_agent: "default".to_string(),
            tools: GuestToolsConfig {
                allow_bash: false,
                allow_file: true,
            },
            indexing: GuestIndexingConfig {
                max_chunk_bytes: default_max_chunk_bytes(),
                embedding_model: default_embedding_model(),
                vector_weight: default_vector_weight(),
                keyword_weight: default_keyword_weight(),
                embedding_cache_size: default_embedding_cache_size(),
            },
            scheduler: GuestSchedulerConfig {
                jobs_path: PathBuf::from("/mnt/brain/cron/jobs.toml"),
            },
            log_level: default_log_level(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentProfile {
    pub name: String,
    pub persona_path: PathBuf,
    pub model_provider: String,
    pub model_name: String,
    pub tool_permissions: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JobsConfig {
    pub jobs: Vec<JobDefinition>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JobDefinition {
    pub id: String,
    pub schedule: String,
    pub description: Option<String>,
    pub task: String,
}
