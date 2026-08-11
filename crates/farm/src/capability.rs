use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    WasmTool,
    McpTool,
    McpResource,
    A2aSkill,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityEffect {
    #[default]
    Read,
    Write,
    External,
    Irreversible,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Capability {
    pub uri: CapabilityUri,
    pub name: String,
    pub description: String,
    pub kind: CapabilityKind,
    pub effect: CapabilityEffect,
    #[serde(default)]
    pub input_schema: Value,
    #[serde(default)]
    pub output_schema: Value,
    #[serde(default)]
    pub required_scopes: Vec<String>,
    #[serde(default)]
    pub data_classes: Vec<String>,
    #[serde(default)]
    pub requires_approval: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct CapabilityUri(String);

impl CapabilityUri {
    pub fn new(scheme: &str, authority: &str, name: &str) -> Result<Self, CapabilityUriError> {
        Self::from_str(&format!("{scheme}://{authority}/{name}"))
    }

    pub fn scheme(&self) -> &str {
        self.0.split_once("://").map(|part| part.0).unwrap_or("")
    }

    pub fn authority(&self) -> &str {
        self.0
            .split_once("://")
            .and_then(|part| part.1.split_once('/'))
            .map(|part| part.0)
            .unwrap_or("")
    }

    pub fn name(&self) -> &str {
        self.0
            .split_once("://")
            .and_then(|part| part.1.split_once('/'))
            .map(|part| part.1)
            .unwrap_or("")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CapabilityUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CapabilityUri {
    type Err = CapabilityUriError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some((scheme, remainder)) = value.split_once("://") else {
            return Err(CapabilityUriError::Malformed(value.to_string()));
        };
        let Some((authority, name)) = remainder.split_once('/') else {
            return Err(CapabilityUriError::Malformed(value.to_string()));
        };
        if !matches!(scheme, "local" | "mcp" | "agent")
            || !valid_component(authority)
            || name.is_empty()
            || name.starts_with('/')
            || name.split('/').any(|part| !valid_component(part))
        {
            return Err(CapabilityUriError::Malformed(value.to_string()));
        }
        Ok(Self(value.to_string()))
    }
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CapabilityUriError {
    #[error("malformed capability URI: {0}")]
    Malformed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_capability_uris() {
        let uri: CapabilityUri = "mcp://bigquery/query.execute".parse().unwrap();
        assert_eq!(uri.scheme(), "mcp");
        assert_eq!(uri.authority(), "bigquery");
        assert_eq!(uri.name(), "query.execute");
        assert!("http://server/tool".parse::<CapabilityUri>().is_err());
        assert!("local://agent/../secret".parse::<CapabilityUri>().is_err());
    }
}
