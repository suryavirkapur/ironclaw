use super::{
    initialize_scheduler_schema, run_job, run_job_by_id, scheduler_tick, CronSchedule,
    SchedulerPaths,
};
use chrono::Timelike;
use common::config::JobDefinition;
use rusqlite::Connection;

#[tokio::test]
async fn scheduler_marks_due_job_triggered() {
    let root = std::env::temp_dir().join(format!("irowclaw-scheduler-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create test root");

    let cron_dir = root.join("cron");
    let logs_dir = root.join("logs");
    let db_path = root.join("db").join("ironclaw.db");
    std::fs::create_dir_all(&cron_dir).expect("create cron dir");
    std::fs::create_dir_all(root.join("db")).expect("create db dir");

    let jobs_path = cron_dir.join("jobs.toml");
    let now = chrono::Utc::now();
    let jobs = format!(
        "jobs = [{{ id = 'job1', schedule = '{} {} * * *', task = 'echo hi' }}]\n",
        now.minute(),
        now.hour()
    );
    std::fs::write(&jobs_path, jobs).expect("write jobs");

    let conn = Connection::open(&db_path).expect("open db");
    initialize_scheduler_schema(&conn).expect("init scheduler schema");
    drop(conn);

    let paths = SchedulerPaths {
        jobs_path,
        logs_dir: logs_dir.clone(),
        db_path: db_path.clone(),
    };
    let triggered = scheduler_tick(&paths).await.expect("tick");
    assert_eq!(triggered, vec!["job1".to_string()]);

    let conn = Connection::open(&db_path).expect("open db for verify");
    let status: String = conn
        .query_row(
            "select status from scheduler_state where job_id = 'job1'",
            [],
            |row| row.get(0),
        )
        .expect("state row");
    assert_eq!(status, "triggered");

    let has_logs = std::fs::read_dir(&logs_dir)
        .ok()
        .and_then(|mut iter| iter.next())
        .is_some();
    assert!(!has_logs);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn scheduled_job_runs_only_after_host_request() {
    let root = std::env::temp_dir().join(format!(
        "irowclaw-scheduler-run-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create test root");

    let cron_dir = root.join("cron");
    let logs_dir = root.join("logs");
    let db_path = root.join("db").join("ironclaw.db");
    std::fs::create_dir_all(&cron_dir).expect("create cron dir");
    std::fs::create_dir_all(root.join("db")).expect("create db dir");

    let jobs_path = cron_dir.join("jobs.toml");
    let now = chrono::Utc::now();
    let jobs = format!(
        "jobs = [{{ id = 'job2', schedule = '{} {} * * *', task = 'echo host-run' }}]\n",
        now.minute(),
        now.hour()
    );
    std::fs::write(&jobs_path, jobs).expect("write jobs");

    let paths = SchedulerPaths {
        jobs_path,
        logs_dir: logs_dir.clone(),
        db_path: db_path.clone(),
    };
    let triggered = scheduler_tick(&paths).await.expect("tick");
    assert_eq!(triggered, vec!["job2".to_string()]);

    let outcome = run_job_by_id(&paths, "job2")
        .await
        .expect("run scheduled job");
    assert!(outcome.ok);
    assert!(!outcome.log_ref.is_empty());

    let conn = Connection::open(&db_path).expect("open db for verify");
    let status: String = conn
        .query_row(
            "select status from scheduler_state where job_id = 'job2'",
            [],
            |row| row.get(0),
        )
        .expect("state row");
    assert_eq!(status, "success");

    let files = std::fs::read_dir(&logs_dir).expect("list logs");
    let mut count = 0usize;
    for file in files {
        if file.is_ok() {
            count = count.saturating_add(1);
        }
    }
    assert_eq!(count, 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cron_parser_rejects_invalid_field_count() {
    let parsed = CronSchedule::parse("* * * *");
    assert!(parsed.is_err());
}

#[test]
fn cron_parser_rejects_out_of_range_values() {
    let parsed = CronSchedule::parse("70 * * * *");
    assert!(parsed.is_err());
}

#[tokio::test]
async fn run_job_records_failed_status_when_command_fails() {
    let root = std::env::temp_dir().join(format!(
        "irowclaw-scheduler-fail-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    if let Err(err) = std::fs::create_dir_all(&root) {
        panic!("create test root failed: {err}");
    }

    let cron_dir = root.join("cron");
    let logs_dir = root.join("logs");
    let db_path = root.join("db").join("ironclaw.db");
    if let Err(err) = std::fs::create_dir_all(&cron_dir) {
        panic!("create cron dir failed: {err}");
    }
    if let Err(err) = std::fs::create_dir_all(root.join("db")) {
        panic!("create db dir failed: {err}");
    }
    let jobs_path = cron_dir.join("jobs.toml");
    if let Err(err) = std::fs::write(&jobs_path, "jobs = []\n") {
        panic!("write jobs file failed: {err}");
    }

    let paths = SchedulerPaths {
        jobs_path,
        logs_dir: logs_dir.clone(),
        db_path: db_path.clone(),
    };
    let job = JobDefinition {
        id: "job-fail".to_string(),
        schedule: "* * * * *".to_string(),
        description: None,
        task: "exit 2".to_string(),
    };
    let outcome = match run_job(&paths, &job).await {
        Ok(value) => value,
        Err(err) => panic!("run job failed: {err}"),
    };
    assert!(!outcome.ok);
    assert!(!outcome.log_ref.is_empty());

    let conn = match Connection::open(&db_path) {
        Ok(value) => value,
        Err(err) => panic!("open db for verify failed: {err}"),
    };
    let status: String = match conn.query_row(
        "select status from scheduler_state where job_id = 'job-fail'",
        [],
        |row| row.get(0),
    ) {
        Ok(value) => value,
        Err(err) => panic!("state row query failed: {err}"),
    };
    assert_eq!(status, "failed");
    let _ = std::fs::remove_dir_all(&root);
}
