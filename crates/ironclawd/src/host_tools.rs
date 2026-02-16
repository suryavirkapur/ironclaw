use std::path::{Path, PathBuf};

pub const MAX_TOOL_OUTPUT_CHARS: usize = 8_000;

pub async fn run_host_tool(
    allowed_tools: &[String],
    user_id: &str,
    tool: &str,
    input: &str,
) -> Result<String, String> {
    if !allowed_tools.iter().any(|t| t == tool) {
        return Err(format!("tool not allowed: {tool}"));
    }

    let ws_root = host_workspace_root(user_id)?;

    match tool {
        "bash" => run_bash(&ws_root, input).await,
        "file_read" => file_read(&ws_root, input).await,
        "file_write" => file_write(&ws_root, input).await,
        _ => Err(format!("unknown tool: {tool}")),
    }
}

pub fn truncate_tool_output(output: &str) -> String {
    if output.chars().count() <= MAX_TOOL_OUTPUT_CHARS {
        return output.to_string();
    }

    let mut truncated = String::new();
    for ch in output.chars().take(MAX_TOOL_OUTPUT_CHARS) {
        truncated.push(ch);
    }
    truncated.push_str("\n[output truncated]");
    truncated
}

fn host_workspace_root(user_id: &str) -> Result<PathBuf, String> {
    // Keep host tool I/O away from secrets. This is a per-user sandbox directory on the host.
    let root = PathBuf::from("data")
        .join("users")
        .join(user_id)
        .join("host-workspace");
    std::fs::create_dir_all(&root).map_err(|e| format!("create host workspace failed: {e}"))?;
    Ok(root)
}

async fn run_bash(cwd: &Path, cmd: &str) -> Result<String, String> {
    use tokio::process::Command;

    if let Some(pattern) = blocked_bash_pattern(cmd) {
        return Err(format!("bash blocked by policy: contains '{pattern}'"));
    }

    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Command::new("bash")
            .arg("-lc")
            .arg(cmd)
            .current_dir(cwd)
            .output(),
    )
    .await
    {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("bash failed: {e}")),
        Err(_) => return Err("bash timed out".to_string()),
    };
    let mut out = String::new();
    out.push_str(&String::from_utf8_lossy(&output.stdout));
    out.push_str(&String::from_utf8_lossy(&output.stderr));

    if output.status.success() {
        Ok(out)
    } else {
        Err(out)
    }
}

fn blocked_bash_pattern(cmd: &str) -> Option<&'static str> {
    let lower = cmd.to_lowercase();
    [
        "rm ", "sudo", "curl ", "wget ", "ssh ", "scp ", "nc ", "netcat",
    ]
    .into_iter()
    .find(|pattern| lower.contains(pattern))
}

async fn file_read(root: &Path, path: &str) -> Result<String, String> {
    let full = safe_join(root, path)?;
    tokio::fs::read_to_string(&full)
        .await
        .map_err(|e| format!("read failed: {e}"))
}

async fn file_write(root: &Path, input: &str) -> Result<String, String> {
    let mut parts = input.splitn(2, '\n');
    let path = parts.next().unwrap_or("").trim();
    let contents = parts.next().unwrap_or("");
    if path.is_empty() {
        return Err("missing path".to_string());
    }
    let full = safe_join(root, path)?;
    if let Some(parent) = full.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("mkdir failed: {e}"))?;
    }
    tokio::fs::write(&full, contents)
        .await
        .map_err(|e| format!("write failed: {e}"))?;
    Ok("ok".to_string())
}

fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim_start_matches('/');
    let candidate = root.join(rel);
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("canonicalize root failed: {e}"))?;

    // parent might not exist yet, so canonicalize a best-effort path.
    let canon_candidate = candidate
        .parent()
        .unwrap_or(&candidate)
        .canonicalize()
        .unwrap_or_else(|_| canon_root.clone())
        .join(candidate.file_name().unwrap_or_default());

    if !canon_candidate.starts_with(&canon_root) {
        return Err("path escapes workspace".to_string());
    }
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::{blocked_bash_pattern, file_read, file_write};

    fn temp_root(name: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("ironclaw-host-tools-{name}-{stamp}"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::create_dir_all(&root);
        root
    }

    #[test]
    fn bash_policy_blocks_sudo_and_rm() {
        assert_eq!(blocked_bash_pattern("sudo ls"), Some("sudo"));
        assert_eq!(blocked_bash_pattern("rm -rf /tmp/demo"), Some("rm "));
        assert_eq!(blocked_bash_pattern("echo safe"), None);
    }

    #[tokio::test]
    async fn file_tools_reject_path_escape() {
        let root = temp_root("escape");
        let read_result = file_read(&root, "../etc/passwd").await;
        assert!(read_result.is_err());

        let write_result = file_write(&root, "../outside.txt\nblocked").await;
        assert!(write_result.is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn file_tools_write_and_read_inside_root() {
        let root = temp_root("roundtrip");
        let write = file_write(&root, "notes/a.txt\nhello").await;
        assert!(write.is_ok());

        let read = file_read(&root, "notes/a.txt").await;
        assert_eq!(read.ok().as_deref(), Some("hello"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
