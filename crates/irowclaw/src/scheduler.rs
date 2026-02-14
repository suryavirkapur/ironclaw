use crate::runtime::IrowclawError;
use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike, Utc};
use common::config::{JobDefinition, JobsConfig};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;

const LOOKBACK_MS: i64 = 60_000;

#[derive(Clone, Debug)]
pub struct SchedulerPaths {
    pub jobs_path: PathBuf,
    pub logs_dir: PathBuf,
    pub db_path: PathBuf,
}

pub fn spawn_scheduler(paths: SchedulerPaths) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(err) = scheduler_tick(&paths).await {
                eprintln!("irowclaw scheduler tick failed: {err}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    })
}

pub async fn scheduler_tick(paths: &SchedulerPaths) -> Result<(), IrowclawError> {
    let jobs = load_jobs(&paths.jobs_path)?;
    let conn = open_scheduler_db(&paths.db_path)?;
    initialize_scheduler_schema(&conn)?;
    drop(conn);

    for job in jobs.jobs {
        process_job(&paths.db_path, &job, &paths.logs_dir).await?;
    }
    Ok(())
}

fn load_jobs(path: &Path) -> Result<JobsConfig, IrowclawError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|err| IrowclawError::new(format!("jobs read failed: {err}")))?;
    toml::from_str(&contents).map_err(|err| IrowclawError::new(format!("jobs parse failed: {err}")))
}

fn open_scheduler_db(path: &Path) -> Result<Connection, IrowclawError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| IrowclawError::new(format!("scheduler db dir create failed: {err}")))?;
    }
    Connection::open(path)
        .map_err(|err| IrowclawError::new(format!("scheduler db open failed: {err}")))
}

pub fn initialize_scheduler_schema(conn: &Connection) -> Result<(), IrowclawError> {
    conn.execute_batch(
        "create table if not exists scheduler_state (
            job_id text primary key,
            schedule text not null,
            task text not null,
            last_run_ms integer,
            next_run_ms integer,
            status text not null,
            last_result_ref text,
            updated_at_ms integer not null
        );",
    )
    .map_err(|err| IrowclawError::new(format!("scheduler schema failed: {err}")))?;
    Ok(())
}

#[derive(Debug)]
struct SchedulerStateRow {
    last_run_ms: Option<i64>,
}

async fn process_job(
    db_path: &Path,
    job: &JobDefinition,
    logs_dir: &Path,
) -> Result<(), IrowclawError> {
    let schedule = CronSchedule::parse(&job.schedule)?;
    let now_ms = now_ms_i64()?;
    let conn = open_scheduler_db(db_path)?;
    initialize_scheduler_schema(&conn)?;
    let state = load_state(&conn, &job.id)?;
    let anchor = state
        .last_run_ms
        .unwrap_or(now_ms.saturating_sub(LOOKBACK_MS));
    let due_ms = next_occurrence_ms(&schedule, anchor.saturating_sub(1))?;

    if now_ms < due_ms {
        upsert_state(
            &conn,
            job,
            state.last_run_ms,
            Some(due_ms),
            "scheduled",
            None,
            now_ms,
        )?;
        return Ok(());
    }

    upsert_state(
        &conn,
        job,
        state.last_run_ms,
        Some(due_ms),
        "running",
        None,
        now_ms,
    )?;
    drop(conn);

    let outcome = execute_job(job, logs_dir).await?;
    let next_run_ms = next_occurrence_ms(&schedule, now_ms)?;
    let status = if outcome.ok { "success" } else { "failed" };
    let conn = open_scheduler_db(db_path)?;
    initialize_scheduler_schema(&conn)?;
    upsert_state(
        &conn,
        job,
        Some(now_ms),
        Some(next_run_ms),
        status,
        Some(outcome.log_ref),
        now_ms,
    )?;
    Ok(())
}

async fn execute_job(job: &JobDefinition, logs_dir: &Path) -> Result<JobOutcome, IrowclawError> {
    let command_output = Command::new("sh")
        .arg("-lc")
        .arg(job.task.clone())
        .output()
        .await
        .map_err(|err| IrowclawError::new(format!("job execute failed: {err}")))?;
    let stdout = String::from_utf8_lossy(&command_output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&command_output.stderr).to_string();
    let exit_code = command_output.status.code().unwrap_or(-1);
    let ok = command_output.status.success();
    let log_ref = append_job_result_log(logs_dir, job, exit_code, ok, &stdout, &stderr)?;
    Ok(JobOutcome { ok, log_ref })
}

struct JobOutcome {
    ok: bool,
    log_ref: String,
}

fn append_job_result_log(
    logs_dir: &Path,
    job: &JobDefinition,
    exit_code: i32,
    ok: bool,
    stdout: &str,
    stderr: &str,
) -> Result<String, IrowclawError> {
    std::fs::create_dir_all(logs_dir)
        .map_err(|err| IrowclawError::new(format!("create logs dir failed: {err}")))?;
    let now = Utc::now();
    let date = now.format("%Y-%m-%d").to_string();
    let log_path = logs_dir.join(format!("{date}.md"));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|err| IrowclawError::new(format!("open log file failed: {err}")))?;

    let timestamp = now.to_rfc3339();
    let mut entry = String::new();
    entry.push_str(&format!("## job {}\n", job.id));
    entry.push_str(&format!("- time: {timestamp}\n"));
    entry.push_str(&format!(
        "- status: {}\n",
        if ok { "success" } else { "failed" }
    ));
    entry.push_str(&format!("- exit_code: {exit_code}\n"));
    entry.push_str("- stdout:\n```\n");
    entry.push_str(stdout);
    if !stdout.ends_with('\n') {
        entry.push('\n');
    }
    entry.push_str("```\n");
    entry.push_str("- stderr:\n```\n");
    entry.push_str(stderr);
    if !stderr.ends_with('\n') {
        entry.push('\n');
    }
    entry.push_str("```\n\n");

    use std::io::Write;
    file.write_all(entry.as_bytes())
        .map_err(|err| IrowclawError::new(format!("write log file failed: {err}")))?;
    Ok(log_path.to_string_lossy().to_string())
}

fn load_state(conn: &Connection, job_id: &str) -> Result<SchedulerStateRow, IrowclawError> {
    let mut stmt = conn
        .prepare("select last_run_ms from scheduler_state where job_id = ?1")
        .map_err(|err| IrowclawError::new(format!("scheduler state prepare failed: {err}")))?;
    let mut rows = stmt
        .query(params![job_id])
        .map_err(|err| IrowclawError::new(format!("scheduler state query failed: {err}")))?;
    if let Some(row) = rows
        .next()
        .map_err(|err| IrowclawError::new(format!("scheduler state row failed: {err}")))?
    {
        Ok(SchedulerStateRow {
            last_run_ms: row.get(0).map_err(|err| {
                IrowclawError::new(format!("scheduler state parse failed: {err}"))
            })?,
        })
    } else {
        Ok(SchedulerStateRow { last_run_ms: None })
    }
}

fn upsert_state(
    conn: &Connection,
    job: &JobDefinition,
    last_run_ms: Option<i64>,
    next_run_ms: Option<i64>,
    status: &str,
    last_result_ref: Option<String>,
    updated_at_ms: i64,
) -> Result<(), IrowclawError> {
    conn.execute(
        "insert into scheduler_state (
            job_id, schedule, task, last_run_ms, next_run_ms, status, last_result_ref, updated_at_ms
        ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        on conflict(job_id) do update set
            schedule = excluded.schedule,
            task = excluded.task,
            last_run_ms = excluded.last_run_ms,
            next_run_ms = excluded.next_run_ms,
            status = excluded.status,
            last_result_ref = excluded.last_result_ref,
            updated_at_ms = excluded.updated_at_ms",
        params![
            job.id,
            job.schedule,
            job.task,
            last_run_ms,
            next_run_ms,
            status,
            last_result_ref.unwrap_or_default(),
            updated_at_ms
        ],
    )
    .map_err(|err| IrowclawError::new(format!("scheduler state upsert failed: {err}")))?;
    Ok(())
}

fn ms_to_datetime(ms: i64) -> Result<DateTime<Utc>, IrowclawError> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .ok_or_else(|| IrowclawError::new("invalid timestamp"))
}

fn now_ms_i64() -> Result<i64, IrowclawError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| IrowclawError::new(format!("time error: {err}")))?;
    Ok(duration.as_millis() as i64)
}

fn next_occurrence_ms(schedule: &CronSchedule, after_ms: i64) -> Result<i64, IrowclawError> {
    let mut cursor = ms_to_datetime(after_ms)?
        .with_second(0)
        .and_then(|value| value.with_nanosecond(0))
        .ok_or_else(|| IrowclawError::new("invalid datetime normalization"))?
        + Duration::minutes(1);
    let max_iterations = 525_600 * 5;
    let mut iterations = 0usize;
    while iterations < max_iterations {
        if schedule.matches(&cursor) {
            return Ok(cursor.timestamp_millis());
        }
        cursor += Duration::minutes(1);
        iterations = iterations.saturating_add(1);
    }
    Err(IrowclawError::new("cron schedule has no next occurrence"))
}

#[derive(Clone, Debug)]
struct CronSchedule {
    minute: CronField,
    hour: CronField,
    day_of_month: CronField,
    month: CronField,
    day_of_week: CronField,
}

impl CronSchedule {
    fn parse(expression: &str) -> Result<Self, IrowclawError> {
        let fields = expression
            .split_whitespace()
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(IrowclawError::new(
                "invalid cron schedule: expected 5 fields",
            ));
        }
        Ok(Self {
            minute: CronField::parse(fields[0], 0, 59)?,
            hour: CronField::parse(fields[1], 0, 23)?,
            day_of_month: CronField::parse(fields[2], 1, 31)?,
            month: CronField::parse(fields[3], 1, 12)?,
            day_of_week: CronField::parse(fields[4], 0, 6)?,
        })
    }

    fn matches(&self, time: &DateTime<Utc>) -> bool {
        self.minute.matches(time.minute())
            && self.hour.matches(time.hour())
            && self.day_of_month.matches(time.day())
            && self.month.matches(time.month())
            && self
                .day_of_week
                .matches(time.weekday().num_days_from_sunday())
    }
}

#[derive(Clone, Debug)]
enum CronField {
    Any,
    Exact(u32),
}

impl CronField {
    fn parse(field: &str, min: u32, max: u32) -> Result<Self, IrowclawError> {
        if field == "*" {
            return Ok(Self::Any);
        }
        let value = field
            .parse::<u32>()
            .map_err(|err| IrowclawError::new(format!("invalid cron field: {err}")))?;
        if value < min || value > max {
            return Err(IrowclawError::new(format!(
                "invalid cron field value: {value} outside {min}-{max}"
            )));
        }
        Ok(Self::Exact(value))
    }

    fn matches(&self, value: u32) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(expected) => *expected == value,
        }
    }
}

#[cfg(test)]
#[path = "scheduler_test.rs"]
mod scheduler_test;
