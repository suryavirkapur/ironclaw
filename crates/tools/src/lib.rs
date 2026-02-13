use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

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
}

impl RestrictedBashTool {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

impl Tool for RestrictedBashTool {
    fn run(&self, _input: &str) -> Result<ToolResult, ToolError> {
        if !self.enabled {
            return Err(ToolError::new("bash tool disabled"));
        }
        Err(ToolError::new("bash tool is not implemented"))
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

#[cfg(test)]
mod lib_test;
