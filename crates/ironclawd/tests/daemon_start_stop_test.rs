use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn unique_temp_dir(name: &str) -> io::Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("{name}-{stamp}-{}", std::process::id()));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn write_host_config(path: &Path, pid_file: &Path, log_file: &Path) -> io::Result<()> {
    let contents = format!(
        concat!(
            "execution_mode = \"host_only\"\n",
            "idle_timeout_minutes = 1\n",
            "log_level = \"info\"\n",
            "[server]\n",
            "bind = \"127.0.0.1\"\n",
            "port = 10938\n",
            "[ui]\n",
            "mount = \"/ui\"\n",
            "index_file = \"index.html\"\n",
            "[storage]\n",
            "users_root = \"data/users\"\n",
            "[llm]\n",
            "model = \"test\"\n",
            "base_url = \"http://127.0.0.1:1\"\n",
            "api = \"responses\"\n",
            "[firecracker]\n",
            "enabled = false\n",
            "kernel_path = \"kernels/firecracker/vmlinux.bin\"\n",
            "rootfs_path = \"rootfs/guest.ext4\"\n",
            "api_socket_dir = \"/tmp/ironclaw-fc\"\n",
            "[daemon]\n",
            "pid_file = \"{}\"\n",
            "log_file = \"{}\"\n",
            "graceful_timeout_ms = 4000\n",
            "log_rotate_keep = 5\n",
            "log_rotate_max_bytes = 1048576\n",
        ),
        pid_file.display(),
        log_file.display()
    );
    std::fs::write(path, contents)
}

fn wait_for_file(path: &Path, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn daemon_start_and_stop_with_pid_file() {
    let temp_root = match unique_temp_dir("ironclawd-daemon-test") {
        Ok(path) => path,
        Err(err) => panic!("temp dir create failed: {err}"),
    };
    let config_path = temp_root.join("ironclawd.toml");
    let pid_file = temp_root.join("ironclawd.pid");
    let log_file = temp_root.join("ironclawd.log");

    if let Err(err) = write_host_config(&config_path, &pid_file, &log_file) {
        panic!("config write failed: {err}");
    }

    let binary = env!("CARGO_BIN_EXE_ironclawd");

    let start_status = Command::new(binary)
        .arg("--daemon")
        .arg("--pid-file")
        .arg(&pid_file)
        .env("IRONCLAWD_CONFIG", &config_path)
        .env("IRONCLAWD_TEST_NO_BIND", "1")
        .status();
    let start_status = match start_status {
        Ok(status) => status,
        Err(err) => panic!("start command failed: {err}"),
    };
    assert!(start_status.success());
    assert!(wait_for_file(&pid_file, Duration::from_secs(5)));

    let stop_status = Command::new(binary)
        .arg("--stop")
        .arg("--pid-file")
        .arg(&pid_file)
        .env("IRONCLAWD_CONFIG", &config_path)
        .status();
    let stop_status = match stop_status {
        Ok(status) => status,
        Err(err) => panic!("stop command failed: {err}"),
    };
    assert!(stop_status.success());

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if !pid_file.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(!pid_file.exists());

    let _ = std::fs::remove_dir_all(temp_root);
}
