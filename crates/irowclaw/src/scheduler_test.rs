use super::{initialize_scheduler_schema, run_job_by_id, scheduler_tick, SchedulerPaths};
use chrono::Timelike;
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
