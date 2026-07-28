use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
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
        version = "0.10.0",
        description = "ironclaw security-first ai agent runtime — host daemon api"
    ),
    tags(
        (name = "channels", description = "channel webhook receivers"),
        (name = "gateway", description = "gateway pairing and status"),
        (name = "admin", description = "admin panel endpoints"),
        (name = "memory", description = "host-side memory management"),
        (name = "soul-guard", description = "soul change approval"),
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

/// build the complete router with openapi + scalar ui
pub fn build_router(state: AppState) -> Router {
    let api_routes = Router::new()
        // health
        .route("/api/health", get(health_handler))
        .route("/api/ready", get(readiness_handler))
        // channel webhooks
        .route(
            "/api/channels/telegram/webhook",
            post(channel_telegram_webhook),
        )
        .route(
            "/api/channels/whatsapp/webhook",
            get(channel_whatsapp_verify).post(channel_whatsapp_webhook),
        )
        // admin: vms
        .route("/api/admin/vms", get(admin_vms_list))
        .route("/api/admin/vms/{vm_id}", get(admin_vm_detail))
        .route("/api/admin/vms/{vm_id}/stop", post(admin_vm_stop))
        // admin: api keys
        .route("/api/admin/keys", get(admin_keys_list))
        .route(
            "/api/admin/keys/{key_name}",
            post(admin_key_set).delete(admin_key_delete),
        )
        // admin: memory
        .route("/api/admin/memory", get(admin_memory_list))
        .route("/api/admin/memory/{memory_id}", get(admin_memory_detail))
        // heartbeat
        .route("/api/admin/heartbeat", get(admin_heartbeat_status))
        .with_state(state);

    api_routes.merge(Scalar::with_url("/api/docs", ApiDoc::openapi()))
}

// ---------------------------------------------------------------------------
// shared response types
// ---------------------------------------------------------------------------

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
    pub postgres: bool,
    pub firecracker: bool,
}

// ---------------------------------------------------------------------------
// health endpoints
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/health",
    tag = "health",
    responses(
        (status = 200, description = "service healthy", body = HealthResponse)
    )
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
    responses(
        (status = 200, description = "readiness status", body = ReadinessResponse)
    )
)]
async fn readiness_handler(State(state): State<AppState>) -> Json<ReadinessResponse> {
    let firecracker_ok = state.host_config.firecracker.enabled;
    Json(ReadinessResponse {
        ready: true,
        postgres: false,
        firecracker: firecracker_ok,
    })
}

// ---------------------------------------------------------------------------
// channel webhook endpoints
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/channels/telegram/webhook",
    tag = "channels",
    request_body = channels::telegram::TelegramUpdate,
    responses(
        (status = 200, description = "update accepted"),
        (status = 400, description = "parse error", body = ApiError),
        (status = 401, description = "invalid signature", body = ApiError)
    )
)]
async fn channel_telegram_webhook(
    State(state): State<AppState>,
    Json(update): Json<channels::telegram::TelegramUpdate>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let inbound = channels::telegram::parse_update(update).map_err(|err| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiError::new(err.to_string())),
        )
    })?;
    tracing::info!(
        "telegram webhook sender={} text_len={}",
        inbound.sender_id,
        inbound.text.len(),
    );
    // todo: route inbound message to guest via vsock/ipc
    let _ = (state, inbound);
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
        (status = 403, description = "token mismatch")
    )
)]
async fn channel_whatsapp_verify(
    State(state): State<AppState>,
    Query(query): Query<WhatsAppVerifyQuery>,
) -> Result<String, StatusCode> {
    let mode = query.hub_mode.as_deref().unwrap_or("");
    let token = query.hub_verify_token.as_deref().unwrap_or("");
    let challenge = query.hub_challenge.unwrap_or_default();

    if mode != "subscribe" {
        return Err(StatusCode::FORBIDDEN);
    }

    let expected = state.host_config.security.webhook_secret.as_str();
    if token != expected {
        return Err(StatusCode::FORBIDDEN);
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
        (status = 400, description = "parse error", body = ApiError)
    )
)]
async fn channel_whatsapp_webhook(
    State(state): State<AppState>,
    Json(webhook): Json<channels::whatsapp::WhatsAppWebhook>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let messages = channels::whatsapp::parse_webhook(webhook).map_err(|err| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiError::new(err.to_string())),
        )
    })?;
    tracing::info!("whatsapp webhook messages_count={}", messages.len(),);
    // todo: route each inbound message to guest via vsock/ipc
    let _ = (state, messages);
    Ok(StatusCode::OK)
}

// ---------------------------------------------------------------------------
// admin: vm management
// ---------------------------------------------------------------------------

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
    responses(
        (status = 200, description = "list of active vms", body = Vec<VmSummary>)
    )
)]
async fn admin_vms_list(State(_state): State<AppState>) -> Json<Vec<VmSummary>> {
    // todo: query vm manager for running instances
    Json(vec![])
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
    State(_state): State<AppState>,
    Path(vm_id): Path<String>,
) -> Result<Json<VmDetail>, (StatusCode, Json<ApiError>)> {
    // todo: look up vm by id
    Err((
        StatusCode::NOT_FOUND,
        Json(ApiError::new(format!("vm not found: {vm_id}"))),
    ))
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
    State(_state): State<AppState>,
    Path(vm_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    // todo: stop vm via vm_manager
    tracing::info!("admin stop vm={vm_id}");
    Err((
        StatusCode::NOT_FOUND,
        Json(ApiError::new(format!("vm not found: {vm_id}"))),
    ))
}

// ---------------------------------------------------------------------------
// admin: api key management (host-side only, never leaves host)
// ---------------------------------------------------------------------------

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
    responses(
        (status = 200, description = "list of api keys", body = Vec<ApiKeyEntry>)
    )
)]
async fn admin_keys_list(State(_state): State<AppState>) -> Json<Vec<ApiKeyEntry>> {
    // todo: read from postgres host key store
    Json(vec![])
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
    State(_state): State<AppState>,
    Path(key_name): Path<String>,
    Json(req): Json<ApiKeySetRequest>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    if req.value.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError::new("key value must not be empty")),
        ));
    }
    // todo: store in postgres, never expose raw value again
    tracing::info!("admin key set name={key_name}");
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
    State(_state): State<AppState>,
    Path(key_name): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    // todo: delete from postgres
    tracing::info!("admin key delete name={key_name}");
    Err((
        StatusCode::NOT_FOUND,
        Json(ApiError::new(format!("key not found: {key_name}"))),
    ))
}

// ---------------------------------------------------------------------------
// admin: memory management (host-side, injected into guest per execution)
// ---------------------------------------------------------------------------

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

#[utoipa::path(
    get,
    path = "/api/admin/memory",
    tag = "memory",
    responses(
        (status = 200, description = "list of memory entries", body = Vec<MemorySummary>)
    )
)]
async fn admin_memory_list(State(_state): State<AppState>) -> Json<Vec<MemorySummary>> {
    // todo: query postgres memory store
    Json(vec![])
}

#[utoipa::path(
    get,
    path = "/api/admin/memory/{memory_id}",
    tag = "memory",
    params(("memory_id" = String, Path, description = "memory entry id")),
    responses(
        (status = 200, description = "memory detail", body = MemoryDetail),
        (status = 404, description = "not found", body = ApiError)
    )
)]
async fn admin_memory_detail(
    State(_state): State<AppState>,
    Path(memory_id): Path<String>,
) -> Result<Json<MemoryDetail>, (StatusCode, Json<ApiError>)> {
    // todo: query postgres memory store
    Err((
        StatusCode::NOT_FOUND,
        Json(ApiError::new(format!("memory not found: {memory_id}"))),
    ))
}

// ---------------------------------------------------------------------------
// heartbeat status
// ---------------------------------------------------------------------------

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
    responses(
        (status = 200, description = "heartbeat status", body = HeartbeatStatus)
    )
)]
async fn admin_heartbeat_status(State(_state): State<AppState>) -> Json<HeartbeatStatus> {
    // todo: read from heartbeat scheduler state
    Json(HeartbeatStatus {
        running: false,
        interval_seconds: 30 * 60,
        last_tick_at: None,
        total_ticks: 0,
    })
}

#[cfg(test)]
mod api_test {
    use super::*;

    #[test]
    fn health_response_version() {
        let resp = HealthResponse {
            status: "ok".into(),
            version: "0.10.0".into(),
        };
        let json = serde_json::to_value(&resp).expect("serialize");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["version"], "0.10.0");
    }

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
