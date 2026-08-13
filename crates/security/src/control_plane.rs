use crate::pairing::constant_time_eq;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlPlaneRole {
    Admin,
    Operator,
    Viewer,
}

impl ControlPlaneRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Operator => "operator",
            Self::Viewer => "viewer",
        }
    }

    pub fn can_write(self) -> bool {
        matches!(self, Self::Admin | Self::Operator)
    }
}

#[derive(Clone, Debug)]
pub struct ControlPlanePrincipal {
    pub id: String,
    pub organization_id: String,
    pub role: ControlPlaneRole,
    pub default_agent: String,
    allowed_agents: HashSet<String>,
}

impl ControlPlanePrincipal {
    pub fn new(
        id: impl Into<String>,
        organization_id: impl Into<String>,
        role: ControlPlaneRole,
        default_agent: impl Into<String>,
        allowed_agents: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            id: id.into(),
            organization_id: organization_id.into(),
            role,
            default_agent: default_agent.into(),
            allowed_agents: allowed_agents.into_iter().collect(),
        }
    }

    pub fn allows_agent(&self, agent_id: &str) -> bool {
        self.role == ControlPlaneRole::Admin && self.allowed_agents.is_empty()
            || self.allowed_agents.contains(agent_id)
    }
}

#[derive(Clone)]
struct Credential {
    token_hash: [u8; 32],
    principal: ControlPlanePrincipal,
}

#[derive(Clone, Default)]
pub struct ControlPlaneAuthorizer {
    credentials: Vec<Credential>,
}

impl ControlPlaneAuthorizer {
    pub fn new(entries: Vec<(String, ControlPlanePrincipal)>) -> Result<Self, String> {
        let mut credentials = Vec::with_capacity(entries.len());
        let mut token_hashes = HashSet::new();
        for (token, principal) in entries {
            if token.trim().len() < 32 {
                return Err(format!(
                    "control-plane token for {} must contain at least 32 characters",
                    principal.id
                ));
            }
            let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            if !token_hashes.insert(token_hash) {
                return Err("control-plane bearer tokens must be unique".to_string());
            }
            credentials.push(Credential {
                token_hash,
                principal,
            });
        }
        Ok(Self { credentials })
    }

    pub fn authenticate(&self, token: &str) -> Option<ControlPlanePrincipal> {
        let supplied: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        self.credentials
            .iter()
            .find(|entry| constant_time_eq(&entry.token_hash, &supplied))
            .map(|entry| entry.principal.clone())
    }

    pub fn is_empty(&self) -> bool {
        self.credentials.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct AuditEvent<'a> {
    pub request_id: &'a str,
    pub occurred_at_ms: i64,
    pub principal: Option<&'a ControlPlanePrincipal>,
    pub method: &'a str,
    pub path: &'a str,
    pub action: &'a str,
    pub resource: &'a str,
    pub decision: &'a str,
    pub status_code: u16,
}

pub fn append_audit_event(conn: &Connection, event: &AuditEvent<'_>) -> Result<(), String> {
    conn.execute(
        "insert into control_plane_audit (
            request_id, occurred_at_ms, principal_id, organization_id, role,
            method, path, action, resource, decision, status_code
         ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            event.request_id,
            event.occurred_at_ms,
            event.principal.map(|value| value.id.as_str()),
            event.principal.map(|value| value.organization_id.as_str()),
            event.principal.map(|value| value.role.as_str()),
            event.method,
            event.path,
            event.action,
            event.resource,
            event.decision,
            event.status_code,
        ],
    )
    .map_err(|err| format!("control-plane audit write failed: {err}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_auth_is_scoped_and_rejects_short_or_duplicate_tokens() {
        let principal = ControlPlanePrincipal::new(
            "alice",
            "org-a",
            ControlPlaneRole::Operator,
            "agent-a",
            vec!["agent-a".to_string()],
        );
        let token = "a-secure-control-plane-token-0001".to_string();
        let auth = ControlPlaneAuthorizer::new(vec![(token.clone(), principal)]).unwrap();
        let authenticated = auth.authenticate(&token).unwrap();
        assert!(authenticated.allows_agent("agent-a"));
        assert!(!authenticated.allows_agent("agent-b"));
        assert!(auth.authenticate("wrong-token").is_none());
        assert!(
            ControlPlaneAuthorizer::new(vec![("short".into(), authenticated.clone())]).is_err()
        );
        assert!(ControlPlaneAuthorizer::new(vec![
            (token.clone(), authenticated.clone()),
            (token, authenticated),
        ])
        .is_err());
    }

    #[test]
    fn audit_events_are_durable() {
        let conn = Connection::open_in_memory().unwrap();
        crate::initialize_schema(&conn).unwrap();
        let principal = ControlPlanePrincipal::new(
            "alice",
            "org-a",
            ControlPlaneRole::Admin,
            "agent-a",
            Vec::new(),
        );
        append_audit_event(
            &conn,
            &AuditEvent {
                request_id: "req-1",
                occurred_at_ms: 42,
                principal: Some(&principal),
                method: "GET",
                path: "/api/farm/tasks",
                action: "read",
                resource: "farm/tasks",
                decision: "allow",
                status_code: 200,
            },
        )
        .unwrap();
        let organization: String = conn
            .query_row(
                "select organization_id from control_plane_audit where request_id='req-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(organization, "org-a");
    }
}
