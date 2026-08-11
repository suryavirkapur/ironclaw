use crate::capability::{Capability, CapabilityEffect, CapabilityKind, CapabilityUri};
use crate::manifest::{AgentManifest, ManifestError};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize)]
pub struct AgentRecord {
    pub manifest: AgentManifest,
    pub manifest_path: PathBuf,
    pub revision: String,
}

#[derive(Clone, Debug, Default)]
pub struct FarmRegistry {
    agents: BTreeMap<String, AgentRecord>,
}

impl FarmRegistry {
    pub fn load_dir(path: &Path) -> Result<Self, RegistryError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        if !path.is_dir() {
            return Err(RegistryError::Io {
                path: path.to_path_buf(),
                message: "agent manifest path is not a directory".to_string(),
            });
        }

        let mut paths = std::fs::read_dir(path)
            .map_err(|err| RegistryError::io(path, err))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| RegistryError::io(path, err))?;
        paths.sort();

        let mut agents = BTreeMap::new();
        for manifest_path in paths.into_iter().filter(|candidate| {
            candidate.extension().and_then(|value| value.to_str()) == Some("toml")
        }) {
            let input = std::fs::read_to_string(&manifest_path)
                .map_err(|err| RegistryError::io(&manifest_path, err))?;
            let manifest =
                AgentManifest::from_toml(&input).map_err(|source| RegistryError::Manifest {
                    path: manifest_path.clone(),
                    source,
                })?;
            if agents.contains_key(&manifest.id) {
                return Err(RegistryError::Validation(format!(
                    "duplicate agent id {}",
                    manifest.id
                )));
            }
            let revision = revision(&manifest)?;
            agents.insert(
                manifest.id.clone(),
                AgentRecord {
                    manifest,
                    manifest_path,
                    revision,
                },
            );
        }

        let registry = Self { agents };
        registry.validate_graph()?;
        Ok(registry)
    }

    pub fn from_manifests(manifests: Vec<AgentManifest>) -> Result<Self, RegistryError> {
        let mut agents = BTreeMap::new();
        for manifest in manifests {
            manifest.validate().map_err(|err| RegistryError::Manifest {
                path: PathBuf::from("<memory>"),
                source: err,
            })?;
            let id = manifest.id.clone();
            if agents.contains_key(&id) {
                return Err(RegistryError::Validation(format!(
                    "duplicate agent id {id}"
                )));
            }
            agents.insert(
                id,
                AgentRecord {
                    revision: revision(&manifest)?,
                    manifest,
                    manifest_path: PathBuf::from("<memory>"),
                },
            );
        }
        let registry = Self { agents };
        registry.validate_graph()?;
        Ok(registry)
    }

    pub fn agents(&self) -> impl Iterator<Item = &AgentRecord> {
        self.agents.values()
    }

    pub fn get(&self, id: &str) -> Option<&AgentRecord> {
        self.agents.get(id)
    }

    pub fn capabilities_for(&self, subject: &str) -> Result<Vec<Capability>, RegistryError> {
        let agent = self
            .get(subject)
            .ok_or_else(|| RegistryError::UnknownAgent(subject.to_string()))?;
        let mut capabilities = Vec::new();

        for tool in &agent.manifest.wasm_tools {
            capabilities.push(Capability {
                uri: CapabilityUri::new("local", subject, &tool.id)
                    .expect("validated agent and tool ids form a valid URI"),
                name: tool.id.clone(),
                description: tool.description.clone(),
                kind: CapabilityKind::WasmTool,
                effect: tool.effect,
                input_schema: tool.input_schema.clone(),
                output_schema: tool.output_schema.clone(),
                required_scopes: Vec::new(),
                data_classes: tool.data_classes.clone(),
                requires_approval: tool.requires_approval,
            });
        }

        for server in &agent.manifest.mcp {
            for tool in &server.tools {
                capabilities.push(Capability {
                    uri: CapabilityUri::new("mcp", &server.id, tool).map_err(|err| {
                        RegistryError::Validation(format!(
                            "agent {} MCP capability is invalid: {err}",
                            agent.manifest.id
                        ))
                    })?,
                    name: tool.clone(),
                    description: format!("MCP tool {tool} on {}", server.id),
                    kind: CapabilityKind::McpTool,
                    effect: CapabilityEffect::External,
                    input_schema: json!({"type": "object"}),
                    output_schema: Value::Null,
                    required_scopes: server.scopes.clone(),
                    data_classes: server.data_classes.clone(),
                    requires_approval: false,
                });
            }
            for resource in &server.resources {
                capabilities.push(Capability {
                    uri: CapabilityUri::new("mcp", &server.id, resource).map_err(|err| {
                        RegistryError::Validation(format!(
                            "agent {} MCP resource is invalid: {err}",
                            agent.manifest.id
                        ))
                    })?,
                    name: resource.clone(),
                    description: format!("MCP resource {resource} on {}", server.id),
                    kind: CapabilityKind::McpResource,
                    effect: CapabilityEffect::Read,
                    input_schema: Value::Null,
                    output_schema: Value::Null,
                    required_scopes: server.scopes.clone(),
                    data_classes: server.data_classes.clone(),
                    requires_approval: false,
                });
            }
        }

        for target_id in &agent.manifest.a2a.delegate_to {
            let target = self
                .get(target_id)
                .ok_or_else(|| RegistryError::UnknownAgent(target_id.clone()))?;
            for skill in &target.manifest.skills {
                capabilities.push(Capability {
                    uri: CapabilityUri::new("agent", target_id, &skill.id)
                        .expect("validated agent and skill ids form a valid URI"),
                    name: skill.id.clone(),
                    description: skill.description.clone(),
                    kind: CapabilityKind::A2aSkill,
                    effect: CapabilityEffect::External,
                    input_schema: skill.input_schema.clone(),
                    output_schema: skill.output_schema.clone(),
                    required_scopes: Vec::new(),
                    data_classes: skill.data_classes.clone(),
                    requires_approval: skill.requires_approval,
                });
            }
        }

        capabilities.sort_by(|left, right| left.uri.cmp(&right.uri));
        Ok(capabilities)
    }

    pub fn agent_card(&self, id: &str, base_url: &str) -> Result<Value, RegistryError> {
        let record = self
            .get(id)
            .ok_or_else(|| RegistryError::UnknownAgent(id.to_string()))?;
        let base = base_url.trim_end_matches('/');
        Ok(json!({
            "name": record.manifest.name,
            "description": record.manifest.role,
            "version": record.revision,
            "supportedInterfaces": [{
                "url": format!("{base}/a2a/{id}"),
                "protocolBinding": "HTTP+JSON",
                "protocolVersion": "1.0"
            }],
            "capabilities": {
                "streaming": true,
                "pushNotifications": false,
                "extendedAgentCard": false
            },
            "defaultInputModes": ["application/json", "text/plain"],
            "defaultOutputModes": ["application/json", "text/plain"],
            "skills": record.manifest.skills.iter().map(|skill| json!({
                "id": skill.id,
                "name": skill.id,
                "description": skill.description,
                "inputModes": ["application/json", "text/plain"],
                "outputModes": ["application/json", "text/plain"]
            })).collect::<Vec<_>>()
        }))
    }

    fn validate_graph(&self) -> Result<(), RegistryError> {
        for record in self.agents.values() {
            let manifest = &record.manifest;
            if let Some(manager) = &manifest.reports_to {
                self.require_agent(manager, &manifest.id, "reports_to")?;
            }
            for target in &manifest.a2a.delegate_to {
                let target_record = self.require_agent(target, &manifest.id, "delegate_to")?;
                let accepts = &target_record.manifest.a2a.accept_from;
                if !accepts.is_empty() && !accepts.iter().any(|caller| caller == &manifest.id) {
                    return Err(RegistryError::Validation(format!(
                        "agent {} delegates to {}, but {} does not accept tasks from it",
                        manifest.id, target, target
                    )));
                }
            }
            for caller in &manifest.a2a.accept_from {
                self.require_agent(caller, &manifest.id, "accept_from")?;
            }
            for tool in &manifest.wasm_tools {
                for target in &tool.permissions.delegate_to {
                    if !manifest
                        .a2a
                        .delegate_to
                        .iter()
                        .any(|allowed| allowed == target)
                    {
                        return Err(RegistryError::Validation(format!(
                            "Wasm tool {} requests delegation to {}, which agent {} is not allowed to delegate to",
                            tool.id, target, manifest.id
                        )));
                    }
                }
                let allowed_mcp = manifest
                    .mcp
                    .iter()
                    .flat_map(|server| {
                        server
                            .tools
                            .iter()
                            .map(move |name| format!("mcp://{}/{}", server.id, name))
                    })
                    .collect::<BTreeSet<_>>();
                for capability in &tool.permissions.mcp_tools {
                    if !allowed_mcp.contains(capability) {
                        return Err(RegistryError::Validation(format!(
                            "Wasm tool {} requests undeclared MCP capability {}",
                            tool.id, capability
                        )));
                    }
                }
            }
        }
        self.validate_reporting_cycles()?;
        // Compile every catalog during startup so malformed capability names
        // cannot remain latent until an agent first tries to use them.
        for id in self.agents.keys() {
            self.capabilities_for(id)?;
        }
        Ok(())
    }

    fn require_agent(
        &self,
        id: &str,
        source: &str,
        field: &str,
    ) -> Result<&AgentRecord, RegistryError> {
        self.get(id).ok_or_else(|| {
            RegistryError::Validation(format!(
                "agent {source} references unknown agent {id} in {field}"
            ))
        })
    }

    fn validate_reporting_cycles(&self) -> Result<(), RegistryError> {
        for id in self.agents.keys() {
            let mut seen = BTreeSet::new();
            let mut cursor = Some(id.as_str());
            while let Some(current) = cursor {
                if !seen.insert(current.to_string()) {
                    return Err(RegistryError::Validation(format!(
                        "reporting hierarchy contains a cycle involving {current}"
                    )));
                }
                cursor = self
                    .get(current)
                    .and_then(|record| record.manifest.reports_to.as_deref());
            }
        }
        Ok(())
    }
}

fn revision(manifest: &AgentManifest) -> Result<String, RegistryError> {
    let encoded = serde_json::to_vec(manifest).map_err(RegistryError::Serialize)?;
    let digest = Sha256::digest(encoded);
    Ok(hex::encode(&digest[..8]))
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("failed to read {path}: {message}")]
    Io { path: PathBuf, message: String },
    #[error("invalid agent manifest {path}: {source}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: ManifestError,
    },
    #[error("farm registry validation failed: {0}")]
    Validation(String),
    #[error("unknown agent: {0}")]
    UnknownAgent(String),
    #[error("failed to serialize agent manifest: {0}")]
    Serialize(serde_json::Error),
}

impl RegistryError {
    fn io(path: &Path, error: impl std::fmt::Display) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::AgentManifest;

    fn manifest(id: &str, delegate_to: &[&str], accept_from: &[&str]) -> AgentManifest {
        AgentManifest::from_toml(&format!(
            r#"
id = "{id}"
name = "{id}"
role = "worker"
[a2a]
delegate_to = [{}]
accept_from = [{}]
[[skills]]
id = "work"
description = "Do work"
"#,
            delegate_to
                .iter()
                .map(|item| format!("\"{item}\""))
                .collect::<Vec<_>>()
                .join(","),
            accept_from
                .iter()
                .map(|item| format!("\"{item}\""))
                .collect::<Vec<_>>()
                .join(",")
        ))
        .unwrap()
    }

    #[test]
    fn catalog_contains_only_declared_delegates() {
        let registry = FarmRegistry::from_manifests(vec![
            manifest("cto", &["backend"], &[]),
            manifest("backend", &[], &["cto"]),
            manifest("finance", &[], &[]),
        ])
        .unwrap();
        let catalog = registry.capabilities_for("cto").unwrap();
        assert!(catalog
            .iter()
            .any(|capability| capability.uri.as_str() == "agent://backend/work"));
        assert!(!catalog
            .iter()
            .any(|capability| capability.uri.as_str() == "agent://finance/work"));
    }

    #[test]
    fn rejects_incompatible_a2a_acl() {
        let error = FarmRegistry::from_manifests(vec![
            manifest("cto", &["backend"], &[]),
            manifest("backend", &[], &["ceo"]),
            manifest("ceo", &[], &[]),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("does not accept tasks"));
    }

    #[test]
    fn bundled_example_organization_is_valid() {
        let manifests = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../agents");
        let registry = FarmRegistry::load_dir(&manifests).unwrap();
        assert_eq!(registry.agents().count(), 5);
        assert!(registry
            .capabilities_for("backend-engineer")
            .unwrap()
            .iter()
            .any(|capability| capability.uri.as_str() == "mcp://observability/logs.search"));
    }
}
