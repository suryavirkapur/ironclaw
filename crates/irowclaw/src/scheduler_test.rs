use super::{initialize_scheduler_schema, scheduler_tick, SchedulerPaths};
use chrono::Timelike;
use rusqlite::Connection;

#[tokio::test]
async fn scheduler_runs_due_job_and_persists_state() {
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
    scheduler_tick(&paths).await.expect("tick");

    let conn = Connection::open(&db_path).expect("open db for verify");
    let (status, result_ref): (String, String) = conn
        .query_row(
            "select status, last_result_ref from scheduler_state where job_id = 'job1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("state row");
    assert_eq!(status, "success");
    assert!(!result_ref.is_empty());

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

#[tokio::test]
async fn scheduler_writes_scheduled_state_for_future_job() {
    let root = std::env::temp_dir().join(format!(
        "irowclaw-scheduler-future-test-{}",
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
    let jobs = "jobs = [{ id = 'job2', schedule = '0 0 1 1 *', task = 'echo never-now' }]\n";
    std::fs::write(&jobs_path, jobs).expect("write jobs");

    let paths = SchedulerPaths {
        jobs_path,
        logs_dir: logs_dir.clone(),
        db_path: db_path.clone(),
    };
    scheduler_tick(&paths).await.expect("tick");

    let conn = Connection::open(&db_path).expect("open db for verify");
    let status: String = conn
        .query_row(
            "select status from scheduler_state where job_id = 'job2'",
            [],
            |row| row.get(0),
        )
        .expect("state row");
    assert_eq!(status, "scheduled");

    let has_logs = std::fs::read_dir(&logs_dir)
        .ok()
        .and_then(|mut iter| iter.next())
        .is_some();
    assert!(!has_logs);
    let _ = std::fs::remove_dir_all(&root);
}
