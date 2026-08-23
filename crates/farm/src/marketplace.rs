//! Agent Tool Marketplace.
//!
//! Tools become invocable only after they are declared on an agent manifest and
//! the farm registry is recompiled. This module is the catalog plus the
//! install/create mutations used by the control plane and desktop app.

use crate::capability::CapabilityEffect;
use crate::manifest::{
    A2aPolicy, AgentManifest, AgentSkill, McpServerAccess, MemoryConfig, ModelConfig,
    WasmPermissions, WasmPolicy, WasmTool,
};
use crate::registry::{FarmRegistry, RegistryError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Returned with the marketplace catalog so the product can explain loading.
pub const HOW_TOOLS_LOAD: &str = "\
Tools are not discovered at runtime and are not installed by chatting with an agent.\n\n\
1. Declare the tool on the agent's `{id}.agent.toml` under `[[wasm_tools]]` (a sandboxed \
Wasm module in `wasm.tools_dir`), `[[mcp]]` (a host-brokered MCP server and its allowlisted \
tool names), or `[a2a]` / `[[skills]]` (A2A skills other agents may invoke).\n\
2. ironclawd loads every manifest in `farm.manifests_dir` into a FarmRegistry. The capability \
router will only authorize `local://agent/tool`, `mcp://server/tool`, and `agent://peer/skill` \
URIs that this registry compiled.\n\
3. When that agent's MicroVM starts, the host sends the manifest into the guest. The guest \
WasmExecutor maps each `[[wasm_tools]]` row to a module under the agent's tools directory \
(no WASI, no ambient network). MCP and A2A never enter the guest — the host brokers them \
after the registry check.\n\
4. Installing from this marketplace rewrites the manifest and reloads the registry. The agent \
picks up the new catalog on the next VM start.\n\n\
Guest builtins such as file_read or browser come from guest config flags, not from the farm \
catalog. Farm-local work is Wasm-only.";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MarketplaceKind {
    Wasm,
    Mcp,
    A2a,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MarketplaceEntry {
    pub id: String,
    pub kind: MarketplaceKind,
    pub title: String,
    pub summary: String,
    pub publisher: String,
    pub loads_as: String,
    pub installed_on: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MarketplaceCatalog {
    pub how_tools_load: String,
    pub entries: Vec<MarketplaceEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CreateAgentSpec {
    pub id: String,
    pub name: String,
    pub role: String,
    #[serde(default)]
    pub reports_to: Option<String>,
    #[serde(default)]
    pub skill_id: Option<String>,
    #[serde(default)]
    pub skill_description: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum MarketplaceError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

impl MarketplaceError {
    fn msg(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}

struct BuiltinTool {
    id: &'static str,
    kind: MarketplaceKind,
    title: &'static str,
    summary: &'static str,
    publisher: &'static str,
    loads_as: &'static str,
    wasm: Option<WasmTool>,
    mcp: Option<McpServerAccess>,
}

fn object_schema() -> serde_json::Value {
    json!({"type": "object"})
}

fn wasm_tool(id: &str, module: &str, description: &str, effect: CapabilityEffect) -> WasmTool {
    WasmTool {
        id: id.to_string(),
        module: PathBuf::from(module),
        description: description.to_string(),
        input_schema: object_schema(),
        output_schema: object_schema(),
        effect,
        data_classes: Vec::new(),
        requires_approval: false,
        permissions: WasmPermissions::default(),
        limits: Default::default(),
    }
}

fn mcp_server(
    id: &str,
    server: &str,
    credential: &str,
    tools: &[&str],
    resources: &[&str],
    scopes: &[&str],
) -> McpServerAccess {
    McpServerAccess {
        id: id.to_string(),
        server: server.to_string(),
        protocol_version: "2025-06-18".to_string(),
        credential: Some(credential.to_string()),
        tools: tools.iter().map(|tool| (*tool).to_string()).collect(),
        resources: resources.iter().map(|item| (*item).to_string()).collect(),
        scopes: scopes.iter().map(|item| (*item).to_string()).collect(),
        data_classes: vec!["internal".to_string()],
    }
}

fn builtins() -> Vec<BuiltinTool> {
    vec![
        BuiltinTool {
            id: "wasm.cluster_logs",
            kind: MarketplaceKind::Wasm,
            title: "Cluster logs",
            summary: "Normalize and cluster related log lines inside the agent's Wasm sandbox.",
            publisher: "Ironclaw",
            loads_as: "local://<agent>/cluster_logs → wasm.tools_dir/cluster_logs.wasm",
            wasm: Some(wasm_tool(
                "cluster_logs",
                "cluster_logs.wasm",
                "Normalize and cluster related log records.",
                CapabilityEffect::Read,
            )),
            mcp: None,
        },
        BuiltinTool {
            id: "wasm.diff_review",
            kind: MarketplaceKind::Wasm,
            title: "Diff review",
            summary: "Score a unified diff for risk, missing tests, and rollback notes.",
            publisher: "Ironclaw",
            loads_as: "local://<agent>/diff_review → wasm.tools_dir/diff_review.wasm",
            wasm: Some(wasm_tool(
                "diff_review",
                "diff_review.wasm",
                "Review a unified diff and return findings.",
                CapabilityEffect::Read,
            )),
            mcp: None,
        },
        BuiltinTool {
            id: "wasm.test_runner",
            kind: MarketplaceKind::Wasm,
            title: "Test runner",
            summary: "Parse and summarize automated test output without leaving the sandbox.",
            publisher: "Ironclaw",
            loads_as: "local://<agent>/test_runner → wasm.tools_dir/test_runner.wasm",
            wasm: Some(wasm_tool(
                "test_runner",
                "test_runner.wasm",
                "Summarize automated test results.",
                CapabilityEffect::Read,
            )),
            mcp: None,
        },
        BuiltinTool {
            id: "wasm.web_fetch",
            kind: MarketplaceKind::Wasm,
            title: "Allowlisted fetch",
            summary: "Fetch an allowlisted URL through host-mediated Wasm I/O, never raw sockets.",
            publisher: "Ironclaw",
            loads_as: "local://<agent>/web_fetch → wasm.tools_dir/web_fetch.wasm",
            wasm: Some(wasm_tool(
                "web_fetch",
                "web_fetch.wasm",
                "Fetch an allowlisted URL and return extracted text.",
                CapabilityEffect::External,
            )),
            mcp: None,
        },
        BuiltinTool {
            id: "mcp.observability",
            kind: MarketplaceKind::Mcp,
            title: "Observability MCP",
            summary: "Host-brokered logs.search and traces.get. Credentials stay on the host.",
            publisher: "Ironclaw",
            loads_as: "mcp://observability/logs.search (host MCP gateway, never in the guest)",
            wasm: None,
            mcp: Some(mcp_server(
                "observability",
                "https://mcp.internal.example/observability",
                "observability",
                &["logs.search", "traces.get"],
                &["service-catalog", "runbooks"],
                &["observability:read"],
            )),
        },
        BuiltinTool {
            id: "mcp.github",
            kind: MarketplaceKind::Mcp,
            title: "GitHub MCP",
            summary: "Host-brokered issues.list and pulls.get. Tokens never enter the guest.",
            publisher: "Ironclaw",
            loads_as: "mcp://github/issues.list (host MCP gateway)",
            wasm: None,
            mcp: Some(mcp_server(
                "github",
                "https://mcp.internal.example/github",
                "github",
                &["issues.list", "pulls.get"],
                &["repo-catalog"],
                &["github:read"],
            )),
        },
    ]
}

pub fn catalog(registry: &FarmRegistry) -> MarketplaceCatalog {
    let mut entries = Vec::new();
    for builtin in builtins() {
        entries.push(MarketplaceEntry {
            id: builtin.id.to_string(),
            kind: builtin.kind,
            title: builtin.title.to_string(),
            summary: builtin.summary.to_string(),
            publisher: builtin.publisher.to_string(),
            loads_as: builtin.loads_as.to_string(),
            installed_on: agents_with_builtin(registry, &builtin),
        });
    }
    for record in registry.agents() {
        for skill in &record.manifest.skills {
            let id = format!("a2a.{}.{}", record.manifest.id, skill.id);
            let installed_on = registry
                .agents()
                .filter(|candidate| {
                    candidate
                        .manifest
                        .a2a
                        .delegate_to
                        .iter()
                        .any(|target| target == &record.manifest.id)
                })
                .map(|candidate| candidate.manifest.id.clone())
                .collect();
            entries.push(MarketplaceEntry {
                id,
                kind: MarketplaceKind::A2a,
                title: format!("{} · {}", record.manifest.name, skill.id.replace('_', " ")),
                summary: skill.description.clone(),
                publisher: record.manifest.id.clone(),
                loads_as: format!(
                    "agent://{}/{} (A2A task; host authorized, guest never dials the peer)",
                    record.manifest.id, skill.id
                ),
                installed_on,
            });
        }
    }
    MarketplaceCatalog {
        how_tools_load: HOW_TOOLS_LOAD.to_string(),
        entries,
    }
}

fn agents_with_builtin(registry: &FarmRegistry, builtin: &BuiltinTool) -> Vec<String> {
    registry
        .agents()
        .filter(|record| match builtin.kind {
            MarketplaceKind::Wasm => builtin.wasm.as_ref().is_some_and(|tool| {
                record
                    .manifest
                    .wasm_tools
                    .iter()
                    .any(|have| have.id == tool.id)
            }),
            MarketplaceKind::Mcp => builtin.mcp.as_ref().is_some_and(|server| {
                record.manifest.mcp.iter().any(|have| {
                    have.id == server.id
                        && server
                            .tools
                            .iter()
                            .all(|tool| have.tools.iter().any(|allowed| allowed == tool))
                })
            }),
            MarketplaceKind::A2a => false,
        })
        .map(|record| record.manifest.id.clone())
        .collect()
}

pub fn new_agent(spec: &CreateAgentSpec) -> Result<AgentManifest, MarketplaceError> {
    let id = spec.id.trim();
    let name = spec.name.trim();
    let role = spec.role.trim();
    if id.is_empty() || name.is_empty() || role.is_empty() {
        return Err(MarketplaceError::msg(
            "agent id, name, and role are required",
        ));
    }
    let skill_id = spec
        .skill_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("assist");
    let skill_description = spec
        .skill_description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Help teammates with assigned work and report evidence.")
        .to_string();
    let mut wasm = WasmPolicy::default();
    wasm.may_create = true;
    let mut a2a = A2aPolicy::default();
    if let Some(manager) = spec
        .reports_to
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        a2a.accept_from.push(manager.to_string());
    }
    Ok(AgentManifest {
        schema_version: 1,
        id: id.to_string(),
        name: name.to_string(),
        role: role.to_string(),
        enabled: true,
        reports_to: spec
            .reports_to
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        model: ModelConfig::default(),
        compute: Default::default(),
        memory: MemoryConfig::default(),
        wasm,
        wasm_tools: Vec::new(),
        mcp: Vec::new(),
        a2a,
        skills: vec![AgentSkill {
            id: skill_id.to_string(),
            description: skill_description,
            input_schema: object_schema(),
            output_schema: object_schema(),
            data_classes: Vec::new(),
            requires_approval: false,
        }],
    })
}

pub fn install(
    registry: &FarmRegistry,
    agent_id: &str,
    listing_id: &str,
) -> Result<Vec<AgentManifest>, MarketplaceError> {
    let mut manifests = snapshot(registry);
    apply_listing(&mut manifests, agent_id, listing_id)?;
    FarmRegistry::from_manifests(manifests.values().cloned().collect())?;
    Ok(changed(registry, &manifests))
}

pub fn create_agent(
    registry: &FarmRegistry,
    spec: CreateAgentSpec,
) -> Result<Vec<AgentManifest>, MarketplaceError> {
    let mut manifests = snapshot(registry);
    if manifests.contains_key(spec.id.trim()) {
        return Err(MarketplaceError::msg(format!(
            "agent {} already exists",
            spec.id.trim()
        )));
    }
    let created = new_agent(&spec)?;
    if let Some(manager_id) = created.reports_to.clone() {
        let manager = manifests.get_mut(&manager_id).ok_or_else(|| {
            MarketplaceError::msg(format!("reports_to agent {manager_id} does not exist"))
        })?;
        if !manager.a2a.delegate_to.iter().any(|id| id == &created.id) {
            manager.a2a.delegate_to.push(created.id.clone());
        }
    }
    let agent_id = created.id.clone();
    manifests.insert(agent_id.clone(), created);
    for tool_id in &spec.tools {
        apply_listing(&mut manifests, &agent_id, tool_id)?;
    }
    FarmRegistry::from_manifests(manifests.values().cloned().collect())?;
    Ok(changed(registry, &manifests))
}

fn snapshot(registry: &FarmRegistry) -> BTreeMap<String, AgentManifest> {
    registry
        .agents()
        .map(|record| (record.manifest.id.clone(), record.manifest.clone()))
        .collect()
}

fn changed(
    registry: &FarmRegistry,
    manifests: &BTreeMap<String, AgentManifest>,
) -> Vec<AgentManifest> {
    manifests
        .values()
        .filter(|manifest| {
            registry
                .get(&manifest.id)
                .map(|record| &record.manifest != *manifest)
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

fn apply_listing(
    manifests: &mut BTreeMap<String, AgentManifest>,
    agent_id: &str,
    listing_id: &str,
) -> Result<(), MarketplaceError> {
    if manifests.get(agent_id).is_none() {
        return Err(MarketplaceError::msg(format!("unknown agent: {agent_id}")));
    }
    if let Some(builtin) = builtins().into_iter().find(|item| item.id == listing_id) {
        let agent = manifests.get_mut(agent_id).expect("agent exists");
        match builtin.kind {
            MarketplaceKind::Wasm => {
                let tool = builtin.wasm.expect("wasm builtin");
                if !agent.wasm_tools.iter().any(|have| have.id == tool.id) {
                    agent.wasm_tools.push(tool);
                }
            }
            MarketplaceKind::Mcp => {
                let server = builtin.mcp.expect("mcp builtin");
                merge_mcp(agent, server);
            }
            MarketplaceKind::A2a => {}
        }
        return Ok(());
    }

    let rest = listing_id.strip_prefix("a2a.").ok_or_else(|| {
        MarketplaceError::msg(format!("unknown marketplace listing: {listing_id}"))
    })?;
    let (target_id, skill_id) = rest.split_once('.').ok_or_else(|| {
        MarketplaceError::msg(format!("unknown marketplace listing: {listing_id}"))
    })?;
    if target_id == agent_id {
        return Err(MarketplaceError::msg(
            "an agent cannot install its own A2A skill as a delegate target",
        ));
    }
    let has_skill = manifests
        .get(target_id)
        .ok_or_else(|| MarketplaceError::msg(format!("unknown agent: {target_id}")))?
        .skills
        .iter()
        .any(|skill| skill.id == skill_id);
    if !has_skill {
        return Err(MarketplaceError::msg(format!(
            "agent {target_id} does not publish skill {skill_id}"
        )));
    }
    {
        let agent = manifests.get_mut(agent_id).expect("agent exists");
        if !agent.a2a.delegate_to.iter().any(|id| id == target_id) {
            agent.a2a.delegate_to.push(target_id.to_string());
        }
    }
    let target = manifests
        .get_mut(target_id)
        .ok_or_else(|| MarketplaceError::msg(format!("unknown agent: {target_id}")))?;
    if !target.a2a.accept_from.is_empty() && !target.a2a.accept_from.iter().any(|id| id == agent_id)
    {
        target.a2a.accept_from.push(agent_id.to_string());
    }
    Ok(())
}

fn merge_mcp(agent: &mut AgentManifest, server: McpServerAccess) {
    if let Some(existing) = agent.mcp.iter_mut().find(|have| have.id == server.id) {
        for tool in server.tools {
            if !existing.tools.iter().any(|have| have == &tool) {
                existing.tools.push(tool);
            }
        }
        for resource in server.resources {
            if !existing.resources.iter().any(|have| have == &resource) {
                existing.resources.push(resource);
            }
        }
        for scope in server.scopes {
            if !existing.scopes.iter().any(|have| have == &scope) {
                existing.scopes.push(scope);
            }
        }
        if existing.credential.is_none() {
            existing.credential = server.credential;
        }
    } else {
        agent.mcp.push(server);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team() -> FarmRegistry {
        FarmRegistry::from_manifests(vec![
            AgentManifest::from_toml(
                r#"
id = "lead"
name = "Sam"
role = "Lead"
[a2a]
delegate_to = ["worker"]
[[skills]]
id = "plan_work"
description = "Plan work"
"#,
            )
            .unwrap(),
            AgentManifest::from_toml(
                r#"
id = "worker"
name = "Alex"
role = "Engineer"
reports_to = "lead"
[a2a]
accept_from = ["lead"]
[[skills]]
id = "write_code"
description = "Write code"
"#,
            )
            .unwrap(),
        ])
        .unwrap()
    }

    #[test]
    fn catalog_includes_builtins_and_a2a_skills() {
        let catalog = catalog(&team());
        assert!(catalog.how_tools_load.contains("FarmRegistry"));
        assert!(catalog
            .entries
            .iter()
            .any(|entry| entry.id == "wasm.cluster_logs"));
        assert!(catalog
            .entries
            .iter()
            .any(|entry| entry.id == "a2a.worker.write_code"));
    }

    #[test]
    fn installing_wasm_and_mcp_updates_that_agent() {
        let registry = team();
        let changed = install(&registry, "worker", "wasm.cluster_logs").unwrap();
        assert_eq!(changed.len(), 1);
        assert!(changed[0]
            .wasm_tools
            .iter()
            .any(|tool| tool.id == "cluster_logs"));

        let changed = install(&registry, "worker", "mcp.observability").unwrap();
        assert!(changed[0].mcp.iter().any(|server| {
            server.id == "observability" && server.tools.iter().any(|tool| tool == "logs.search")
        }));
    }

    #[test]
    fn installing_a2a_skill_grants_delegate_route() {
        let registry = FarmRegistry::from_manifests(vec![
            AgentManifest::from_toml(
                r#"
id = "lead"
name = "Sam"
role = "Lead"
[[skills]]
id = "plan_work"
description = "Plan work"
"#,
            )
            .unwrap(),
            AgentManifest::from_toml(
                r#"
id = "worker"
name = "Alex"
role = "Engineer"
[[skills]]
id = "write_code"
description = "Write code"
"#,
            )
            .unwrap(),
        ])
        .unwrap();
        let changed = install(&registry, "lead", "a2a.worker.write_code").unwrap();
        let lead = changed
            .iter()
            .find(|manifest| manifest.id == "lead")
            .unwrap();
        assert!(lead.a2a.delegate_to.iter().any(|id| id == "worker"));
    }

    #[test]
    fn create_agent_with_tools_validates_graph() {
        let registry = team();
        let created = create_agent(
            &registry,
            CreateAgentSpec {
                id: "qa".into(),
                name: "Quinn".into(),
                role: "QA Engineer".into(),
                reports_to: Some("lead".into()),
                skill_id: Some("verify_release".into()),
                skill_description: Some("Verify a release.".into()),
                tools: vec!["wasm.test_runner".into(), "a2a.worker.write_code".into()],
            },
        )
        .unwrap();
        let qa = created.iter().find(|manifest| manifest.id == "qa").unwrap();
        assert!(qa.wasm_tools.iter().any(|tool| tool.id == "test_runner"));
        assert!(qa.a2a.delegate_to.iter().any(|id| id == "worker"));
        assert!(qa.a2a.accept_from.iter().any(|id| id == "lead"));
        let lead = created
            .iter()
            .find(|manifest| manifest.id == "lead")
            .unwrap();
        assert!(lead.a2a.delegate_to.iter().any(|id| id == "qa"));
        let worker = created
            .iter()
            .find(|manifest| manifest.id == "worker")
            .unwrap();
        assert!(worker.a2a.accept_from.iter().any(|id| id == "qa"));
    }
}
