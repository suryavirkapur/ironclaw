use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkspaceChannel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub member_ids: Vec<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
struct WorkspaceSnapshot {
    #[serde(default)]
    channels: BTreeMap<String, WorkspaceChannel>,
}

#[derive(Clone, Default)]
pub struct WorkspaceStore {
    channels: Arc<Mutex<BTreeMap<String, WorkspaceChannel>>>,
    snapshot_path: Option<Arc<PathBuf>>,
}

impl WorkspaceStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, WorkspaceError> {
        let path = path.into();
        let snapshot = if path.exists() {
            let contents = std::fs::read(&path).map_err(|err| WorkspaceError::Io {
                path: path.clone(),
                message: err.to_string(),
            })?;
            serde_json::from_slice(&contents).map_err(|err| WorkspaceError::Io {
                path: path.clone(),
                message: format!("invalid workspace snapshot: {err}"),
            })?
        } else {
            WorkspaceSnapshot::default()
        };
        Ok(Self {
            channels: Arc::new(Mutex::new(snapshot.channels)),
            snapshot_path: Some(Arc::new(path)),
        })
    }

    pub fn list(&self) -> Result<Vec<WorkspaceChannel>, WorkspaceError> {
        let channels = self.lock()?;
        let mut listed = channels.values().cloned().collect::<Vec<_>>();
        listed.sort_by(|left, right| {
            left.updated_at_ms
                .cmp(&right.updated_at_ms)
                .reverse()
                .then_with(|| {
                    left.name
                        .to_ascii_lowercase()
                        .cmp(&right.name.to_ascii_lowercase())
                })
        });
        Ok(listed)
    }

    pub fn get(&self, id: &str) -> Result<Option<WorkspaceChannel>, WorkspaceError> {
        Ok(self.lock()?.get(id).cloned())
    }

    pub fn create(
        &self,
        name: &str,
        topic: &str,
        member_ids: Vec<String>,
        created_at_ms: u64,
    ) -> Result<WorkspaceChannel, WorkspaceError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(WorkspaceError::Invalid("channel name is required".into()));
        }
        let id = slug_id(name);
        validate_id(&id)?;
        let mut members = unique_members(member_ids)?;
        members.sort();
        let channel = WorkspaceChannel {
            id: id.clone(),
            name: name.to_string(),
            topic: topic.trim().to_string(),
            member_ids: members,
            created_at_ms,
            updated_at_ms: created_at_ms,
        };
        let mut channels = self.lock()?;
        if channels.contains_key(&id) {
            return Err(WorkspaceError::AlreadyExists(id));
        }
        channels.insert(id, channel.clone());
        drop(channels);
        self.persist()?;
        Ok(channel)
    }

    pub fn add_members(
        &self,
        channel_id: &str,
        member_ids: Vec<String>,
        updated_at_ms: u64,
    ) -> Result<WorkspaceChannel, WorkspaceError> {
        validate_id(channel_id)?;
        let incoming = unique_members(member_ids)?;
        if incoming.is_empty() {
            return Err(WorkspaceError::Invalid(
                "at least one agent is required".into(),
            ));
        }
        let mut channels = self.lock()?;
        let channel = channels
            .get_mut(channel_id)
            .ok_or_else(|| WorkspaceError::NotFound(channel_id.to_string()))?;
        for agent_id in incoming {
            if !channel
                .member_ids
                .iter()
                .any(|existing| existing == &agent_id)
            {
                channel.member_ids.push(agent_id);
            }
        }
        channel.member_ids.sort();
        channel.updated_at_ms = updated_at_ms;
        let updated = channel.clone();
        drop(channels);
        self.persist()?;
        Ok(updated)
    }

    fn persist(&self) -> Result<(), WorkspaceError> {
        let Some(path) = &self.snapshot_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| WorkspaceError::Io {
                path: path.as_ref().clone(),
                message: err.to_string(),
            })?;
        }
        let snapshot = WorkspaceSnapshot {
            channels: self.lock()?.clone(),
        };
        let encoded = serde_json::to_vec_pretty(&snapshot).map_err(|err| WorkspaceError::Io {
            path: path.as_ref().clone(),
            message: err.to_string(),
        })?;
        std::fs::write(path.as_ref(), encoded).map_err(|err| WorkspaceError::Io {
            path: path.as_ref().clone(),
            message: err.to_string(),
        })
    }

    fn lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, WorkspaceChannel>>, WorkspaceError> {
        self.channels.lock().map_err(|_| WorkspaceError::Poisoned)
    }
}

fn slug_id(name: &str) -> String {
    let mut slug = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

fn unique_members(member_ids: Vec<String>) -> Result<Vec<String>, WorkspaceError> {
    let mut unique = Vec::new();
    for raw in member_ids {
        let id = raw.trim();
        if id.is_empty() {
            continue;
        }
        validate_id(id)?;
        if !unique.iter().any(|existing: &String| existing == id) {
            unique.push(id.to_string());
        }
    }
    Ok(unique)
}

fn validate_id(value: &str) -> Result<(), WorkspaceError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(WorkspaceError::Invalid(
            "id must be 1-64 ASCII letters, digits, '-' or '_'".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("channel already exists: {0}")]
    AlreadyExists(String),
    #[error("channel not found: {0}")]
    NotFound(String),
    #[error("invalid workspace data: {0}")]
    Invalid(String),
    #[error("workspace lock poisoned")]
    Poisoned,
    #[error("workspace I/O failed at {path}: {message}")]
    Io { path: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_adds_members() {
        let store = WorkspaceStore::default();
        let channel = store
            .create(
                "Sales Outbound",
                "Nightly outreach",
                vec!["chief".into()],
                10,
            )
            .unwrap();
        assert_eq!(channel.id, "sales-outbound");
        let updated = store
            .add_members(
                "sales-outbound",
                vec!["account-manager".into(), "chief".into()],
                20,
            )
            .unwrap();
        assert_eq!(updated.member_ids, vec!["account-manager", "chief"]);
    }

    #[test]
    fn persists_channels_across_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("workspace.json");
        let store = WorkspaceStore::open(&path).unwrap();
        store.create("Inbox", "", vec!["maya".into()], 1).unwrap();
        let reopened = WorkspaceStore::open(path).unwrap();
        assert_eq!(reopened.list().unwrap()[0].id, "inbox");
    }
}
