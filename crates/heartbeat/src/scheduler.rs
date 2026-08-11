use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::HeartbeatError;

/// heartbeat scheduler configuration
#[derive(Clone, Debug)]
pub struct HeartbeatConfig {
    /// interval between heartbeat ticks
    pub interval: Duration,
    /// human-readable label for logging
    pub label: String,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30 * 60),
            label: "default".into(),
        }
    }
}

/// async callback invoked on each heartbeat tick
pub type HeartbeatCallback =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>>
    + Send>> + Send + Sync>;

/// tokio-based periodic heartbeat scheduler
pub struct HeartbeatScheduler {
    config: HeartbeatConfig,
    shutdown_tx: Option<watch::Sender<bool>>,
}

impl HeartbeatScheduler {
    pub fn new(config: HeartbeatConfig) -> Self {
        Self {
            config,
            shutdown_tx: None,
        }
    }

    /// start the heartbeat loop in a background tokio task
    pub fn start(&mut self, callback: HeartbeatCallback) -> Result<(), HeartbeatError> {
        if self.shutdown_tx.is_some() {
            return Err(HeartbeatError::StartFailed(
                "scheduler already running".into(),
            ));
        }
        let (tx, mut rx) = watch::channel(false);
        self.shutdown_tx = Some(tx);
        let config = self.config.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.interval);
            // skip the immediate first tick
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        tracing::info!(
                            "heartbeat tick label={}",
                            config.label,
                        );
                        if let Err(err) = (callback)().await {
                            tracing::error!(
                                "heartbeat callback failed label={} err={}",
                                config.label,
                                err,
                            );
                        }
                    }
                    _ = rx.changed() => {
                        if *rx.borrow() {
                            tracing::info!(
                                "heartbeat shutdown label={}",
                                config.label,
                            );
                            break;
                        }
                    }
                }
            }
        });
        Ok(())
    }

    /// stop the heartbeat scheduler
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
    }
}

impl Drop for HeartbeatScheduler {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
#[path = "scheduler_test.rs"]
mod scheduler_test;
