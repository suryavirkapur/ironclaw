use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, OpenApi, ToSchema};
use utoipa_scalar::{Scalar, Servable};

use crate::AppState;

/// openapi doc aggregator
#[derive(OpenApi)]
#[openapi(
    info(
        title = "ironclaw host api",
        version = "0.10.1",
        description = "ironclaw security-first ai agent runtime — host daemon api"
    ),
    tags(
        (name = "channels", description = "channel webhook receivers"),
        (name = "admin", description = "admin panel endpoints"),
        (name = "heartbeat", description = "proactive heartbeat status"),
        (name = "health", description = "health and readiness")
    ),
    paths(
        health_handler,
        readiness_handler,
        channel_telegram_webhook,
        channel_whatsapp_webhook,
        channel_whatsapp_verify,
        admin_vms_list,
        admin_vm_detail,
        admin_vm_stop,
        admin_keys_list,
        admin_key_set,
        admin_key_delete,
        admin_memory_list,
        admin_memory_detail,
        admin_heartbeat_status,
    ),
    components(
        schemas(
            HealthResponse,
            ReadinessResponse,
            channels::telegram::TelegramUpdate,
            channels::telegram::TelegramMessage,
            channels::telegram::TelegramChat,
            channels::whatsapp::WhatsAppWebhook,
            channels::whatsapp::WhatsAppEntry,
            channels::whatsapp::WhatsAppChange,
            channels::whatsapp::WhatsAppValue,
            channels::whatsapp::WhatsAppMetadata,
            channels::whatsapp::WhatsAppInboundMessage,
            channels::whatsapp::WhatsAppText,
            WhatsAppVerifyQuery,
            MemoryListQuery,
            VmSummary,
            VmDetail,
            ApiKeyEntry,
            ApiKeySetRequest,
            MemorySummary,
            MemoryDetail,
            HeartbeatStatus,
            ApiError,
        )
    ),
)]
pub struct ApiDoc;

type ApiResult<T> = Result<T, ApiRouteError>;

/// build the complete router with openapi + scalar ui
pub fn build_router(state: AppState) -> Router {
    let health_routes = Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(readiness_handler));

    let channel_routes = Router::new()
        .route("/telegram/webhook", post(channel_telegram_webhook))
        .route(
            "/whatsapp/webhook",
            get(channel_whatsapp_verify).post(channel_whatsapp_webhook),
        );

    let admin_routes = Router::new()
        .route("/vms", get(admin_vms_list))
        .route("/vms/{vm_id}", get(admin_vm_detail))
        .route("/vms/{vm_id}/stop", post(admin_vm_stop))
        .route("/keys", get(admin_keys_list))
        .route(
            "/keys/{key_name}",
            post(admin_key_set).delete(admin_key_delete),
        )
        .route("/memory", get(admin_memory_list))
        .route("/memory/{memory_id}", get(admin_memory_detail))
        .route("/heartbeat", get(admin_heartbeat_status));

    Router::new()
        .nest(
            "/api",
            Router::new()
                .merge(health_routes)
                .nest("/channels", channel_routes)
                .nest("/admin", admin_routes),
        )
        .merge(Scalar::with_url("/api/docs", ApiDoc::openapi()))
        .with_state(state)
}

/// standard api error envelope
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiError {
    pub error: String,
}

impl ApiError {
    fn new(msg: impl Into<String>) -> Self {
        Self { error: msg.into() }
    }
}

#[derive(Debug)]
enum ApiRouteError {
    BadRequest(String),
    Unauthorized(String),
    Forbidden(String),
    NotFound(String),
    Internal(String),
}

impl ApiRouteError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    fn status_code(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::BadRequest(message) => message,
            Self::Unauthorized(message) => message,
            Self::Forbidden(message) => message,
            Self::NotFound(message) => message,
            Self::Internal(message) => message,
        }
    }
}

impl std::fmt::Display for ApiRouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl std::error::Error for ApiRouteError {}

impl From<crate::IronclawError> for ApiRouteError {
    fn from(value: crate::IronclawError) -> Self {
        Self::internal(value.to_string())
    }
}

impl IntoResponse for ApiRouteError {
    fn into_response(self) -> Response {
        (
            self.status_code(),
            Json(ApiError::new(self.message().to_string())),
        )
            .into_response()
    }
}

/// health check response
#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// readiness probe response
#[derive(Debug, Serialize, ToSchema)]
pub struct ReadinessResponse {
    pub ready: bool,
    pub sqlite: bool,
    pub firecracker: bool,
}

#[utoipa::path(
    get,
    path = "/api/health",
    tag = "health",
    responses((status = 200, description = "service healthy", body = HealthResponse))
)]
async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    })
}

#[utoipa::path(
    get,
    path = "/api/ready",
    tag = "health",
    responses((status = 200, description = "readiness status", body = ReadinessResponse))
)]
async fn readiness_handler(State(state): State<AppState>) -> Json<ReadinessResponse> {
    let sqlite = crate::check_sqlite_ready(&state);
    Json(ReadinessResponse {
        ready: sqlite,
        sqlite,
        firecracker: state.host_config.firecracker.enabled,
    })
}

#[utoipa::path(
    post,
    path = "/api/channels/telegram/webhook",
    tag = "channels",
    request_body = channels::telegram::TelegramUpdate,
    responses(
        (status = 200, description = "update accepted"),
        (status = 400, description = "parse error", body = ApiError),
        (status = 401, description = "invalid webhook secret", body = ApiError),
        (status = 500, description = "routing failure", body = ApiError)
    )
)]
async fn channel_telegram_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(update): Json<channels::telegram::TelegramUpdate>,
) -> ApiResult<StatusCode> {
    validate_channel_secret(&state, &headers)?;
    let inbound = channels::telegram::parse_update(update)
        .map_err(|err| ApiRouteError::bad_request(err.to_string()))?;
    crate::route_webhook_message_to_guest(
        &state,
        "telegram",
        inbound.sender_id.as_str(),
        inbound.text.as_str(),
    )
    .await?;
    Ok(StatusCode::OK)
}

/// whatsapp verification query params
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct WhatsAppVerifyQuery {
    #[serde(rename = "hub.mode")]
    pub hub_mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    pub hub_verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    pub hub_challenge: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/channels/whatsapp/webhook",
    tag = "channels",
    params(WhatsAppVerifyQuery),
    responses(
        (status = 200, description = "verification challenge echoed"),
        (status = 403, description = "token mismatch", body = ApiError)
    )
)]
async fn channel_whatsapp_verify(
    State(state): State<AppState>,
    Query(query): Query<WhatsAppVerifyQuery>,
) -> ApiResult<String> {
    let mode = query.hub_mode.as_deref().unwrap_or("");
    let token = query.hub_verify_token.as_deref().unwrap_or("");
    let challenge = query.hub_challenge.unwrap_or_default();
    if mode != "subscribe" {
        return Err(ApiRouteError::Forbidden(
            "verification mode is invalid".into(),
        ));
    }
    let expected = state.host_config.security.webhook_secret.as_str();
    if token != expected {
        return Err(ApiRouteError::Forbidden(
            "verification token mismatch".into(),
        ));
    }
    Ok(challenge)
}

#[utoipa::path(
    post,
    path = "/api/channels/whatsapp/webhook",
    tag = "channels",
    request_body = channels::whatsapp::WhatsAppWebhook,
    responses(
        (status = 200, description = "webhook accepted"),
        (status = 400, description = "parse error", body = ApiError),
        (status = 401, description = "invalid webhook secret", body = ApiError),
        (status = 500, description = "routing failure", body = ApiError)
    )
)]
async fn channel_whatsapp_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(webhook): Json<channels::whatsapp::WhatsAppWebhook>,
) -> ApiResult<StatusCode> {
    validate_channel_secret(&state, &headers)?;
    let messages = channels::whatsapp::parse_webhook(webhook)
        .map_err(|err| ApiRouteError::bad_request(err.to_string()))?;
    for inbound in messages {
        crate::route_webhook_message_to_guest(
            &state,
            "whatsapp",
            inbound.sender_id.as_str(),
            inbound.text.as_str(),
        )
        .await?;
    }
    Ok(StatusCode::OK)
}

/// summary of an active microvm
#[derive(Debug, Serialize, ToSchema)]
pub struct VmSummary {
    pub vm_id: String,
    pub user_id: String,
    pub status: String,
    pub uptime_seconds: u64,
}

/// detailed microvm information
#[derive(Debug, Serialize, ToSchema)]
pub struct VmDetail {
    pub vm_id: String,
    pub user_id: String,
    pub status: String,
    pub uptime_seconds: u64,
    pub brain_path: String,
    pub memory_mb: u64,
    pub vcpu_count: u32,
}

#[utoipa::path(
    get,
    path = "/api/admin/vms",
    tag = "admin",
    responses((status = 200, description = "list of active vms", body = Vec<VmSummary>))
)]
async fn admin_vms_list(State(state): State<AppState>) -> Json<Vec<VmSummary>> {
    let snapshots = state.list_vm_snapshots().await;
    let body = snapshots
        .into_iter()
        .map(|entry| VmSummary {
            vm_id: entry.vm_id,
            user_id: entry.user_id,
            status: entry.status,
            uptime_seconds: entry.uptime_seconds,
        })
        .collect::<Vec<_>>();
    Json(body)
}

#[utoipa::path(
    get,
    path = "/api/admin/vms/{vm_id}",
    tag = "admin",
    params(("vm_id" = String, Path, description = "vm identifier")),
    responses(
        (status = 200, description = "vm details", body = VmDetail),
        (status = 404, description = "vm not found", body = ApiError)
    )
)]
async fn admin_vm_detail(
    State(state): State<AppState>,
    Path(vm_id): Path<String>,
) -> ApiResult<Json<VmDetail>> {
    let snapshot = state
        .vm_detail(vm_id.as_str())
        .await
        .ok_or_else(|| ApiRouteError::not_found(format!("vm not found: {vm_id}")))?;
    Ok(Json(VmDetail {
        vm_id: snapshot.vm_id,
        user_id: snapshot.user_id,
        status: snapshot.status,
        uptime_seconds: snapshot.uptime_seconds,
        brain_path: snapshot.brain_path,
        memory_mb: snapshot.memory_mb,
        vcpu_count: snapshot.vcpu_count,
    }))
}

#[utoipa::path(
    post,
    path = "/api/admin/vms/{vm_id}/stop",
    tag = "admin",
    params(("vm_id" = String, Path, description = "vm identifier")),
    responses(
        (status = 200, description = "vm stopped"),
        (status = 404, description = "vm not found", body = ApiError)
    )
)]
async fn admin_vm_stop(
    State(state): State<AppState>,
    Path(vm_id): Path<String>,
) -> ApiResult<StatusCode> {
    let stopped = state.stop_vm(vm_id.as_str()).await?;
    if !stopped {
        return Err(ApiRouteError::not_found(format!("vm not found: {vm_id}")));
    }
    Ok(StatusCode::OK)
}

/// api key entry visible in admin panel
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyEntry {
    pub name: String,
    pub masked_value: String,
    pub updated_at: String,
}

/// set an api key
#[derive(Debug, Deserialize, ToSchema)]
pub struct ApiKeySetRequest {
    pub value: String,
}

#[utoipa::path(
    get,
    path = "/api/admin/keys",
    tag = "admin",
    responses((status = 200, description = "list of api keys", body = Vec<ApiKeyEntry>))
)]
async fn admin_keys_list(State(state): State<AppState>) -> ApiResult<Json<Vec<ApiKeyEntry>>> {
    let entries = crate::list_host_api_keys(&state)?;
    let body = entries
        .into_iter()
        .map(|entry| ApiKeyEntry {
            name: entry.name,
            masked_value: entry.masked_value,
            updated_at: entry.updated_at,
        })
        .collect::<Vec<_>>();
    Ok(Json(body))
}

#[utoipa::path(
    post,
    path = "/api/admin/keys/{key_name}",
    tag = "admin",
    params(("key_name" = String, Path, description = "key identifier")),
    request_body = ApiKeySetRequest,
    responses(
        (status = 200, description = "key stored"),
        (status = 400, description = "invalid request", body = ApiError)
    )
)]
async fn admin_key_set(
    State(state): State<AppState>,
    Path(key_name): Path<String>,
    Json(req): Json<ApiKeySetRequest>,
) -> ApiResult<StatusCode> {
    if req.value.trim().is_empty() {
        return Err(ApiRouteError::bad_request("key value must not be empty"));
    }
    crate::set_host_api_key(&state, key_name.as_str(), req.value.as_str())?;
    Ok(StatusCode::OK)
}

#[utoipa::path(
    delete,
    path = "/api/admin/keys/{key_name}",
    tag = "admin",
    params(("key_name" = String, Path, description = "key identifier")),
    responses(
        (status = 200, description = "key deleted"),
        (status = 404, description = "key not found", body = ApiError)
    )
)]
async fn admin_key_delete(
    State(state): State<AppState>,
    Path(key_name): Path<String>,
) -> ApiResult<StatusCode> {
    let deleted = crate::delete_host_api_key(&state, key_name.as_str())?;
    if !deleted {
        return Err(ApiRouteError::not_found(format!(
            "key not found: {key_name}"
        )));
    }
    Ok(StatusCode::OK)
}

/// summary of a memory entry
#[derive(Debug, Serialize, ToSchema)]
pub struct MemorySummary {
    pub id: String,
    pub kind: String,
    pub preview: String,
    pub updated_at: String,
}

/// detailed memory entry
#[derive(Debug, Serialize, ToSchema)]
pub struct MemoryDetail {
    pub id: String,
    pub kind: String,
    pub content: String,
    pub metadata: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct MemoryListQuery {
    pub user_id: Option<String>,
    pub limit: Option<u32>,
}

#[utoipa::path(
    get,
    path = "/api/admin/memory",
    tag = "admin",
    params(MemoryListQuery),
    responses((status = 200, description = "list of memory entries", body = Vec<MemorySummary>))
)]
async fn admin_memory_list(
    State(state): State<AppState>,
    Query(query): Query<MemoryListQuery>,
) -> ApiResult<Json<Vec<MemorySummary>>> {
    let user_id = query.user_id.unwrap_or_else(|| "owner".to_string());
    let limit = query.limit.unwrap_or(50).clamp(1, 200) as usize;
    let rows = crate::list_host_memories(&state, user_id.as_str(), limit)?;
    let body = rows
        .into_iter()
        .map(|row| MemorySummary {
            id: row.id.to_string(),
            kind: row.kind,
            preview: row.preview,
            updated_at: row.updated_at,
        })
        .collect::<Vec<_>>();
    Ok(Json(body))
}

#[utoipa::path(
    get,
    path = "/api/admin/memory/{memory_id}",
    tag = "admin",
    params(("memory_id" = String, Path, description = "memory entry id")),
    responses(
        (status = 200, description = "memory detail", body = MemoryDetail),
        (status = 404, description = "not found", body = ApiError)
    )
)]
async fn admin_memory_detail(
    State(state): State<AppState>,
    Path(memory_id): Path<String>,
) -> ApiResult<Json<MemoryDetail>> {
    let id = memory_id
        .parse::<i64>()
        .map_err(|_| ApiRouteError::bad_request("memory id must be an integer"))?;
    let row = crate::get_host_memory(&state, "owner", id)?
        .ok_or_else(|| ApiRouteError::not_found(format!("memory not found: {memory_id}")))?;
    Ok(Json(MemoryDetail {
        id: row.id.to_string(),
        kind: row.kind,
        content: row.content,
        metadata: row.metadata,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

/// heartbeat scheduler status
#[derive(Debug, Serialize, ToSchema)]
pub struct HeartbeatStatus {
    pub running: bool,
    pub interval_seconds: u64,
    pub last_tick_at: Option<String>,
    pub total_ticks: u64,
}

#[utoipa::path(
    get,
    path = "/api/admin/heartbeat",
    tag = "heartbeat",
    responses((status = 200, description = "heartbeat status", body = HeartbeatStatus))
)]
async fn admin_heartbeat_status(State(state): State<AppState>) -> Json<HeartbeatStatus> {
    let status = state.heartbeat_snapshot();
    Json(HeartbeatStatus {
        running: status.running,
        interval_seconds: status.interval_seconds,
        last_tick_at: status.last_tick_at,
        total_ticks: status.total_ticks,
    })
}

fn validate_channel_secret(state: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    let secret = headers
        .get("x-webhook-secret")
        .and_then(|value| value.to_str().ok());
    if security::auth::validate_webhook_secret(&state.host_config.security.webhook_secret, secret) {
        return Ok(());
    }
    Err(ApiRouteError::Unauthorized(
        "invalid webhook secret".to_string(),
    ))
}

#[cfg(test)]
mod api_test {
    use super::*;

    #[test]
    fn api_error_serializes() {
        let err = ApiError::new("something failed");
        let json = serde_json::to_value(&err).expect("serialize");
        assert_eq!(json["error"], "something failed");
    }

    #[test]
    fn openapi_spec_generates() {
        let spec = ApiDoc::openapi();
        let json = serde_json::to_string(&spec).expect("serialize openapi");
        assert!(json.contains("ironclaw host api"));
        assert!(json.contains("/api/channels/telegram/webhook"));
        assert!(json.contains("/api/channels/whatsapp/webhook"));
        assert!(json.contains("/api/admin/vms"));
        assert!(json.contains("/api/admin/keys"));
        assert!(json.contains("/api/admin/memory"));
        assert!(json.contains("/api/admin/heartbeat"));
    }
}
