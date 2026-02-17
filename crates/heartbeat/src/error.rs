/// heartbeat scheduler errors
#[derive(Debug)]
pub enum HeartbeatError {
    /// scheduler task failed to start
    StartFailed(String),
    /// scheduler was cancelled
    Cancelled,
    /// callback execution failed
    CallbackFailed(String),
}

impl std::fmt::Display for HeartbeatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StartFailed(msg) => write!(f, "heartbeat start failed: {msg}"),
            Self::Cancelled => write!(f, "heartbeat cancelled"),
            Self::CallbackFailed(msg) => write!(f, "heartbeat callback failed: {msg}"),
        }
    }
}

impl std::error::Error for HeartbeatError {}
