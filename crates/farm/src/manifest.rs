use crate::capability::CapabilityEffect;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AgentManifest {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub role: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub reports_to: Option<String>,
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub compute: ComputeConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub wasm: WasmPolicy,
    #[serde(default)]
    pub wasm_tools: Vec<WasmTool>,
    #[serde(default)]
    pub mcp: Vec<McpServerAccess>,
    #[serde(default)]
    pub a2a: A2aPolicy,
    #[serde(default)]
    pub skills: Vec<AgentSkill>,
}

fn schema_version() -> u32 {
    1
}

fn enabled() -> bool {
    true
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelConfig {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ComputeConfig {
    #[serde(default = "default_vcpus")]
    pub vcpus: u8,
    #[serde(default = "default_memory_mib")]
    pub memory_mib: u32,
    #[serde(default = "default_disk_quota_mb")]
    pub disk_quota_mb: u32,
}

fn default_vcpus() -> u8 {
    2
}
fn default_memory_mib() -> u32 {
    2048
}
fn default_disk_quota_mb() -> u32 {
    2048
}

impl Default for ComputeConfig {
    fn default() -> Self {
        Self {
            vcpus: default_vcpus(),
            memory_mib: default_memory_mib(),
            disk_quota_mb: default_disk_quota_mb(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MemoryConfig {
    #[serde(default = "default_memory_engine")]
    pub engine: String,
    #[serde(default = "private_memory")]
    pub private: bool,
    #[serde(default)]
    pub collections: Vec<String>,
}

fn private_memory() -> bool {
    true
}

fn default_memory_engine() -> String {
    "core-agent-memory".to_string()
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            engine: default_memory_engine(),
            private: true,
            collections: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WasmPolicy {
    #[serde(default = "default_tools_dir")]
    pub tools_dir: PathBuf,
    #[serde(default)]
    pub may_create: bool,
    #[serde(default)]
    pub may_publish: bool,
}

fn default_tools_dir() -> PathBuf {
    PathBuf::from("tools")
}

impl Default for WasmPolicy {
    fn default() -> Self {
        Self {
            tools_dir: default_tools_dir(),
            may_create: false,
            may_publish: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct WasmTool {
    pub id: String,
    pub module: PathBuf,
    pub description: String,
    #[serde(default = "object_schema")]
    pub input_schema: Value,
    #[serde(default = "object_schema")]
    pub output_schema: Value,
    #[serde(default)]
    pub effect: CapabilityEffect,
    #[serde(default)]
    pub data_classes: Vec<String>,
    #[serde(default)]
    pub requires_approval: bool,
    #[serde(default)]
    pub permissions: WasmPermissions,
    #[serde(default)]
    pub limits: WasmLimits,
}

fn object_schema() -> Value {
    json!({"type": "object"})
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct WasmPermissions {
    #[serde(default)]
    pub mcp_tools: Vec<String>,
    #[serde(default)]
    pub delegate_to: Vec<String>,
    #[serde(default)]
    pub filesystem_read: Vec<PathBuf>,
    #[serde(default)]
    pub filesystem_write: Vec<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WasmLimits {
    #[serde(default = "default_wasm_memory_mib")]
    pub memory_mib: u32,
    #[serde(default = "default_wasm_fuel")]
    pub fuel: u64,
    #[serde(default = "default_wasm_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_wasm_memory_mib() -> u32 {
    64
}
fn default_wasm_fuel() -> u64 {
    10_000_000
}
fn default_wasm_timeout_ms() -> u64 {
    10_000
}

impl Default for WasmLimits {
    fn default() -> Self {
        Self {
            memory_mib: default_wasm_memory_mib(),
            fuel: default_wasm_fuel(),
            timeout_ms: default_wasm_timeout_ms(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct McpServerAccess {
    pub id: String,
    pub server: String,
    #[serde(default = "default_mcp_protocol_version")]
    pub protocol_version: String,
    /// Logical credential name resolved by the host credential broker. Never
    /// put a token in an agent manifest.
    #[serde(default)]
    pub credential: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub data_classes: Vec<String>,
}

fn default_mcp_protocol_version() -> String {
    "2025-06-18".to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct A2aPolicy {
    #[serde(default)]
    pub accept_from: Vec<String>,
    #[serde(default)]
    pub delegate_to: Vec<String>,
    #[serde(default = "default_delegation_depth")]
    pub max_delegation_depth: u8,
    #[serde(default = "default_concurrent_tasks")]
    pub max_concurrent_tasks: u16,
}

fn default_delegation_depth() -> u8 {
    4
}
fn default_concurrent_tasks() -> u16 {
    4
}

impl Default for A2aPolicy {
    fn default() -> Self {
        Self {
            accept_from: Vec::new(),
            delegate_to: Vec::new(),
            max_delegation_depth: default_delegation_depth(),
            max_concurrent_tasks: default_concurrent_tasks(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AgentSkill {
    pub id: String,
    pub description: String,
    #[serde(default = "object_schema")]
    pub input_schema: Value,
    #[serde(default = "object_schema")]
    pub output_schema: Value,
    #[serde(default)]
    pub data_classes: Vec<String>,
    #[serde(default)]
    pub requires_approval: bool,
}

impl AgentManifest {
    pub fn from_toml(input: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(input)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != 1 {
            return Err(ManifestError::Validation(format!(
                "agent {} uses unsupported schema_version {}",
                self.id, self.schema_version
            )));
        }
        validate_id("agent id", &self.id)?;
        if self.name.trim().is_empty() || self.role.trim().is_empty() {
            return Err(ManifestError::Validation(format!(
                "agent {} requires non-empty name and role",
                self.id
            )));
        }
        if let Some(manager) = &self.reports_to {
            validate_id("reports_to", manager)?;
            if manager == &self.id {
                return Err(ManifestError::Validation(format!(
                    "agent {} cannot report to itself",
                    self.id
                )));
            }
        }
        if self.compute.vcpus == 0 || self.compute.memory_mib < 128 {
            return Err(ManifestError::Validation(format!(
                "agent {} has invalid compute limits",
                self.id
            )));
        }
        if self.memory.engine != "core-agent-memory" {
            return Err(ManifestError::Validation(format!(
                "agent {} uses unsupported memory engine {}",
                self.id, self.memory.engine
            )));
        }
        validate_relative_path("wasm.tools_dir", &self.wasm.tools_dir)?;

        let mut wasm_ids = HashSet::new();
        for tool in &self.wasm_tools {
            validate_id("wasm tool id", &tool.id)?;
            if !wasm_ids.insert(tool.id.as_str()) {
                return Err(ManifestError::Validation(format!(
                    "agent {} declares duplicate Wasm tool {}",
                    self.id, tool.id
                )));
            }
            validate_relative_path("wasm module", &tool.module)?;
            if tool.module.extension().and_then(|value| value.to_str()) != Some("wasm") {
                return Err(ManifestError::Validation(format!(
                    "Wasm tool {} module must end in .wasm",
                    tool.id
                )));
            }
            if tool.limits.memory_mib == 0 || tool.limits.fuel == 0 || tool.limits.timeout_ms == 0 {
                return Err(ManifestError::Validation(format!(
                    "Wasm tool {} limits must be non-zero",
                    tool.id
                )));
            }
        }

        let mut mcp_ids = HashSet::new();
        for server in &self.mcp {
            validate_id("MCP server id", &server.id)?;
            if !mcp_ids.insert(server.id.as_str()) {
                return Err(ManifestError::Validation(format!(
                    "agent {} declares duplicate MCP server {}",
                    self.id, server.id
                )));
            }
            if !(server.server.starts_with("https://")
                || server.server.starts_with("http://127.0.0.1")
                || server.server.starts_with("http://localhost"))
            {
                return Err(ManifestError::Validation(format!(
                    "MCP server {} must use HTTPS (HTTP is allowed only for loopback)",
                    server.id
                )));
            }
            validate_unique_names("MCP tool", &server.tools)?;
            validate_unique_names("MCP resource", &server.resources)?;
        }

        let mut skill_ids = HashSet::new();
        for skill in &self.skills {
            validate_id("A2A skill id", &skill.id)?;
            if !skill_ids.insert(skill.id.as_str()) {
                return Err(ManifestError::Validation(format!(
                    "agent {} declares duplicate A2A skill {}",
                    self.id, skill.id
                )));
            }
        }
        validate_unique_names("A2A delegate", &self.a2a.delegate_to)?;
        validate_unique_names("A2A caller", &self.a2a.accept_from)?;
        if self.a2a.max_concurrent_tasks == 0 {
            return Err(ManifestError::Validation(format!(
                "agent {} max_concurrent_tasks must be non-zero",
                self.id
            )));
        }
        if self.a2a.delegate_to.iter().any(|target| target == &self.id) {
            return Err(ManifestError::Validation(format!(
                "agent {} cannot delegate to itself",
                self.id
            )));
        }
        Ok(())
    }
}

fn validate_id(label: &str, value: &str) -> Result<(), ManifestError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ManifestError::Validation(format!(
            "{label} must be 1-64 ASCII letters, digits, '-' or '_'"
        )));
    }
    Ok(())
}

fn validate_relative_path(label: &str, path: &Path) -> Result<(), ManifestError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ManifestError::Validation(format!(
            "{label} must be a non-empty relative path without '..'"
        )));
    }
    Ok(())
}

fn validate_unique_names(label: &str, values: &[String]) -> Result<(), ManifestError> {
    let mut seen = HashSet::new();
    for value in values {
        if value.is_empty() || !seen.insert(value) {
            return Err(ManifestError::Validation(format!(
                "{label} entries must be non-empty and unique"
            )));
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest TOML is invalid: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("manifest validation failed: {0}")]
    Validation(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
id = "analyst"
name = "Ada"
role = "Data analyst"

[wasm]
may_create = true

[[wasm_tools]]
id = "cluster_logs"
module = "cluster_logs.wasm"
description = "Cluster logs"

[[mcp]]
id = "bigquery"
server = "https://mcp.example.com/bigquery"
tools = ["query"]

[[skills]]
id = "analyze_data"
description = "Analyze a dataset"
"#;

    #[test]
    fn parses_and_defaults_manifest() {
        let manifest = AgentManifest::from_toml(VALID).unwrap();
        assert_eq!(manifest.id, "analyst");
        assert_eq!(manifest.compute.memory_mib, 2048);
        assert_eq!(manifest.wasm_tools[0].limits.memory_mib, 64);
    }

    #[test]
    fn rejects_non_wasm_and_path_escape() {
        let invalid = VALID.replace("cluster_logs.wasm", "../cluster_logs.lua");
        assert!(AgentManifest::from_toml(&invalid).is_err());
    }
}
