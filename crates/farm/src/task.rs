use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Submitted,
    Working,
    InputRequired,
    Completed,
    Failed,
    Canceled,
    Rejected,
}

impl TaskState {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Canceled | Self::Rejected
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct FarmTask {
    pub id: String,
    pub context_id: String,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    pub requester: String,
    pub assignee: String,
    pub skill: String,
    pub state: TaskState,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub output: Option<Value>,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
    #[serde(default)]
    pub delegation_depth: u8,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Default)]
pub struct TaskLedger {
    tasks: Arc<Mutex<BTreeMap<String, FarmTask>>>,
    snapshot_path: Option<Arc<PathBuf>>,
}

impl TaskLedger {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, TaskError> {
        let path = path.into();
        let tasks = if path.exists() {
            let contents = std::fs::read(&path).map_err(|err| TaskError::Io {
                path: path.clone(),
                message: err.to_string(),
            })?;
            serde_json::from_slice(&contents).map_err(|err| TaskError::Io {
                path: path.clone(),
                message: format!("invalid task snapshot: {err}"),
            })?
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            tasks: Arc::new(Mutex::new(tasks)),
            snapshot_path: Some(Arc::new(path)),
        })
    }

    pub fn insert(&self, task: FarmTask) -> Result<(), TaskError> {
        validate_task(&task)?;
        let mut tasks = self.tasks.lock().map_err(|_| TaskError::Poisoned)?;
        if tasks.contains_key(&task.id) {
            return Err(TaskError::AlreadyExists(task.id));
        }
        if let Some(parent) = &task.parent_task_id {
            let parent_task = tasks
                .get(parent)
                .ok_or_else(|| TaskError::ParentNotFound(parent.clone()))?;
            if task.delegation_depth != parent_task.delegation_depth.saturating_add(1) {
                return Err(TaskError::Invalid(
                    "child task delegation depth must equal parent depth plus one".to_string(),
                ));
            }
        } else if task.delegation_depth != 0 {
            return Err(TaskError::Invalid(
                "root task delegation depth must be zero".to_string(),
            ));
        }
        let id = task.id.clone();
        tasks.insert(id.clone(), task);
        if let Err(err) = self.persist(&tasks) {
            tasks.remove(&id);
            return Err(err);
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<FarmTask>, TaskError> {
        Ok(self
            .tasks
            .lock()
            .map_err(|_| TaskError::Poisoned)?
            .get(id)
            .cloned())
    }

    pub fn list(&self) -> Result<Vec<FarmTask>, TaskError> {
        Ok(self
            .tasks
            .lock()
            .map_err(|_| TaskError::Poisoned)?
            .values()
            .cloned()
            .collect())
    }

    /// Mark tasks whose execution was interrupted by a daemon restart as failed.
    /// The current runtime does not checkpoint guest execution, so these tasks
    /// cannot truthfully remain submitted/working after the process restarts.
    pub fn fail_incomplete_on_startup(
        &self,
        reason: &str,
        updated_at_ms: u64,
    ) -> Result<usize, TaskError> {
        let mut tasks = self.tasks.lock().map_err(|_| TaskError::Poisoned)?;
        let previous = tasks.clone();
        let mut changed = 0usize;
        for task in tasks.values_mut() {
            if !task.state.terminal() {
                task.state = TaskState::Failed;
                task.output = Some(serde_json::json!({"error": reason}));
                task.updated_at_ms = updated_at_ms;
                changed = changed.saturating_add(1);
            }
        }
        if changed > 0 {
            if let Err(err) = self.persist(&tasks) {
                *tasks = previous;
                return Err(err);
            }
        }
        Ok(changed)
    }

    pub fn transition(
        &self,
        id: &str,
        next: TaskState,
        output: Option<Value>,
        updated_at_ms: u64,
    ) -> Result<FarmTask, TaskError> {
        self.transition_with_artifacts(id, next, output, None, updated_at_ms)
    }

    pub fn transition_with_artifacts(
        &self,
        id: &str,
        next: TaskState,
        output: Option<Value>,
        artifact_ids: Option<Vec<String>>,
        updated_at_ms: u64,
    ) -> Result<FarmTask, TaskError> {
        let mut tasks = self.tasks.lock().map_err(|_| TaskError::Poisoned)?;
        let task = tasks
            .get_mut(id)
            .ok_or_else(|| TaskError::NotFound(id.to_string()))?;
        if !transition_allowed(task.state, next) {
            return Err(TaskError::InvalidTransition {
                from: task.state,
                to: next,
            });
        }
        let previous = task.clone();
        task.state = next;
        task.output = output;
        if let Some(artifact_ids) = artifact_ids {
            task.artifact_ids = artifact_ids;
        }
        task.updated_at_ms = updated_at_ms;
        let updated = task.clone();
        if let Err(err) = self.persist(&tasks) {
            tasks.insert(id.to_string(), previous);
            return Err(err);
        }
        Ok(updated)
    }

    fn persist(&self, tasks: &BTreeMap<String, FarmTask>) -> Result<(), TaskError> {
        let Some(path) = self.snapshot_path.as_deref() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| TaskError::Io {
                path: parent.to_path_buf(),
                message: err.to_string(),
            })?;
        }
        let encoded = serde_json::to_vec_pretty(tasks).map_err(|err| TaskError::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, encoded).map_err(|err| TaskError::Io {
            path: temporary.clone(),
            message: err.to_string(),
        })?;
        std::fs::rename(&temporary, path).map_err(|err| TaskError::Io {
            path: path.to_path_buf(),
            message: err.to_string(),
        })
    }
}

fn validate_task(task: &FarmTask) -> Result<(), TaskError> {
    if task.id.trim().is_empty()
        || task.context_id.trim().is_empty()
        || task.requester.trim().is_empty()
        || task.assignee.trim().is_empty()
        || task.skill.trim().is_empty()
    {
        return Err(TaskError::Invalid(
            "task identity, participants, and skill must be non-empty".to_string(),
        ));
    }
    if task.updated_at_ms < task.created_at_ms {
        return Err(TaskError::Invalid(
            "task updated_at_ms cannot precede created_at_ms".to_string(),
        ));
    }
    Ok(())
}

fn transition_allowed(from: TaskState, to: TaskState) -> bool {
    use TaskState::*;
    matches!(
        (from, to),
        (Submitted, Working | Failed | Canceled | Rejected)
            | (Working, InputRequired | Completed | Failed | Canceled)
            | (InputRequired, Working | Canceled)
    )
}

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("task already exists: {0}")]
    AlreadyExists(String),
    #[error("task not found: {0}")]
    NotFound(String),
    #[error("parent task not found: {0}")]
    ParentNotFound(String),
    #[error("invalid task: {0}")]
    Invalid(String),
    #[error("invalid task transition from {from:?} to {to:?}")]
    InvalidTransition { from: TaskState, to: TaskState },
    #[error("task ledger lock poisoned")]
    Poisoned,
    #[error("task ledger I/O failed at {path}: {message}")]
    Io { path: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(id: &str) -> FarmTask {
        FarmTask {
            id: id.to_string(),
            context_id: "context-1".to_string(),
            parent_task_id: None,
            requester: "ceo".to_string(),
            assignee: "cto".to_string(),
            skill: "plan".to_string(),
            state: TaskState::Submitted,
            input: json!({"goal": "ship"}),
            output: None,
            artifact_ids: Vec::new(),
            delegation_depth: 0,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn enforces_task_state_machine() {
        let ledger = TaskLedger::default();
        ledger.insert(task("one")).unwrap();
        ledger
            .transition("one", TaskState::Working, None, 2)
            .unwrap();
        ledger
            .transition("one", TaskState::Completed, Some(json!({"ok": true})), 3)
            .unwrap();
        assert!(ledger
            .transition("one", TaskState::Working, None, 4)
            .is_err());
    }

    #[test]
    fn persists_tasks_across_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tasks.json");
        let ledger = TaskLedger::open(&path).unwrap();
        ledger.insert(task("persisted")).unwrap();
        let reopened = TaskLedger::open(path).unwrap();
        assert!(reopened.get("persisted").unwrap().is_some());
    }

    #[test]
    fn startup_recovery_fails_only_incomplete_tasks_and_persists() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tasks.json");
        let ledger = TaskLedger::open(&path).unwrap();
        ledger.insert(task("interrupted")).unwrap();
        ledger.insert(task("finished")).unwrap();
        ledger
            .transition("finished", TaskState::Working, None, 2)
            .unwrap();
        ledger
            .transition(
                "finished",
                TaskState::Completed,
                Some(json!({"ok": true})),
                3,
            )
            .unwrap();

        assert_eq!(
            ledger
                .fail_incomplete_on_startup("daemon restarted", 10)
                .unwrap(),
            1
        );
        let interrupted = TaskLedger::open(path)
            .unwrap()
            .get("interrupted")
            .unwrap()
            .unwrap();
        assert_eq!(interrupted.state, TaskState::Failed);
        assert_eq!(
            interrupted.output,
            Some(json!({"error": "daemon restarted"}))
        );
        assert_eq!(
            ledger.get("finished").unwrap().unwrap().state,
            TaskState::Completed
        );
    }
}
