/// channel gateway errors
#[derive(Debug)]
pub enum ChannelError {
    /// webhook signature verification failed
    InvalidSignature(String),
    /// upstream api returned an error
    UpstreamApi(String),
    /// message parsing/validation failed
    ParseFailed(String),
    /// channel not configured or disabled
    NotConfigured(String),
    /// internal transport/routing error
    Transport(String),
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSignature(msg) => write!(f, "invalid signature: {msg}"),
            Self::UpstreamApi(msg) => write!(f, "upstream api error: {msg}"),
            Self::ParseFailed(msg) => write!(f, "parse failed: {msg}"),
            Self::NotConfigured(msg) => write!(f, "not configured: {msg}"),
            Self::Transport(msg) => write!(f, "transport error: {msg}"),
        }
    }
}

impl std::error::Error for ChannelError {}

impl axum::response::IntoResponse for ChannelError {
    fn into_response(self) -> axum::response::Response {
        let status = match &self {
            Self::InvalidSignature(_) => axum::http::StatusCode::UNAUTHORIZED,
            Self::UpstreamApi(_) => axum::http::StatusCode::BAD_GATEWAY,
            Self::ParseFailed(_) => axum::http::StatusCode::BAD_REQUEST,
            Self::NotConfigured(_) => axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Self::Transport(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = serde_json::json!({ "error": self.to_string() });
        (status, axum::Json(body)).into_response()
    }
}
