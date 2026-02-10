use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostConfig {
    pub server: HostServerConfig,
    pub ui: HostUiConfig,
    pub firecracker: HostFirecrackerConfig,
    pub storage: HostStorageConfig,
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
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HostStorageConfig {
    pub users_root: PathBuf,
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
            },
            storage: HostStorageConfig { users_root },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GuestConfig {
    pub default_agent: String,
    pub tools: GuestToolsConfig,
    pub indexing: GuestIndexingConfig,
    pub scheduler: GuestSchedulerConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GuestToolsConfig {
    pub allow_bash: bool,
    pub allow_file: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GuestIndexingConfig {
    pub max_chunk_bytes: usize,
    pub semantic_weight: f32,
    pub lexical_weight: f32,
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
                max_chunk_bytes: 2048,
                semantic_weight: 0.7,
                lexical_weight: 0.3,
            },
            scheduler: GuestSchedulerConfig {
                jobs_path: PathBuf::from("/mnt/brain/cron/jobs.toml"),
            },
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
