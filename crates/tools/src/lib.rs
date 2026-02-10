use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
#[error("tool error: {message}")]
pub struct ToolError {
    message: String,
}

impl ToolError {
    fn new(message: impl Into<String>) -> Self {
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

pub trait BashTool: Send + Sync {
    fn run(&self, command: &str) -> Result<ToolResult, ToolError>;
}

pub trait FileTool: Send + Sync {
    fn read(&self, path: &Path) -> Result<String, ToolError>;
    fn write(&self, path: &Path, contents: &str) -> Result<(), ToolError>;
    fn list(&self, path: &Path) -> Result<Vec<PathBuf>, ToolError>;
}

pub struct StubBashTool;

impl BashTool for StubBashTool {
    fn run(&self, _command: &str) -> Result<ToolResult, ToolError> {
        Err(ToolError::new("bash tool disabled"))
    }
}

pub struct StubFileTool {
    root: PathBuf,
}

impl StubFileTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn ensure_rooted(&self, path: &Path) -> Result<PathBuf, ToolError> {
        let full = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        if !full.starts_with(&self.root) {
            return Err(ToolError::new("path escapes tool root"));
        }
        Ok(full)
    }
}

impl FileTool for StubFileTool {
    fn read(&self, path: &Path) -> Result<String, ToolError> {
        let full = self.ensure_rooted(path)?;
        std::fs::read_to_string(&full)
            .map_err(|err| ToolError::new(format!("read failed: {err}")))
    }

    fn write(&self, path: &Path, contents: &str) -> Result<(), ToolError> {
        let full = self.ensure_rooted(path)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| ToolError::new(format!("create dir failed: {err}")))?;
        }
        std::fs::write(&full, contents)
            .map_err(|err| ToolError::new(format!("write failed: {err}")))
    }

    fn list(&self, path: &Path) -> Result<Vec<PathBuf>, ToolError> {
        let full = self.ensure_rooted(path)?;
        let mut entries = Vec::new();
        let read_dir = std::fs::read_dir(&full)
            .map_err(|err| ToolError::new(format!("list failed: {err}")))?;
        for entry in read_dir {
            let entry = entry.map_err(|err| ToolError::new(format!("list failed: {err}")))?;
            entries.push(entry.path());
        }
        Ok(entries)
    }
}
