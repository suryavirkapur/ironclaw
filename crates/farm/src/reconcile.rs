use crate::registry::FarmRegistry;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePhase {
    Pending,
    Starting,
    Running,
    Draining,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentRuntimeState {
    pub agent_id: String,
    pub applied_revision: String,
    pub phase: RuntimePhase,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ReconcileAction {
    Start {
        agent_id: String,
        revision: String,
    },
    Restart {
        agent_id: String,
        from_revision: String,
        to_revision: String,
    },
    Stop {
        agent_id: String,
        reason: String,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReconcilePlan {
    pub actions: Vec<ReconcileAction>,
}

impl ReconcilePlan {
    pub fn build(desired: &FarmRegistry, actual: &BTreeMap<String, AgentRuntimeState>) -> Self {
        let mut actions = Vec::new();
        let desired_ids = desired
            .agents()
            .filter(|record| record.manifest.enabled)
            .map(|record| record.manifest.id.clone())
            .collect::<BTreeSet<_>>();

        for record in desired.agents() {
            let id = &record.manifest.id;
            if !record.manifest.enabled {
                if actual.contains_key(id) {
                    actions.push(ReconcileAction::Stop {
                        agent_id: id.clone(),
                        reason: "disabled in manifest".to_string(),
                    });
                }
                continue;
            }
            match actual.get(id) {
                None => actions.push(ReconcileAction::Start {
                    agent_id: id.clone(),
                    revision: record.revision.clone(),
                }),
                Some(state)
                    if matches!(state.phase, RuntimePhase::Stopped | RuntimePhase::Failed) =>
                {
                    actions.push(ReconcileAction::Start {
                        agent_id: id.clone(),
                        revision: record.revision.clone(),
                    });
                }
                Some(state) if state.applied_revision != record.revision => {
                    actions.push(ReconcileAction::Restart {
                        agent_id: id.clone(),
                        from_revision: state.applied_revision.clone(),
                        to_revision: record.revision.clone(),
                    });
                }
                Some(_) => {}
            }
        }

        for id in actual.keys() {
            if !desired_ids.contains(id) && desired.get(id).is_none() {
                actions.push(ReconcileAction::Stop {
                    agent_id: id.clone(),
                    reason: "manifest removed".to_string(),
                });
            }
        }
        Self { actions }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::AgentManifest;

    #[test]
    fn plans_start_restart_and_stop() {
        let desired = FarmRegistry::from_manifests(vec![
            AgentManifest::from_toml("id='new'\nname='New'\nrole='worker'").unwrap(),
            AgentManifest::from_toml("id='changed'\nname='Changed'\nrole='worker'").unwrap(),
        ])
        .unwrap();
        let actual = BTreeMap::from([
            (
                "changed".to_string(),
                AgentRuntimeState {
                    agent_id: "changed".to_string(),
                    applied_revision: "old".to_string(),
                    phase: RuntimePhase::Running,
                },
            ),
            (
                "removed".to_string(),
                AgentRuntimeState {
                    agent_id: "removed".to_string(),
                    applied_revision: "old".to_string(),
                    phase: RuntimePhase::Running,
                },
            ),
        ]);
        let plan = ReconcilePlan::build(&desired, &actual);
        assert_eq!(plan.actions.len(), 3);
    }
}
