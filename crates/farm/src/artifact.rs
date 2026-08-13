use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub id: String,
    pub task_id: String,
    pub producer_agent: String,
    pub filename: String,
    pub mime_type: String,
    pub caption: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub created_at_ms: u64,
}

#[derive(Clone)]
pub struct ArtifactStore {
    root: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl ArtifactStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ArtifactError> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|err| ArtifactError::Io {
            path: root.clone(),
            message: err.to_string(),
        })?;
        Ok(Self {
            root: Arc::new(root),
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn put(
        &self,
        task_id: &str,
        producer_agent: &str,
        filename: &str,
        mime_type: &str,
        caption: &str,
        data: &[u8],
        created_at_ms: u64,
    ) -> Result<ArtifactRecord, ArtifactError> {
        if data.is_empty() {
            return Err(ArtifactError::Invalid("artifact data is empty".into()));
        }
        if data.len() > MAX_ARTIFACT_BYTES {
            return Err(ArtifactError::Invalid(format!(
                "artifact exceeds {MAX_ARTIFACT_BYTES} byte limit"
            )));
        }
        let _guard = self
            .lock
            .lock()
            .map_err(|_| ArtifactError::Invalid("artifact store lock poisoned".into()))?;
        let filename = safe_filename(filename)?;
        let sha256 = hex::encode(Sha256::digest(data));
        let id = sha256.clone();
        let record = ArtifactRecord {
            id: id.clone(),
            task_id: task_id.to_string(),
            producer_agent: producer_agent.to_string(),
            filename,
            mime_type: mime_type.to_string(),
            caption: caption.to_string(),
            size_bytes: data.len() as u64,
            sha256,
            created_at_ms,
        };
        let data_path = self.data_path(&id);
        if data_path.exists() {
            verify_data(&data_path, &id)?;
        } else {
            write_atomic(&data_path, data)?;
        }
        let mut records = self.read_records(&id).unwrap_or_default();
        records.retain(|existing| {
            existing.task_id != record.task_id || existing.producer_agent != record.producer_agent
        });
        records.push(record.clone());
        let metadata = serde_json::to_vec_pretty(&records)
            .map_err(|err| ArtifactError::Invalid(err.to_string()))?;
        write_atomic(&self.metadata_path(&id), &metadata)?;
        Ok(record)
    }

    pub fn get(&self, id: &str) -> Result<(ArtifactRecord, Vec<u8>), ArtifactError> {
        validate_id(id)?;
        let _guard = self
            .lock
            .lock()
            .map_err(|_| ArtifactError::Invalid("artifact store lock poisoned".into()))?;
        let record = self
            .read_records(id)?
            .pop()
            .ok_or_else(|| ArtifactError::Invalid("artifact metadata is empty".into()))?;
        let data_path = self.data_path(id);
        let data = std::fs::read(&data_path).map_err(|err| ArtifactError::Io {
            path: data_path.clone(),
            message: err.to_string(),
        })?;
        let digest = hex::encode(Sha256::digest(&data));
        if digest != record.sha256 || digest != id {
            return Err(ArtifactError::Invalid(
                "artifact content hash does not match its id".into(),
            ));
        }
        Ok((record, data))
    }

    pub fn get_records(&self, id: &str) -> Result<(Vec<ArtifactRecord>, Vec<u8>), ArtifactError> {
        validate_id(id)?;
        let _guard = self
            .lock
            .lock()
            .map_err(|_| ArtifactError::Invalid("artifact store lock poisoned".into()))?;
        let records = self.read_records(id)?;
        let data_path = self.data_path(id);
        let data = std::fs::read(&data_path).map_err(|err| ArtifactError::Io {
            path: data_path.clone(),
            message: err.to_string(),
        })?;
        verify_bytes(&data, id)?;
        Ok((records, data))
    }

    fn read_records(&self, id: &str) -> Result<Vec<ArtifactRecord>, ArtifactError> {
        let metadata_path = self.metadata_path(id);
        let metadata = std::fs::read(&metadata_path).map_err(|err| ArtifactError::Io {
            path: metadata_path.clone(),
            message: err.to_string(),
        })?;
        serde_json::from_slice::<Vec<ArtifactRecord>>(&metadata)
            .or_else(|_| {
                serde_json::from_slice::<ArtifactRecord>(&metadata).map(|record| vec![record])
            })
            .map_err(|err| ArtifactError::Invalid(format!("invalid artifact metadata: {err}")))
    }

    fn data_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.blob"))
    }

    fn metadata_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }
}

fn verify_data(path: &Path, id: &str) -> Result<(), ArtifactError> {
    let data = std::fs::read(path).map_err(|err| ArtifactError::Io {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    verify_bytes(&data, id)
}

fn verify_bytes(data: &[u8], id: &str) -> Result<(), ArtifactError> {
    let digest = hex::encode(Sha256::digest(data));
    if digest == id {
        Ok(())
    } else {
        Err(ArtifactError::Invalid(
            "artifact content hash does not match its id".into(),
        ))
    }
}

fn safe_filename(filename: &str) -> Result<String, ArtifactError> {
    let basename = Path::new(filename)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty() && *value != "." && *value != "..")
        .ok_or_else(|| ArtifactError::Invalid("artifact filename is invalid".into()))?;
    if basename != filename {
        return Err(ArtifactError::Invalid(
            "artifact filename must be a basename".into(),
        ));
    }
    if basename
        .chars()
        .any(|character| character.is_control() || matches!(character, '"' | '\\'))
    {
        return Err(ArtifactError::Invalid(
            "artifact filename contains unsupported header characters".into(),
        ));
    }
    Ok(basename.to_string())
}

fn validate_id(id: &str) -> Result<(), ArtifactError> {
    if id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(ArtifactError::Invalid("artifact id is invalid".into()))
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), ArtifactError> {
    let temp = path.with_extension(format!("{}.tmp", rand_suffix()));
    std::fs::write(&temp, data).map_err(|err| ArtifactError::Io {
        path: temp.clone(),
        message: err.to_string(),
    })?;
    std::fs::rename(&temp, path).map_err(|err| ArtifactError::Io {
        path: path.to_path_buf(),
        message: err.to_string(),
    })
}

fn rand_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().to_string())
        .unwrap_or_else(|_| "0".into())
}

#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("invalid artifact: {0}")]
    Invalid(String),
    #[error("artifact I/O failed at {path}: {message}")]
    Io { path: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_addressed_artifact_roundtrip_survives_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temp.path()).unwrap();
        let record = store
            .put(
                "task-1",
                "backend-engineer",
                "service.zip",
                "application/zip",
                "tested build",
                b"zip bytes",
                42,
            )
            .unwrap();
        assert_eq!(record.id.len(), 64);
        let reopened = ArtifactStore::open(temp.path()).unwrap();
        let (loaded, data) = reopened.get(&record.id).unwrap();
        assert_eq!(loaded, record);
        assert_eq!(data, b"zip bytes");
    }

    #[test]
    fn rejects_empty_data_and_unsafe_names() {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temp.path()).unwrap();
        assert!(store
            .put("task", "agent", "empty.txt", "text/plain", "", b"", 1)
            .is_err());
        assert!(store
            .put("task", "agent", "../escape.txt", "text/plain", "", b"x", 1)
            .is_err());
    }

    #[test]
    fn same_bytes_keep_provenance_for_multiple_tasks() {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temp.path()).unwrap();
        let first = store
            .put("task-1", "nora", "app.py", "text/plain", "", b"same", 1)
            .unwrap();
        store
            .put("task-2", "ravi", "app.py", "text/plain", "", b"same", 2)
            .unwrap();
        let (records, data) = store.get_records(&first.id).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].task_id, "task-1");
        assert_eq!(records[1].task_id, "task-2");
        assert_eq!(data, b"same");
    }
}
