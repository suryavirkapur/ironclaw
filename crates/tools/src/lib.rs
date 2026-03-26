use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
#[error("tool error: {message}")]
pub struct ToolError {
    message: String,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolResult {
    pub output: String,
    pub ok: bool,
}

pub trait Tool: Send + Sync {
    fn run(&self, input: &str) -> Result<ToolResult, ToolError>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
    allowed: HashSet<String>,
}

impl ToolRegistry {
    pub fn new(allowed_tools: &[String]) -> Self {
        let allowed = allowed_tools
            .iter()
            .map(|tool| tool.to_string())
            .collect::<HashSet<_>>();
        Self {
            tools: HashMap::new(),
            allowed,
        }
    }

    pub fn set_allowed_tools(&mut self, allowed_tools: &[String]) {
        self.allowed = allowed_tools
            .iter()
            .map(|tool| tool.to_string())
            .collect::<HashSet<_>>();
    }

    pub fn register(&mut self, name: &str, tool: Box<dyn Tool>) {
        self.tools.insert(name.to_string(), tool);
    }

    pub fn execute(&self, name: &str, input: &str) -> Result<ToolResult, ToolError> {
        if !self.allowed.contains(name) {
            return Err(ToolError::new(format!("tool not allowed: {name}")));
        }
        let Some(tool) = self.tools.get(name) else {
            return Err(ToolError::new(format!("unknown tool: {name}")));
        };
        tool.run(input)
    }
}

pub struct FileReadTool {
    root: PathBuf,
}

impl FileReadTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Tool for FileReadTool {
    fn run(&self, input: &str) -> Result<ToolResult, ToolError> {
        let full = rooted_path(&self.root, input)?;
        let output = std::fs::read_to_string(&full)
            .map_err(|err| ToolError::new(format!("read failed: {err}")))?;
        Ok(ToolResult { output, ok: true })
    }
}

pub struct FileWriteTool {
    root: PathBuf,
}

impl FileWriteTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Tool for FileWriteTool {
    fn run(&self, input: &str) -> Result<ToolResult, ToolError> {
        let mut parts = input.splitn(2, '\n');
        let path = parts.next().unwrap_or("").trim();
        if path.is_empty() {
            return Err(ToolError::new("missing path"));
        }
        let contents = parts.next().unwrap_or("");
        let full = rooted_path(&self.root, path)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| ToolError::new(format!("create dir failed: {err}")))?;
        }
        std::fs::write(&full, contents)
            .map_err(|err| ToolError::new(format!("write failed: {err}")))?;
        Ok(ToolResult {
            output: "ok".to_string(),
            ok: true,
        })
    }
}

pub struct RestrictedBashTool {
    enabled: bool,
    max_output_bytes: usize,
    working_dir: PathBuf,
}

impl RestrictedBashTool {
    pub fn new(enabled: bool, working_dir: PathBuf) -> Self {
        Self {
            enabled,
            max_output_bytes: 32 * 1024,
            working_dir,
        }
    }
}

impl Tool for RestrictedBashTool {
    fn run(&self, input: &str) -> Result<ToolResult, ToolError> {
        if !self.enabled {
            return Err(ToolError::new("bash tool disabled"));
        }

        // Very small guardrails. This is still dangerous; enable explicitly.
        let lower = input.to_lowercase();
        for banned in [
            "rm ", "sudo", "curl ", "wget ", "ssh ", "scp ", "nc ", "netcat",
        ] {
            if lower.contains(banned) {
                return Err(ToolError::new(format!(
                    "bash blocked by policy: contains '{banned}'"
                )));
            }
        }

        // Firecracker guest rootfs uses busybox; bash may not exist.
        // Use sh for portability.
        let out = std::process::Command::new("sh")
            .arg("-lc")
            .arg(input)
            .current_dir(&self.working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|err| ToolError::new(format!("bash exec failed: {err}")))?;

        let mut combined = Vec::new();
        combined.extend_from_slice(&out.stdout);
        combined.extend_from_slice(&out.stderr);
        if combined.len() > self.max_output_bytes {
            combined.truncate(self.max_output_bytes);
            combined.extend_from_slice(b"\n...<truncated>\n");
        }

        let output = String::from_utf8_lossy(&combined).to_string();
        Ok(ToolResult {
            output,
            ok: out.status.success(),
        })
    }
}

fn rooted_path(root: &Path, raw_path: &str) -> Result<PathBuf, ToolError> {
    let mut safe = PathBuf::new();
    let candidate = Path::new(raw_path);
    for component in candidate.components() {
        match component {
            Component::Normal(segment) => safe.push(segment),
            Component::CurDir => {}
            Component::RootDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(ToolError::new("path escapes tool root"));
            }
        }
    }
    if safe.as_os_str().is_empty() {
        return Err(ToolError::new("missing path"));
    }
    Ok(root.join(safe))
}

#[derive(Deserialize)]
pub struct CodeInput {
    pub language: String,
    pub code: String,
    pub stdin: Option<String>,
}

#[derive(Serialize)]
pub struct CodeResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

pub struct CodeExecutionTool {
    workspace_root: PathBuf,
    timeout_secs: u32,
    allowed_domains: Vec<String>,
}

impl CodeExecutionTool {
    pub fn new(workspace_root: PathBuf, timeout_secs: u32, allowed_domains: Vec<String>) -> Self {
        Self {
            workspace_root,
            timeout_secs,
            allowed_domains,
        }
    }

    fn language_to_binary(lang: &str) -> Option<&'static str> {
        match lang.to_lowercase().as_str() {
            "python" | "python3" | "py" => Some("python3"),
            "node" | "javascript" | "js" => Some("node"),
            "bash" | "shell" | "sh" => Some("sh"),
            _ => None,
        }
    }

    fn language_to_ext(lang: &str) -> Option<&'static str> {
        match lang.to_lowercase().as_str() {
            "python" | "python3" | "py" => Some("py"),
            "node" | "javascript" | "js" => Some("js"),
            "bash" | "shell" | "sh" => Some("sh"),
            _ => None,
        }
    }

    fn check_network_safety(&self, code: &str) -> Result<(), ToolError> {
        if self.allowed_domains.is_empty() {
            return Ok(());
        }

        let network_patterns = [
            "urllib.request",
            "urllib.error",
            "urllib.parse",
            "requests.",
            "http://",
            "https://",
            "fetch(",
            "axios.",
            "http.get",
            "http.post",
            "node:http",
            "node:https",
            "node:fetch",
            "require('http')",
            "require('https')",
            "websocket",
            "socket.connect",
            "socket.createConnection",
        ];

        let code_lower = code.to_lowercase();
        for pattern in network_patterns {
            if code_lower.contains(&pattern.to_lowercase()) {
                return Err(ToolError::new(format!(
                    "code uses network features but allowlist is configured. Domains allowed: {:?}. \
                     Remove network calls or configure allowed_domains.",
                    self.allowed_domains
                )));
            }
        }
        Ok(())
    }
}

impl Tool for CodeExecutionTool {
    fn run(&self, input: &str) -> Result<ToolResult, ToolError> {
        let spec: CodeInput = serde_json::from_str(input)
            .map_err(|e| ToolError::new(format!("invalid json: {}", e)))?;

        let binary = Self::language_to_binary(&spec.language)
            .ok_or_else(|| ToolError::new(format!("unsupported language: {}", spec.language)))?;

        let ext = Self::language_to_ext(&spec.language)
            .ok_or_else(|| ToolError::new("cannot determine file extension"))?;

        self.check_network_safety(&spec.code)?;

        let script_path = self.workspace_root.join(format!("exec.{}", ext));
        std::fs::write(&script_path, &spec.code)
            .map_err(|e| ToolError::new(format!("write failed: {}", e)))?;

        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| ToolError::new(format!("runtime error: {}", e)))?;

        let timeout_secs = self.timeout_secs;
        let script_path_for_exec = script_path.clone();
        let result = rt.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs as u64),
                tokio::task::spawn_blocking(move || {
                    std::process::Command::new(binary)
                        .arg(script_path_for_exec)
                        .output()
                })
            ).await
        });

        let _ = std::fs::remove_file(&script_path);

        match result {
            Ok(Ok(Ok(output))) => {
                let code_result = CodeResult {
                    exit_code: output.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                    timed_out: false,
                };
                Ok(ToolResult {
                    ok: output.status.success(),
                    output: serde_json::to_string(&code_result).unwrap_or_else(|_| "{}".to_string()),
                })
            }
            Ok(Ok(Err(e))) => Err(ToolError::new(format!("execution failed: {}", e))),
            Ok(Err(e)) => Err(ToolError::new(format!("task failed: {}", e))),
            Err(_) => {
                let code_result = CodeResult {
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("execution timed out after {}s", timeout_secs),
                    timed_out: true,
                };
                Ok(ToolResult {
                    ok: false,
                    output: serde_json::to_string(&code_result).unwrap_or_else(|_| "{}".to_string()),
                })
            }
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct ToolMeta {
    pub name: String,
    pub language: String,
    pub description: String,
    pub created_at: String,
}

#[derive(Deserialize)]
pub struct ToolInstallInput {
    pub name: String,
    pub language: String,
    pub code: String,
    pub description: Option<String>,
}

pub struct ToolInstallTool {
    tools_dir: PathBuf,
    workspace_root: PathBuf,
    timeout_secs: u32,
    allowed_domains: Vec<String>,
}

impl ToolInstallTool {
    pub fn new(
        tools_dir: PathBuf,
        workspace_root: PathBuf,
        timeout_secs: u32,
        allowed_domains: Vec<String>,
    ) -> Self {
        Self {
            tools_dir,
            workspace_root,
            timeout_secs,
            allowed_domains,
        }
    }
}

impl Tool for ToolInstallTool {
    fn run(&self, input: &str) -> Result<ToolResult, ToolError> {
        let spec: ToolInstallInput = serde_json::from_str(input)
            .map_err(|e| ToolError::new(format!("invalid json: {}", e)))?;

        if !spec.name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(ToolError::new("name must be alphanumeric + underscore"));
        }

        if spec.name.is_empty() || spec.name.len() > 64 {
            return Err(ToolError::new("name must be 1-64 characters"));
        }

        let ext = CodeExecutionTool::language_to_ext(&spec.language)
            .ok_or_else(|| ToolError::new("unsupported language"))?;

        std::fs::create_dir_all(&self.tools_dir)
            .map_err(|e| ToolError::new(format!("create dir failed: {}", e)))?;

        let script_path = self.tools_dir.join(format!("{}.{}", spec.name, ext));
        let meta_path = self.tools_dir.join(format!("{}.meta.json", spec.name));

        std::fs::write(&script_path, &spec.code)
            .map_err(|e| ToolError::new(format!("write script failed: {}", e)))?;

        let now = format!("{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0));
        let meta = ToolMeta {
            name: spec.name.clone(),
            language: spec.language.clone(),
            description: spec.description.unwrap_or_default(),
            created_at: now,
        };

        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|e| ToolError::new(format!("serialize meta failed: {}", e)))?;

        std::fs::write(&meta_path, meta_json)
            .map_err(|e| ToolError::new(format!("write meta failed: {}", e)))?;

        Ok(ToolResult {
            ok: true,
            output: serde_json::json!({
                "installed": spec.name,
                "path": script_path.display().to_string(),
                "language": spec.language
            }).to_string(),
        })
    }
}

pub struct ToolCallTool {
    tools_dir: PathBuf,
    workspace_root: PathBuf,
    timeout_secs: u32,
    allowed_domains: Vec<String>,
}

impl ToolCallTool {
    pub fn new(
        tools_dir: PathBuf,
        workspace_root: PathBuf,
        timeout_secs: u32,
        allowed_domains: Vec<String>,
    ) -> Self {
        Self {
            tools_dir,
            workspace_root,
            timeout_secs,
            allowed_domains,
        }
    }
}

impl Tool for ToolCallTool {
    fn run(&self, input: &str) -> Result<ToolResult, ToolError> {
        let parts: Vec<&str> = input.splitn(2, ' ').collect();
        let tool_name = parts.first().ok_or_else(|| ToolError::new("missing tool name"))?;
        let args = parts.get(1).unwrap_or(&"");

        let meta_path = self.tools_dir.join(format!("{}.meta.json", tool_name));
        let meta: ToolMeta = serde_json::from_str(
            &std::fs::read_to_string(&meta_path)
                .map_err(|e| ToolError::new(format!("tool not found: {}", e)))?
        ).map_err(|e| ToolError::new(format!("invalid meta: {}", e)))?;

        let ext = CodeExecutionTool::language_to_ext(&meta.language).unwrap();
        let script_path = self.tools_dir.join(format!("{}.{}", tool_name, ext));

        let code = std::fs::read_to_string(&script_path)
            .map_err(|e| ToolError::new(format!("read tool failed: {}", e)))?;

        let exec_tool = CodeExecutionTool::new(
            self.workspace_root.clone(),
            self.timeout_secs,
            self.allowed_domains.clone(),
        );

        let input_json = serde_json::json!({
            "language": meta.language,
            "code": code,
            "stdin": args.to_string(),
        });

        exec_tool.run(&input_json.to_string())
    }
}

impl ToolRegistry {
    pub fn load_installed_tools(&mut self, tools_dir: &Path) {
        let Ok(entries) = std::fs::read_dir(tools_dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("meta.json") {
                if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                    let meta_path = tools_dir.join(format!("{}.meta.json", name));
                    if let Ok(meta_json) = std::fs::read_to_string(&meta_path) {
                        if let Ok(meta) = serde_json::from_str::<ToolMeta>(&meta_json) {
                            let tools_dir = tools_dir.to_path_buf();
                            let workspace_root = tools_dir.join("../workspace");
                            self.register(
                                name,
                                Box::new(ToolCallTool::new(
                                    tools_dir,
                                    workspace_root,
                                    30,
                                    vec![],
                                )),
                            );
                            tracing::info!("loaded installed tool: {} ({})", name, meta.language);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod lib_test;
