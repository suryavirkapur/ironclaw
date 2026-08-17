//! Default install home and config-rooted path loading.
//!
//! With no `--config`, Ironclaw uses `~/.config/ironclaw/ironclawd.toml`. The
//! first successful start writes that file and the layout around it, then reads
//! it. With `--config FILE` (or `IRONCLAWD_CONFIG`), FILE's parent directory is
//! the root for every relative path in the file.

use crate::IronclawError;
use common::config::HostConfig;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG_FILE_NAME: &str = "ironclawd.toml";

const LAYOUT_DIRS: &[&str] = &["users", "kernels", "rootfs", "agents", "run", "whatsapp"];

const DEFAULT_IRONCLAWD_TOML: &str = r#"# Ironclaw host config.
# Paths that are not absolute are relative to this file's directory.

execution_mode = "guest_tools"
idle_timeout_minutes = 15
log_level = "info"

[server]
bind = "127.0.0.1"
port = 9938

[ui]
mount = "/ui"
index_file = "index.html"

[storage]
users_root = "users"

[llm]
model = "minimax/minimax-m2.5"
base_url = "https://openrouter.ai/api/v1"
api = "chat_completions"

[firecracker]
enabled = true
kernel_path = "kernels/vmlinux.bin"
rootfs_path = "rootfs/ubuntu-24.04.ext4"
api_socket_dir = "run/fc"
vsock_uds_dir = "run/vsock"
vsock_port = 5000
vcpus = 2
memory_mib = 2048
disk_quota_mb = 512

[farm]
enabled = false
manifests_dir = "agents"
public_base_url = "http://127.0.0.1:9938"

[security]
allowed_channels = ["websocket"]

[security.network]
allowed_domains = []

[daemon]
pid_file = "run/ironclawd.pid"
log_file = "run/ironclawd.log"
graceful_timeout_ms = 5000
log_rotate_keep = 5
log_rotate_max_bytes = 10485760

[telegram]
enabled = false

[whatsapp]
enabled = false
session_dir = "whatsapp"
"#;

const DEFAULT_ASSISTANT_AGENT: &str = r#"id = "assistant"
name = "Assistant"
role = "Personal assistant"

[model]
provider = "openrouter"
model = "minimax/minimax-m2.5"

[[skills]]
id = "help"
description = "Help with tasks using this agent's private workspace and memory."
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostHomePrep {
    pub config_path: PathBuf,
    pub root: PathBuf,
    pub created_config: bool,
    pub created_dirs: Vec<PathBuf>,
    pub host_notes: Vec<String>,
}

pub fn default_config_path() -> Result<PathBuf, IronclawError> {
    let config_home = dirs::config_dir().ok_or_else(|| {
        IronclawError::new("could not resolve the user config directory (~/.config)")
    })?;
    Ok(config_home.join("ironclaw").join(DEFAULT_CONFIG_FILE_NAME))
}

pub fn resolve_config_path(cli_config: Option<&Path>) -> Result<PathBuf, IronclawError> {
    if let Some(path) = cli_config {
        return Ok(absolute_path(path)?);
    }
    if let Ok(path) = std::env::var("IRONCLAWD_CONFIG") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Ok(absolute_path(Path::new(trimmed))?);
        }
    }
    Ok(absolute_path(&default_config_path()?)?)
}

pub fn config_root(config_path: &Path) -> Result<PathBuf, IronclawError> {
    let absolute = absolute_path(config_path)?;
    absolute
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| IronclawError::new("config path has no parent directory"))
}

pub fn prepare_host_home(config_path: &Path) -> Result<HostHomePrep, IronclawError> {
    let config_path = absolute_path(config_path)?;
    let root = config_root(&config_path)?;
    ensure_dir(&root)?;

    let mut created_dirs = Vec::new();
    let created_config = if config_path.exists() {
        false
    } else {
        for name in LAYOUT_DIRS {
            let dir = root.join(name);
            ensure_dir(&dir)?;
            created_dirs.push(dir);
        }
        std::fs::write(&config_path, DEFAULT_IRONCLAWD_TOML)
            .map_err(|err| IronclawError::new(format!("write config failed: {err}")))?;
        write_default_agent(&root.join("agents"))?;
        true
    };

    let host_notes = host_readiness_notes();
    Ok(HostHomePrep {
        config_path,
        root,
        created_config,
        created_dirs,
        host_notes,
    })
}

pub fn load_host_config_from_path(path: &Path) -> Result<HostConfig, IronclawError> {
    let config_path = absolute_path(path)?;
    let contents = std::fs::read_to_string(&config_path).map_err(|err| {
        IronclawError::new(format!(
            "config read failed at {}: {err}",
            config_path.display()
        ))
    })?;
    let mut config: HostConfig = toml::from_str(&contents)
        .map_err(|err| IronclawError::new(format!("config parse failed: {err}")))?;
    config.resolve_relative_paths(&config_root(&config_path)?);
    Ok(config)
}

pub fn print_prep(prep: &HostHomePrep) {
    let root = prep.root.display();
    if prep.created_config {
        eprintln!("Initialized Ironclaw home at {root}");
        eprintln!("  config  {}", prep.config_path.display());
        eprintln!("  users   {root}/users");
        eprintln!("  agents  {root}/agents");
        eprintln!("  kernels {root}/kernels  (place vmlinux.bin here)");
        eprintln!("  rootfs  {root}/rootfs   (place ubuntu-24.04.ext4 here)");
    }
    for note in &prep.host_notes {
        eprintln!("{note}");
    }
}

fn write_default_agent(agents_dir: &Path) -> Result<(), IronclawError> {
    let has_agent = std::fs::read_dir(agents_dir)
        .map_err(|err| IronclawError::new(format!("read agents dir failed: {err}")))?
        .filter_map(|entry| entry.ok())
        .any(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("toml"));
    if has_agent {
        return Ok(());
    }
    std::fs::write(
        agents_dir.join("assistant.agent.toml"),
        DEFAULT_ASSISTANT_AGENT,
    )
    .map_err(|err| IronclawError::new(format!("write default agent failed: {err}")))
}

fn host_readiness_notes() -> Vec<String> {
    let mut notes = Vec::new();
    match std::process::Command::new("firecracker")
        .arg("--version")
        .output()
    {
        Ok(output) if output.status.success() => {}
        _ => notes.push(
            "warning: `firecracker` was not found on PATH; install Firecracker before starting a VM"
                .to_string(),
        ),
    }
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
    {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {
            notes.push("warning: /dev/kvm is missing; Ironclaw needs KVM on Linux".to_string());
        }
        Err(_) => {
            notes.push("warning: this user cannot read and write /dev/kvm".to_string());
        }
    }
    notes
}

fn absolute_path(path: &Path) -> Result<PathBuf, IronclawError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let cwd = std::env::current_dir()
        .map_err(|err| IronclawError::new(format!("current directory failed: {err}")))?;
    Ok(cwd.join(path))
}

fn ensure_dir(path: &Path) -> Result<(), IronclawError> {
    std::fs::create_dir_all(path).map_err(|err| {
        IronclawError::new(format!("create directory {} failed: {err}", path.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_home() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("ironclaw-home-{}-{}", stamp, std::process::id()));
        std::fs::create_dir_all(&path).expect("temp home");
        path
    }

    #[test]
    fn prepare_writes_config_layout_and_load_resolves_paths() {
        let home = temp_home();
        let config_path = home.join("ironclawd.toml");
        let prep = prepare_host_home(&config_path).expect("prepare");
        assert!(prep.created_config);
        assert!(config_path.exists());
        assert!(home.join("users").is_dir());
        assert!(home.join("agents").join("assistant.agent.toml").is_file());

        let config = load_host_config_from_path(&config_path).expect("load");
        assert_eq!(config.storage.users_root, home.join("users"));
        assert_eq!(config.farm.manifests_dir, home.join("agents"));
        assert_eq!(
            config.firecracker.kernel_path,
            home.join("kernels").join("vmlinux.bin")
        );
        assert_eq!(
            config.daemon.pid_file.as_deref(),
            Some(home.join("run").join("ironclawd.pid").as_path())
        );

        let again = prepare_host_home(&config_path).expect("prepare existing");
        assert!(!again.created_config);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn default_toml_parses() {
        let config: HostConfig = toml::from_str(DEFAULT_IRONCLAWD_TOML).expect("default toml");
        assert!(config.firecracker.enabled);
        assert_eq!(config.storage.users_root, PathBuf::from("users"));
        assert_eq!(config.farm.manifests_dir, PathBuf::from("agents"));
    }
}
