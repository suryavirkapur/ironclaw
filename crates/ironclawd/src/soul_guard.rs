use crate::IronclawError;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

const SOUL_PATH: &str = "/mnt/brain/soul.md";
const MAX_BYTES_THRESHOLD: usize = 1024;
const MAX_RATIO_THRESHOLD: f64 = 0.20;

#[derive(Clone, Debug)]
struct SoulSnapshot {
    hash: String,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PendingSoulApproval {
    pub id: i64,
    pub user_id: String,
    pub path: String,
    pub before_hash: String,
    pub after_hash: String,
    pub byte_change: i64,
    pub change_ratio: f64,
    pub created_at_ms: i64,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SoulDecision {
    Approve,
    Reject,
}

impl SoulDecision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approve => "approved",
            Self::Reject => "rejected",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DiffSummary {
    pub byte_change: usize,
    pub change_ratio: f64,
    pub suspicious: bool,
}

pub fn soul_guard_db_path(users_root: &Path) -> PathBuf {
    users_root.join("soul_guard.db")
}

pub fn initialize_soul_guard_schema(conn: &Connection) -> Result<(), IronclawError> {
    conn.execute_batch(
        "create table if not exists soul_change_log (
            id integer primary key autoincrement,
            detected_at_ms integer not null,
            user_id text not null,
            path text not null,
            before_hash text not null,
            after_hash text not null,
            byte_change integer not null,
            change_ratio real not null,
            suspicious integer not null,
            approval_status text not null
        );
        create table if not exists soul_change_approvals (
            id integer primary key autoincrement,
            created_at_ms integer not null,
            user_id text not null,
            path text not null,
            before_hash text not null,
            after_hash text not null,
            byte_change integer not null,
            change_ratio real not null,
            status text not null,
            decided_at_ms integer,
            decision_note text
        );",
    )
    .map_err(|err| IronclawError::new(format!("soul guard schema failed: {err}")))?;
    Ok(())
}

fn open_soul_guard_db(db_path: &Path) -> Result<Connection, IronclawError> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| IronclawError::new(format!("soul guard dir create failed: {err}")))?;
    }
    let conn = Connection::open(db_path)
        .map_err(|err| IronclawError::new(format!("soul guard db open failed: {err}")))?;
    initialize_soul_guard_schema(&conn)?;
    Ok(conn)
}

pub async fn run_monitor(
    users_root: PathBuf,
    db_path: PathBuf,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), IronclawError> {
    let mut known = HashMap::<PathBuf, SoulSnapshot>::new();
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                monitor_once(&users_root, &db_path, &mut known)?;
            }
            changed = shutdown.changed() => {
                let _ = changed;
                if *shutdown.borrow() {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn monitor_once(
    users_root: &Path,
    db_path: &Path,
    known: &mut HashMap<PathBuf, SoulSnapshot>,
) -> Result<(), IronclawError> {
    let conn = open_soul_guard_db(db_path)?;
    let paths = collect_soul_paths(users_root)?;
    for path in paths {
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(&path)
            .map_err(|err| IronclawError::new(format!("soul read failed: {err}")))?;
        let hash = sha256_hex(&bytes);
        if let Some(snapshot) = known.get(&path) {
            if snapshot.hash == hash {
                continue;
            }

            let diff = summarize_diff(&snapshot.bytes, &bytes);
            let user_id = path_user_id(users_root, &path).unwrap_or_else(|| "global".to_string());
            let approval_status = if diff.suspicious {
                "pending"
            } else {
                "not_required"
            };
            let detected_at_ms = now_ms_i64()?;
            conn.execute(
                "insert into soul_change_log (
                    detected_at_ms, user_id, path, before_hash, after_hash, byte_change,
                    change_ratio, suspicious, approval_status
                ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    detected_at_ms,
                    user_id,
                    path.to_string_lossy().to_string(),
                    snapshot.hash,
                    hash,
                    diff.byte_change as i64,
                    diff.change_ratio,
                    if diff.suspicious { 1i64 } else { 0i64 },
                    approval_status
                ],
            )
            .map_err(|err| IronclawError::new(format!("soul log insert failed: {err}")))?;

            tracing::warn!(
                "soul guard change path={} before_hash={} after_hash={} suspicious={}",
                path.display(),
                snapshot.hash,
                hash,
                diff.suspicious
            );

            if diff.suspicious {
                queue_pending_approval(
                    &conn,
                    detected_at_ms,
                    &user_id,
                    &path.to_string_lossy(),
                    &snapshot.hash,
                    &hash,
                    diff.byte_change as i64,
                    diff.change_ratio,
                )?;
            }
        }

        known.insert(path, SoulSnapshot { hash, bytes });
    }
    Ok(())
}

fn queue_pending_approval(
    conn: &Connection,
    created_at_ms: i64,
    user_id: &str,
    path: &str,
    before_hash: &str,
    after_hash: &str,
    byte_change: i64,
    change_ratio: f64,
) -> Result<(), IronclawError> {
    let existing: i64 = conn
        .query_row(
            "select count(1)
             from soul_change_approvals
             where path = ?1 and after_hash = ?2 and status = 'pending'",
            params![path, after_hash],
            |row| row.get(0),
        )
        .map_err(|err| IronclawError::new(format!("soul queue check failed: {err}")))?;
    if existing > 0 {
        return Ok(());
    }

    conn.execute(
        "insert into soul_change_approvals (
            created_at_ms, user_id, path, before_hash, after_hash, byte_change, change_ratio,
            status
        ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending')",
        params![
            created_at_ms,
            user_id,
            path,
            before_hash,
            after_hash,
            byte_change,
            change_ratio
        ],
    )
    .map_err(|err| IronclawError::new(format!("soul queue insert failed: {err}")))?;
    Ok(())
}

pub fn list_pending_approvals(db_path: &Path) -> Result<Vec<PendingSoulApproval>, IronclawError> {
    let conn = open_soul_guard_db(db_path)?;
    let mut stmt = conn
        .prepare(
            "select id, user_id, path, before_hash, after_hash, byte_change, change_ratio,
                    created_at_ms, status
             from soul_change_approvals
             where status = 'pending'
             order by id asc",
        )
        .map_err(|err| IronclawError::new(format!("soul queue query prepare failed: {err}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(PendingSoulApproval {
                id: row.get(0)?,
                user_id: row.get(1)?,
                path: row.get(2)?,
                before_hash: row.get(3)?,
                after_hash: row.get(4)?,
                byte_change: row.get(5)?,
                change_ratio: row.get(6)?,
                created_at_ms: row.get(7)?,
                status: row.get(8)?,
            })
        })
        .map_err(|err| IronclawError::new(format!("soul queue map failed: {err}")))?;

    let mut output = Vec::new();
    for row in rows {
        let value =
            row.map_err(|err| IronclawError::new(format!("soul queue row failed: {err}")))?;
        output.push(value);
    }
    Ok(output)
}

pub fn decide_approval(
    db_path: &Path,
    approval_id: i64,
    decision: SoulDecision,
    note: Option<String>,
) -> Result<bool, IronclawError> {
    let conn = open_soul_guard_db(db_path)?;
    let updated = conn
        .execute(
            "update soul_change_approvals
             set status = ?1, decided_at_ms = ?2, decision_note = ?3
             where id = ?4 and status = 'pending'",
            params![
                decision.as_str(),
                now_ms_i64()?,
                note.unwrap_or_default(),
                approval_id
            ],
        )
        .map_err(|err| IronclawError::new(format!("soul decision update failed: {err}")))?;
    Ok(updated > 0)
}

pub fn has_pending_approval_for_user(db_path: &Path, user_id: &str) -> Result<bool, IronclawError> {
    let conn = open_soul_guard_db(db_path)?;
    let count: i64 = conn
        .query_row(
            "select count(1)
             from soul_change_approvals
             where status = 'pending'
               and (user_id = ?1 or user_id = 'global')",
            params![user_id],
            |row| row.get(0),
        )
        .map_err(|err| IronclawError::new(format!("soul pending query failed: {err}")))?;
    Ok(count > 0)
}

pub fn summarize_diff(before: &[u8], after: &[u8]) -> DiffSummary {
    let shared = before.len().min(after.len());
    let mut mismatches = 0usize;
    for idx in 0..shared {
        if before[idx] != after[idx] {
            mismatches = mismatches.saturating_add(1);
        }
    }
    let size_delta = before.len().max(after.len()) - shared;
    let byte_change = mismatches.saturating_add(size_delta);
    let baseline = before.len().max(1) as f64;
    let change_ratio = (byte_change as f64) / baseline;
    let suspicious = byte_change > MAX_BYTES_THRESHOLD || change_ratio > MAX_RATIO_THRESHOLD;
    DiffSummary {
        byte_change,
        change_ratio,
        suspicious,
    }
}

fn collect_soul_paths(users_root: &Path) -> Result<Vec<PathBuf>, IronclawError> {
    let mut paths = vec![PathBuf::from(SOUL_PATH)];
    if !users_root.exists() {
        return Ok(paths);
    }

    let entries = std::fs::read_dir(users_root)
        .map_err(|err| IronclawError::new(format!("soul read users root failed: {err}")))?;
    for entry in entries {
        let entry =
            entry.map_err(|err| IronclawError::new(format!("soul read entry failed: {err}")))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        paths.push(path.join("guest").join("soul.md"));
    }
    Ok(paths)
}

fn path_user_id(users_root: &Path, soul_path: &Path) -> Option<String> {
    let relative = soul_path.strip_prefix(users_root).ok()?;
    let mut components = relative.components();
    let first = components.next()?;
    let value = first.as_os_str().to_str()?;
    Some(value.to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let hash = hasher.finalize();
    format!("{hash:x}")
}

fn now_ms_i64() -> Result<i64, IronclawError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| IronclawError::new(format!("time error: {err}")))?;
    Ok(now.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::{
        decide_approval, has_pending_approval_for_user, initialize_soul_guard_schema,
        list_pending_approvals, open_soul_guard_db, summarize_diff, SoulDecision,
    };
    use rusqlite::params;

    #[test]
    fn suspicious_diff_detected_by_ratio() {
        let before = b"hello";
        let after = b"zzzzz";
        let diff = summarize_diff(before, after);
        assert!(diff.suspicious);
        assert!(diff.change_ratio > 0.2);
    }

    #[test]
    fn suspicious_diff_detected_by_size() {
        let before = vec![b'a'; 10];
        let after = vec![b'b'; 2200];
        let diff = summarize_diff(&before, &after);
        assert!(diff.suspicious);
        assert!(diff.byte_change > 1024);
    }

    #[test]
    fn approval_queue_roundtrip() {
        let db_path = std::env::temp_dir().join("ironclaw-soul-guard-test.db");
        let _ = std::fs::remove_file(&db_path);
        let conn = open_soul_guard_db(&db_path).expect("open db");
        initialize_soul_guard_schema(&conn).expect("schema");
        conn.execute(
            "insert into soul_change_approvals (
                created_at_ms, user_id, path, before_hash, after_hash, byte_change, change_ratio,
                status
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending')",
            params![1i64, "owner", "/tmp/soul.md", "a", "b", 2400i64, 0.5f64],
        )
        .expect("insert");
        drop(conn);

        let pending = list_pending_approvals(&db_path).expect("list");
        assert_eq!(pending.len(), 1);
        assert!(has_pending_approval_for_user(&db_path, "owner").expect("pending for user"));
        assert!(decide_approval(
            &db_path,
            pending[0].id,
            SoulDecision::Approve,
            Some("approved for test".to_string())
        )
        .expect("decide"));
        assert!(!has_pending_approval_for_user(&db_path, "owner").expect("pending after decision"));
        let _ = std::fs::remove_file(&db_path);
    }
}
