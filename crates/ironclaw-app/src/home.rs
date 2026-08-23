use common::config::HostConfig;
use std::fs;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "ironclawd.toml";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderItem {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedHome {
    pub folder: PathBuf,
    pub config_file: Option<PathBuf>,
    pub daemon_url: String,
}

pub fn default_config_folder() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ironclaw")
}

pub fn last_home_store() -> PathBuf {
    if let Ok(dir) = std::env::var("IRONCLAW_APP_STATE_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("last-home");
        }
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ironclaw-app")
        .join("last-home")
}

pub fn load_last_home() -> Option<PathBuf> {
    let contents = fs::read_to_string(last_home_store()).ok()?;
    let path = PathBuf::from(contents.trim());
    path.is_dir().then_some(path)
}

pub fn save_last_home(path: &Path) -> Result<(), String> {
    let store = last_home_store();
    if let Some(parent) = store.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    fs::write(store, path.to_string_lossy().as_bytes()).map_err(|err| err.to_string())
}

pub fn starting_folder() -> PathBuf {
    load_last_home()
        .filter(|path| path.is_dir())
        .unwrap_or_else(default_config_folder)
}

pub fn list_folder(path: &Path) -> Result<Vec<FolderItem>, String> {
    if !path.is_dir() {
        return Err(format!("{} is not a folder", path.display()));
    }
    let mut items = Vec::new();
    let entries = fs::read_dir(path).map_err(|err| err.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "." || name == ".." {
            continue;
        }
        let path = entry.path();
        let is_dir = path.is_dir();
        items.push(FolderItem { name, path, is_dir });
    }
    items.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(items)
}

pub fn config_file_in(dir: &Path) -> Option<PathBuf> {
    let path = dir.join(CONFIG_FILE_NAME);
    path.is_file().then_some(path)
}

pub fn daemon_url_from_config(config: &HostConfig) -> String {
    let host = loopback_host(&config.server.bind);
    format!("http://{host}:{}", config.server.port)
}

pub fn open_home(folder: &Path) -> Result<OpenedHome, String> {
    let folder = if folder.is_absolute() {
        folder.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| err.to_string())?
            .join(folder)
    };
    if !folder.is_dir() {
        return Err(format!("{} is not a folder", folder.display()));
    }
    let config_file = config_file_in(&folder);
    let daemon_url = if let Some(config_file) = &config_file {
        let contents = fs::read_to_string(config_file).map_err(|err| err.to_string())?;
        let config: HostConfig = toml::from_str(&contents).map_err(|err| err.to_string())?;
        daemon_url_from_config(&config)
    } else {
        std::env::var("IRONCLAW_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:9938".to_string())
    };
    save_last_home(&folder)?;
    Ok(OpenedHome {
        folder,
        config_file,
        daemon_url,
    })
}

fn loopback_host(bind: &str) -> String {
    match bind {
        "" | "0.0.0.0" | "::" | "[::]" => "127.0.0.1".into(),
        host if host.starts_with('[') => host.to_string(),
        host if host.contains(':') => format!("[{host}]"),
        host => host.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ironclaw-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn lists_dirs_before_files() {
        let root = temp_dir();
        fs::create_dir(root.join("agents")).unwrap();
        fs::write(root.join("ironclawd.toml"), "x = 1").unwrap();
        fs::write(root.join("notes.txt"), "hi").unwrap();
        let names: Vec<_> = list_folder(&root)
            .unwrap()
            .into_iter()
            .map(|item| (item.name, item.is_dir))
            .collect();
        assert_eq!(
            names,
            vec![
                ("agents".into(), true),
                ("ironclawd.toml".into(), false),
                ("notes.txt".into(), false),
            ]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn opens_home_and_reads_daemon_bind() {
        let root = temp_dir();
        let state = temp_dir();
        std::env::set_var("IRONCLAW_APP_STATE_DIR", &state);
        fs::write(
            root.join("ironclawd.toml"),
            r#"
[server]
bind = "0.0.0.0"
port = 9941
[storage]
users_root = "users"
[llm]
model = "test"
base_url = "http://127.0.0.1:1"
api = "responses"
[firecracker]
enabled = false
kernel_path = "kernels/vmlinux.bin"
rootfs_path = "rootfs/guest.ext4"
api_socket_dir = "run/fc"
"#,
        )
        .unwrap();
        let opened = open_home(&root).unwrap();
        assert_eq!(opened.daemon_url, "http://127.0.0.1:9941");
        assert_eq!(
            opened.config_file.as_deref(),
            Some(root.join("ironclawd.toml").as_path())
        );
        assert_eq!(load_last_home().as_deref(), Some(root.as_path()));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(state);
    }

    #[test]
    fn open_rejects_files() {
        let root = temp_dir();
        let file = root.join("ironclawd.toml");
        fs::write(&file, "[server]\n").unwrap();
        let err = open_home(&file).unwrap_err();
        assert!(err.contains("not a folder"), "{err}");
        let _ = fs::remove_dir_all(root);
    }
}
