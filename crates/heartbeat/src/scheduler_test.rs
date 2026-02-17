use super::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

#[tokio::test]
async fn heartbeat_fires_after_interval() {
    let counter = Arc::new(AtomicU32::new(0));
    let counter_clone = counter.clone();
    let callback: HeartbeatCallback = Arc::new(move || {
        let c = counter_clone.clone();
        Box::pin(async move {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    });
    let mut scheduler = HeartbeatScheduler::new(HeartbeatConfig {
        interval: Duration::from_millis(50),
        label: "test".into(),
    });
    scheduler.start(callback).expect("start should succeed");
    tokio::time::sleep(Duration::from_millis(180)).await;
    scheduler.stop();
    let count = counter.load(Ordering::SeqCst);
    assert!(count >= 2, "expected at least 2 ticks, got {count}");
}

#[tokio::test]
async fn double_start_fails() {
    let callback: HeartbeatCallback = Arc::new(|| Box::pin(async { Ok(()) }));
    let mut scheduler = HeartbeatScheduler::new(HeartbeatConfig::default());
    scheduler.start(callback.clone()).expect("first start ok");
    let result = scheduler.start(callback);
    assert!(result.is_err());
    scheduler.stop();
}
