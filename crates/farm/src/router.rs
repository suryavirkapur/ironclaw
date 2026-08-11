use crate::capability::{Capability, CapabilityKind, CapabilityUri};
use crate::registry::{FarmRegistry, RegistryError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Invocation {
    pub subject: String,
    pub task_id: String,
    pub capability: CapabilityUri,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub delegation_depth: u8,
    #[serde(default)]
    pub approved: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct InvocationResult {
    pub output: Value,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
}

#[async_trait]
pub trait CapabilityBackend: Send + Sync {
    fn scheme(&self) -> &'static str;

    async fn invoke(
        &self,
        capability: &Capability,
        invocation: &Invocation,
    ) -> Result<InvocationResult, RouterError>;
}

pub struct CapabilityRouter {
    registry: Arc<FarmRegistry>,
    backends: HashMap<&'static str, Arc<dyn CapabilityBackend>>,
}

impl CapabilityRouter {
    pub fn new(registry: Arc<FarmRegistry>) -> Self {
        Self {
            registry,
            backends: HashMap::new(),
        }
    }

    pub fn register_backend(&mut self, backend: Arc<dyn CapabilityBackend>) {
        self.backends.insert(backend.scheme(), backend);
    }

    pub fn authorize(&self, invocation: &Invocation) -> Result<Capability, RouterError> {
        let record = self
            .registry
            .get(&invocation.subject)
            .ok_or_else(|| RouterError::UnknownAgent(invocation.subject.clone()))?;
        if !record.manifest.enabled {
            return Err(RouterError::Denied(format!(
                "agent {} is disabled",
                invocation.subject
            )));
        }
        if invocation.task_id.trim().is_empty() {
            return Err(RouterError::Denied(
                "capability invocations require a task id".to_string(),
            ));
        }
        if invocation.capability.scheme() == "agent"
            && invocation.delegation_depth >= record.manifest.a2a.max_delegation_depth
        {
            return Err(RouterError::Denied(format!(
                "agent {} exceeded its delegation depth limit",
                invocation.subject
            )));
        }

        let capability = self
            .registry
            .capabilities_for(&invocation.subject)?
            .into_iter()
            .find(|candidate| candidate.uri == invocation.capability)
            .ok_or_else(|| {
                RouterError::Denied(format!(
                    "{} is not exposed to agent {}",
                    invocation.capability, invocation.subject
                ))
            })?;
        if capability.requires_approval && !invocation.approved {
            return Err(RouterError::ApprovalRequired(
                invocation.capability.to_string(),
            ));
        }
        Ok(capability)
    }

    pub async fn invoke(&self, invocation: Invocation) -> Result<InvocationResult, RouterError> {
        let capability = self.authorize(&invocation)?;
        if capability.kind == CapabilityKind::McpResource {
            return Err(RouterError::NotInvocable(capability.uri.to_string()));
        }
        let backend = self
            .backends
            .get(invocation.capability.scheme())
            .ok_or_else(|| {
                RouterError::BackendUnavailable(invocation.capability.scheme().to_string())
            })?;
        backend.invoke(&capability, &invocation).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("unknown agent: {0}")]
    UnknownAgent(String),
    #[error("capability denied: {0}")]
    Denied(String),
    #[error("approval required for capability: {0}")]
    ApprovalRequired(String),
    #[error("capability is context and cannot be invoked as a tool: {0}")]
    NotInvocable(String),
    #[error("no backend registered for capability scheme: {0}")]
    BackendUnavailable(String),
    #[error("capability backend failed: {0}")]
    Backend(String),
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::AgentManifest;
    use serde_json::json;

    struct EchoBackend;

    #[async_trait]
    impl CapabilityBackend for EchoBackend {
        fn scheme(&self) -> &'static str {
            "local"
        }

        async fn invoke(
            &self,
            _capability: &Capability,
            invocation: &Invocation,
        ) -> Result<InvocationResult, RouterError> {
            Ok(InvocationResult {
                output: invocation.input.clone(),
                artifact_ids: Vec::new(),
            })
        }
    }

    fn registry() -> Arc<FarmRegistry> {
        let manifest = AgentManifest::from_toml(
            r#"
id = "worker"
name = "Worker"
role = "Worker"
[[wasm_tools]]
id = "echo"
module = "echo.wasm"
description = "Echo input"
"#,
        )
        .unwrap();
        Arc::new(FarmRegistry::from_manifests(vec![manifest]).unwrap())
    }

    #[tokio::test]
    async fn routes_only_authorized_capabilities() {
        let mut router = CapabilityRouter::new(registry());
        router.register_backend(Arc::new(EchoBackend));
        let result = router
            .invoke(Invocation {
                subject: "worker".to_string(),
                task_id: "task-1".to_string(),
                capability: "local://worker/echo".parse().unwrap(),
                input: json!({"hello": "world"}),
                delegation_depth: 0,
                approved: false,
            })
            .await
            .unwrap();
        assert_eq!(result.output, json!({"hello": "world"}));

        let denied = router
            .authorize(&Invocation {
                subject: "worker".to_string(),
                task_id: "task-1".to_string(),
                capability: "mcp://secret/read".parse().unwrap(),
                input: Value::Null,
                delegation_depth: 0,
                approved: false,
            })
            .unwrap_err();
        assert!(matches!(denied, RouterError::Denied(_)));
    }
}
