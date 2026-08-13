use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};

mod api;
mod auth_transport;
mod daemon;
mod host_tools;
mod llm_client;
mod mcp;
mod soul_guard;
mod whatsapp;

use auth_transport::AuthenticatedTransport;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use common::config::{GuestConfig, HostConfig, HostExecutionMode};
#[cfg(feature = "firecracker")]
use common::firecracker::{FirecrackerManager, FirecrackerManagerConfig};
use common::firecracker::{StubVmManager, VmConfig, VmInstance, VmManager};
use common::logging::{init_logging, LoggingConfig, LoggingHandle};
use common::proto::ironclaw::{
    agent_control, message_envelope, AgentControl, Artifact, MessageEnvelope, UploadedFile,
};
use common::slack::{
    parse_slack_message, validate_slack_signature, SlackResponse, SlackUrlVerification,
};
use daemon::GatewayCommand;
use farm::{FarmRegistry, FarmTask, TaskLedger, TaskState};
use futures::{SinkExt, StreamExt};
use host_tools::{run_host_tool, truncate_tool_output};
use include_dir::{include_dir, Dir};
use llm_client::{ConversationMessage, LlmClient, ToolLoopObservation, ToolPlan};
use memory::{
    build_memory_block, forget_memories_by_query, forget_memory_by_id, initialize_schema,
    list_pinned_memories, maybe_summarize_session, redact_secrets, retrieve_memories,
    upsert_memory, NewMemory,
};
use prost::Message as ProstMessage;
use rusqlite::Connection;
use security::auth::{channel_allowed, validate_webhook_secret};
use security::pairing::PairingManager;
use security::rate_limiter::{RateLimitConfig, RateLimiter};
use serde::Deserialize;
use serde::Serialize;
use soul_guard::{
    decide_approval, has_pending_approval_for_user, list_pending_approvals, run_monitor,
    soul_guard_db_path, SoulDecision,
};
use std::cmp::min;
use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use whatsapp::should_enable_whatsapp;

static UI_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../ui");
const TELEGRAM_CHUNK_MAX_CHARS: usize = 4096;
const MAX_INBOUND_FILE_BYTES: usize = 8 * 1024 * 1024;
const TELEGRAM_TRANSCRIPT_MAX_TURNS: usize = 50;
const TELEGRAM_RETRY_MAX_ATTEMPTS: usize = 2;
const MEMORY_RETRIEVAL_LIMIT: usize = 10;
const MEMORY_PROMPT_BUDGET_CHARS: usize = 3200;
const IDLE_CHECK_SECONDS: u64 = 10;

#[tokio::main]
async fn main() -> Result<(), IronclawError> {
    let cli = daemon::CliArgs::parse()?;
    let config_path = host_config_path()?;
    let config = load_host_config_from_path(&config_path)?;

    if let Some(command) = cli.gateway_command.clone() {
        return run_gateway_cli(&config, command);
    }

    let pid_file = resolve_pid_file(&cli, &config)?;

    if cli.stop {
        daemon::stop_daemon(&pid_file)?;
        return Ok(());
    }

    if cli.should_spawn_daemon() {
        daemon::spawn_daemon_child(&cli)?;
        return Ok(());
    }

    let _pid_file_guard =
        if cli.daemon_child || cli.pid_file.is_some() || config.daemon.pid_file.is_some() {
            Some(daemon::PidFileGuard::create(pid_file)?)
        } else {
            None
        };

    let logging = init_logging(LoggingConfig {
        level: config.log_level,
        log_file: config.daemon.log_file.clone(),
        rotate_keep: config.daemon.log_rotate_keep,
        rotate_max_bytes: config.daemon.log_rotate_max_bytes,
    })
    .map_err(|err| IronclawError::new(format!("logging init failed: {err}")))?;

    run_server(config, config_path, logging).await
}

async fn run_server(
    config: HostConfig,
    config_path: PathBuf,
    logging: LoggingHandle,
) -> Result<(), IronclawError> {
    let graceful_timeout = Duration::from_millis(config.daemon.graceful_timeout_ms);
    spawn_reload_signal_task(config_path, logging);
    if test_no_bind_enabled() {
        return run_no_bind_daemon().await;
    }

    let run_telegram = should_enable_telegram(&config);
    let telegram_settings = if run_telegram {
        Some(TelegramSettings::from_config(&config)?)
    } else {
        None
    };
    let run_whatsapp = should_enable_whatsapp(&config);
    let whatsapp_config = if run_whatsapp {
        Some(config.whatsapp.clone())
    } else {
        None
    };
    let addr = format!("{}:{}", config.server.bind, config.server.port);
    let state = AppState::new(config)?;
    let legacy_routes = Router::new()
        .route("/ws", get(ws_handler))
        .route("/api/gateway/pair/start", post(gateway_pair_start_handler))
        .route(
            "/api/gateway/pair/verify",
            post(gateway_pair_verify_handler),
        )
        .route("/api/gateway/status", get(gateway_status_handler))
        .route("/webhooks/{channel}", post(webhook_handler))
        .route("/ui", get(ui_index_handler))
        .route("/ui/{*path}", get(ui_asset_handler))
        .route("/api/soul-guard/pending", get(soul_guard_pending_handler))
        .route(
            "/api/soul-guard/decision",
            post(soul_guard_decision_handler),
        )
        .with_state(state.clone());

    let api_routes = api::build_router(state.clone());
    let app = legacy_routes.merge(api_routes);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|err| IronclawError::new(format!("bind failed: {err}")))?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let shutdown_server_rx = shutdown_rx.clone();
    let soul_guard_state = state.clone();
    let soul_guard_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        let users_root = soul_guard_state.host_config.storage.users_root.clone();
        let db_path = soul_guard_state.soul_guard_db_path.as_ref().clone();
        if let Err(err) = run_monitor(users_root, db_path, soul_guard_shutdown).await {
            tracing::error!("soul guard monitor failed: {err}");
        }
    });

    let mut server_task = tokio::spawn(async move {
        let mut rx = shutdown_server_rx;
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                loop {
                    if *rx.borrow() {
                        break;
                    }
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await
            .map_err(|err| IronclawError::new(format!("server failed: {err}")))
    });

    let mut telegram_task = if let Some(settings) = telegram_settings {
        let telegram_state = state.clone();
        let rx = shutdown_rx.clone();
        Some(tokio::spawn(async move {
            run_telegram_loop(telegram_state, settings, rx).await
        }))
    } else {
        None
    };

    let mut whatsapp_task = if let Some(wa_config) = whatsapp_config {
        let whatsapp_state = state.clone();
        let rx = shutdown_rx.clone();
        Some(tokio::spawn(async move {
            run_whatsapp_loop(whatsapp_state, wa_config, rx).await
        }))
    } else {
        None
    };

    #[cfg(unix)]
    let mut sigterm_stream =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|err| IronclawError::new(format!("sigterm signal setup failed: {err}")))?;

    let result = tokio::select! {
        result = &mut server_task => {
            let _ = shutdown_tx.send(true);
            join_server_task_result(result)
        }
        result = join_optional_task(&mut telegram_task, "telegram") => {
            let _ = shutdown_tx.send(true);
            result
        }
        result = join_optional_task(&mut whatsapp_task, "whatsapp") => {
            let _ = shutdown_tx.send(true);
            result
        }
        signal = tokio::signal::ctrl_c() => {
            let _ = signal;
            let _ = shutdown_tx.send(true);
            await_shutdown(
                &mut server_task,
                &mut telegram_task,
                &mut whatsapp_task,
                graceful_timeout,
            )
            .await
        }
        _ = async {
            #[cfg(unix)]
            {
                let _ = sigterm_stream.recv().await;
            }
            #[cfg(not(unix))]
            {
                std::future::pending::<()>().await;
            }
        } => {
            let _ = shutdown_tx.send(true);
            await_shutdown(
                &mut server_task,
                &mut telegram_task,
                &mut whatsapp_task,
                graceful_timeout,
            )
            .await
        }
    };
    if let Err(err) = state.vm_manager.stop_all().await {
        tracing::warn!("vm shutdown cleanup failed: {err}");
    }
    result
}

fn test_no_bind_enabled() -> bool {
    std::env::var("IRONCLAWD_TEST_NO_BIND")
        .ok()
        .as_deref()
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

async fn run_no_bind_daemon() -> Result<(), IronclawError> {
    #[cfg(unix)]
    {
        let mut sigterm_stream =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|err| IronclawError::new(format!("sigterm signal setup failed: {err}")))?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => Ok(()),
            _ = sigterm_stream.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .map_err(|err| IronclawError::new(format!("ctrl_c failed: {err}")))?;
        Ok(())
    }
}

#[cfg(unix)]
fn spawn_reload_signal_task(config_path: PathBuf, logging: LoggingHandle) {
    tokio::spawn(async move {
        let hup_result = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup());
        let usr1_result =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1());
        let mut hup = match hup_result {
            Ok(stream) => stream,
            Err(err) => {
                tracing::error!("sighup signal setup failed: {err}");
                return;
            }
        };
        let mut usr1 = match usr1_result {
            Ok(stream) => stream,
            Err(err) => {
                tracing::error!("sigusr1 signal setup failed: {err}");
                return;
            }
        };
        loop {
            tokio::select! {
                _ = hup.recv() => {
                    match load_host_config_from_path(&config_path) {
                        Ok(reloaded) => {
                            if let Err(err) = logging.set_level(reloaded.log_level) {
                                tracing::error!("sighup logging reload failed: {err}");
                            } else {
                                tracing::info!("sighup reloaded logging level");
                            }
                        }
                        Err(err) => tracing::error!("sighup config reload failed: {err}"),
                    }
                }
                _ = usr1.recv() => {
                    if let Err(err) = logging.rotate_logs() {
                        tracing::error!("sigusr1 log rotation failed: {err}");
                    } else {
                        tracing::info!("sigusr1 rotated logs");
                    }
                }
            }
        }
    });
}

#[cfg(not(unix))]
fn spawn_reload_signal_task(_config_path: PathBuf, _logging: LoggingHandle) {}

fn resolve_pid_file(cli: &daemon::CliArgs, config: &HostConfig) -> Result<PathBuf, IronclawError> {
    if let Some(path) = &cli.pid_file {
        return Ok(path.clone());
    }
    if let Some(path) = &config.daemon.pid_file {
        return Ok(path.clone());
    }
    let runtime_dir = daemon::default_runtime_dir()?;
    Ok(runtime_dir.join("ironclawd.pid"))
}

fn join_server_task_result(
    result: Result<Result<(), IronclawError>, tokio::task::JoinError>,
) -> Result<(), IronclawError> {
    match result {
        Ok(value) => value,
        Err(err) => Err(IronclawError::new(format!(
            "server task join failed: {err}"
        ))),
    }
}

async fn join_optional_task(
    task: &mut Option<JoinHandle<Result<(), IronclawError>>>,
    name: &str,
) -> Result<(), IronclawError> {
    if let Some(handle) = task.take() {
        match handle.await {
            Ok(value) => value,
            Err(err) => Err(IronclawError::new(format!(
                "{name} task join failed: {err}"
            ))),
        }
    } else {
        std::future::pending::<Result<(), IronclawError>>().await
    }
}

async fn await_shutdown(
    server_task: &mut JoinHandle<Result<(), IronclawError>>,
    telegram_task: &mut Option<JoinHandle<Result<(), IronclawError>>>,
    whatsapp_task: &mut Option<JoinHandle<Result<(), IronclawError>>>,
    graceful_timeout: Duration,
) -> Result<(), IronclawError> {
    let wait_all = async {
        let server_result = server_task.await;
        let _ = join_server_task_result(server_result);
        if let Some(handle) = telegram_task.take() {
            let _ = handle.await;
        }
        if let Some(handle) = whatsapp_task.take() {
            let _ = handle.await;
        }
    };

    match tokio::time::timeout(graceful_timeout, wait_all).await {
        Ok(_) => Ok(()),
        Err(_) => Err(IronclawError::new(
            "graceful shutdown timed out before tasks completed",
        )),
    }
}

#[derive(Clone)]
struct AppState {
    host_config: Arc<HostConfig>,
    llm_client: Arc<LlmClient>,
    vm_manager: Arc<dyn VmManager>,
    guest_config_path: Arc<PathBuf>,
    local_guest: bool,
    stub_vm_manager: Option<Arc<StubVmManager>>,
    execution_mode: RuntimeExecutionMode,
    guest_allow_bash: bool,
    guest_allow_browser: bool,
    soul_guard_db_path: Arc<PathBuf>,
    security_db_path: Arc<PathBuf>,
    farm_registry: Arc<FarmRegistry>,
    farm_tasks: TaskLedger,
    mcp_gateway: Arc<mcp::McpGateway>,
    farm_agent_locks: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl AppState {
    fn new(config: HostConfig) -> Result<Self, IronclawError> {
        let farm_registry = if config.farm.enabled {
            FarmRegistry::load_dir(&config.farm.manifests_dir).map_err(|err| {
                IronclawError::new(format!("agent farm manifest load failed: {err}"))
            })?
        } else {
            FarmRegistry::default()
        };
        if let Some(entry_agent) = config.farm.entry_agent.as_deref() {
            if farm_registry.get(entry_agent).is_none() {
                return Err(IronclawError::new(format!(
                    "farm entry agent is not registered: {entry_agent}"
                )));
            }
        }
        let farm_registry = Arc::new(farm_registry);
        let mcp_gateway = Arc::new(
            mcp::McpGateway::new(farm_registry.clone(), config.farm.clone())
                .map_err(|err| IronclawError::new(format!("MCP gateway init failed: {err}")))?,
        );
        let llm_client = Arc::new(
            LlmClient::new(config.llm.clone())
                .map_err(|err| IronclawError::new(format!("llm client init failed: {err}")))?,
        );
        let firecracker_runtime_enabled =
            config.firecracker.enabled && cfg!(feature = "firecracker");
        if config.firecracker.enabled && !cfg!(feature = "firecracker") {
            return Err(IronclawError::new(
                "firecracker is enabled in config but this binary was built without the \
                 firecracker feature",
            ));
        }
        let local_guest = !firecracker_runtime_enabled;
        let guest_config_path = Arc::new(guest_config_path());
        let (guest_allow_bash, guest_allow_browser) = load_guest_tool_flags(&guest_config_path);
        let (vm_manager, stub_vm_manager) = if firecracker_runtime_enabled {
            #[cfg(feature = "firecracker")]
            {
                let manager = FirecrackerManager::new(FirecrackerManagerConfig {
                    firecracker_bin: PathBuf::from("firecracker"),
                    kernel_path: config.firecracker.kernel_path.clone(),
                    rootfs_path: config.firecracker.rootfs_path.clone(),
                    api_socket_dir: config.firecracker.api_socket_dir.clone(),
                    vsock_uds_dir: config
                        .firecracker
                        .vsock_uds_dir
                        .clone()
                        .unwrap_or_else(|| PathBuf::from("/tmp/ironclaw/vsock")),
                    vsock_port: config
                        .firecracker
                        .vsock_port
                        .unwrap_or_else(common::firecracker::default_vsock_port),
                    vcpus: config.firecracker.vcpus,
                    memory_mib: config.firecracker.memory_mib,
                    enable_network: config.firecracker.enable_network,
                });
                (Arc::new(manager) as Arc<dyn VmManager>, None)
            }
            #[cfg(not(feature = "firecracker"))]
            {
                let stub_vm_manager = Arc::new(StubVmManager::new(32));
                let vm_manager: Arc<dyn VmManager> = stub_vm_manager.clone();
                (vm_manager, Some(stub_vm_manager))
            }
        } else {
            let stub_vm_manager = Arc::new(StubVmManager::new(32));
            let vm_manager: Arc<dyn VmManager> = stub_vm_manager.clone();
            (vm_manager, Some(stub_vm_manager))
        };
        let execution_mode = RuntimeExecutionMode::from_config(&config);
        let soul_guard_db = soul_guard_db_path(&config.storage.users_root);
        let security_db = security_db_path(&config.storage.users_root);
        let farm_tasks = TaskLedger::open(
            config.storage.users_root.join("_farm").join("tasks.json"),
        )
        .map_err(|err| IronclawError::new(format!("farm task ledger init failed: {err}")))?;
        let recovered_tasks = farm_tasks
            .fail_incomplete_on_startup(
                "task execution was interrupted by a daemon restart",
                now_ms()?,
            )
            .map_err(|err| IronclawError::new(format!("farm task recovery failed: {err}")))?;
        if recovered_tasks > 0 {
            tracing::warn!(
                recovered_tasks,
                "marked interrupted farm tasks failed during startup"
            );
        }
        Ok(Self {
            host_config: Arc::new(config),
            llm_client,
            vm_manager,
            guest_config_path,
            local_guest,
            stub_vm_manager,
            execution_mode,
            guest_allow_bash,
            guest_allow_browser,
            soul_guard_db_path: Arc::new(soul_guard_db),
            security_db_path: Arc::new(security_db),
            farm_registry,
            farm_tasks,
            mcp_gateway,
            farm_agent_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        })
    }

    fn idle_timeout_duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.host_config.idle_timeout_minutes.saturating_mul(60))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeExecutionMode {
    HostOnly,
    GuestTools,
    GuestAutonomous,
}

impl RuntimeExecutionMode {
    fn from_config(config: &HostConfig) -> Self {
        match config.execution_mode {
            HostExecutionMode::HostOnly => Self::HostOnly,
            HostExecutionMode::GuestTools => Self::GuestTools,
            HostExecutionMode::GuestAutonomous => Self::GuestAutonomous,
            HostExecutionMode::Auto => {
                if config.firecracker.enabled {
                    Self::GuestTools
                } else {
                    Self::HostOnly
                }
            }
        }
    }

    fn to_wire(self) -> &'static str {
        match self {
            Self::HostOnly => "host_only",
            Self::GuestTools => "guest_tools",
            Self::GuestAutonomous => "guest_autonomous",
        }
    }
}

#[derive(Debug)]
pub struct IronclawError {
    message: String,
}

impl std::fmt::Display for IronclawError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ironclawd error: {}", self.message)
    }
}

impl std::error::Error for IronclawError {}

impl IronclawError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Deserialize)]
struct WsQuery {
    user_id: Option<String>,
    session_id: Option<String>,
    node_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChannelSource {
    WebSocket,
    Telegram,
    WhatsApp,
}

fn resolve_owner_user_id(source: ChannelSource, inbound_user_id: Option<&str>) -> String {
    match source {
        ChannelSource::WebSocket => inbound_user_id.unwrap_or("local").to_string(),
        ChannelSource::Telegram | ChannelSource::WhatsApp => "owner".to_string(),
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WsQuery>,
) -> Response {
    if state.local_guest || state.execution_mode == RuntimeExecutionMode::HostOnly {
        tracing::warn!("websocket denied: Firecracker guest execution is required");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Firecracker guest execution is required",
        )
            .into_response();
    }
    if let Err(err) = ensure_channel_allowed(&state, "websocket") {
        tracing::warn!("websocket denied by channel allowlist: {err}");
        return (StatusCode::FORBIDDEN, "channel not allowed").into_response();
    }
    if let Err(err) = ensure_gateway_request_authorized(&state, &query, Some(&headers)) {
        tracing::warn!("websocket denied by gateway auth: {err}");
        return (StatusCode::UNAUTHORIZED, "unauthorized gateway").into_response();
    }
    let user_id = resolve_owner_user_id(ChannelSource::WebSocket, query.user_id.as_deref());
    if let Err(err) = enforce_rate_limit(&state, &user_id, "websocket", 0) {
        tracing::warn!(
            "rate limit hit user_id={} channel=websocket err={}",
            user_id,
            err
        );
        return (StatusCode::TOO_MANY_REQUESTS, "429 too many requests").into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, state, query))
        .into_response()
}

async fn handle_socket(socket: WebSocket, state: AppState, query: WsQuery) {
    let user_id = resolve_owner_user_id(ChannelSource::WebSocket, query.user_id.as_deref());
    let session_id = query.session_id.unwrap_or_else(|| "session".to_string());
    tracing::debug!(
        "channel route source=websocket user_id={} session_id={} event=ingress",
        user_id,
        session_id
    );
    let (vm_instance, guest_transport) = match start_vm_pair(&state, &user_id).await {
        Ok(pair) => pair,
        Err(err) => {
            tracing::error!("vm start failed: {err}");
            return;
        }
    };

    // host tools used only in host-only mode and explicit host fallbacks.
    let host_allowed_tools = allowed_tools_for_agent(&state, &user_id);
    let guest_allowed_tools = allowed_tools_for_agent(&state, &user_id);

    let cap_token = {
        use rand::Rng;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    };

    let mut transport = vm_instance.transport;
    tracing::debug!(
        "channel route source=websocket user_id={} event=vm_ready",
        user_id
    );

    if state.local_guest {
        if let Some(guest_transport) = guest_transport {
            let guest_user_id = user_id.clone();
            let users_root = state.host_config.storage.users_root.clone();
            let brain_root = users_root.join(&guest_user_id).join("guest");
            let guest_config_path = brain_root.join("config").join("irowclaw.toml");
            tokio::spawn(async move {
                if let Err(err) = std::fs::create_dir_all(&brain_root) {
                    tracing::warn!("create brain root failed: {err}");
                }
                std::env::set_var("IRONCLAW_BRAIN_ROOT", &brain_root);

                if let Err(err) =
                    irowclaw::runtime::run_with_transport(guest_transport, guest_config_path).await
                {
                    tracing::error!("guest runtime failed: {err}");
                }
            });
        }
    }

    let (mut sender, mut receiver) = socket.split();

    // Send AuthChallenge to client and wait for AuthAck.
    {
        use common::proto::ironclaw::AuthChallenge;
        let challenge = MessageEnvelope {
            user_id: user_id.clone(),
            session_id: session_id.clone(),
            msg_id: 0,
            timestamp_ms: now_ms().unwrap_or(0),
            cap_token: cap_token.clone(),
            payload: Some(message_envelope::Payload::AuthChallenge(AuthChallenge {
                cap_token: cap_token.clone(),
                allowed_tools: guest_allowed_tools.clone(),
                execution_mode: state.execution_mode.to_wire().to_string(),
                brave_api_key: String::new(),
                agent_manifest_toml: String::new(),
            })),
        };
        let challenge_json = match serde_json::to_string(&challenge) {
            Ok(value) => value,
            Err(err) => {
                tracing::error!("auth challenge serialize failed: {err}");
                return;
            }
        };
        if let Err(err) = sender.send(Message::Text(challenge_json.into())).await {
            tracing::error!("auth challenge send failed: {err}");
            return;
        }

        let auth_ack =
            tokio::time::timeout(std::time::Duration::from_secs(5), receiver.next()).await;
        match auth_ack {
            Ok(Some(Ok(Message::Text(text)))) => {
                let ack: MessageEnvelope = match serde_json::from_str(text.as_ref()) {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::error!("auth ack parse failed: {err}");
                        return;
                    }
                };
                match ack.payload {
                    Some(message_envelope::Payload::AuthAck(ack)) if ack.cap_token == cap_token => {
                    }
                    other => {
                        tracing::error!("invalid auth ack: {other:?}");
                        return;
                    }
                }
            }
            Ok(Some(Ok(Message::Close(_)))) => {
                tracing::debug!("client closed connection during auth");
                return;
            }
            Ok(Some(Ok(other))) => {
                tracing::error!("unexpected message type during auth: {other:?}");
                return;
            }
            Ok(Some(Err(err))) => {
                tracing::error!("auth ack recv failed: {err}");
                return;
            }
            Ok(None) => {
                tracing::error!("auth ack connection closed");
                return;
            }
            Err(_) => {
                tracing::error!("auth ack timed out");
                return;
            }
        }
    }

    // Now do internal auth with guest.
    {
        use common::proto::ironclaw::AuthChallenge;
        let challenge = MessageEnvelope {
            user_id: user_id.clone(),
            session_id: session_id.clone(),
            msg_id: 0,
            timestamp_ms: now_ms().unwrap_or(0),
            cap_token: cap_token.clone(),
            payload: Some(message_envelope::Payload::AuthChallenge(AuthChallenge {
                cap_token: cap_token.clone(),
                allowed_tools: guest_allowed_tools.clone(),
                execution_mode: state.execution_mode.to_wire().to_string(),
                brave_api_key: brave_api_key_for_guest(),
                agent_manifest_toml: agent_manifest_toml_for(&state, &user_id),
            })),
        };
        if let Err(err) = transport.send(challenge).await {
            tracing::error!("guest auth challenge send failed: {err}");
            return;
        }

        match tokio::time::timeout(std::time::Duration::from_secs(5), transport.recv()).await {
            Ok(Ok(Some(msg))) => match msg.payload {
                Some(message_envelope::Payload::AuthAck(ack)) if ack.cap_token == cap_token => {}
                other => {
                    tracing::error!("invalid guest auth ack: {other:?}");
                    return;
                }
            },
            Ok(Ok(None)) => return,
            Ok(Err(err)) => {
                tracing::error!("guest auth ack recv failed: {err}");
                return;
            }
            Err(_) => {
                tracing::error!("guest auth ack timed out");
                return;
            }
        }

        transport = Box::new(AuthenticatedTransport::new(transport, cap_token.clone()));
    }

    // host tool policy.
    let tool_user_id = user_id.clone();

    // Single-loop bridge.
    // Avoid holding a mutex across `.await` on `Transport::recv()`.
    let mut transport = transport;
    let mut msg_id = 1u64;
    let mut guest_sleeping = false;
    let idle_timeout = state.idle_timeout_duration();
    let mut last_user_activity = std::time::Instant::now();

    loop {
        tokio::select! {
            ws_msg = receiver.next() => {
                let Some(Ok(message)) = ws_msg else { break; };
                if let Message::Text(text) = message {
                    if let Err(err) = enforce_rate_limit(&state, &user_id, "websocket", 0) {
                        tracing::warn!(
                            "rate limit hit user_id={} channel=websocket err={}",
                            user_id,
                            err
                        );
                        let _ = sender
                            .send(Message::Text("429 too many requests".to_string().into()))
                            .await;
                        break;
                    }
                    last_user_activity = std::time::Instant::now();
                    match has_pending_approval_for_user(&state.soul_guard_db_path, &user_id) {
                        Ok(true) => {
                            let envelope = MessageEnvelope {
                                user_id: user_id.clone(),
                                session_id: session_id.clone(),
                                msg_id,
                                timestamp_ms: now_ms().unwrap_or(0),
                                cap_token: String::new(),
                                payload: Some(message_envelope::Payload::StreamDelta(
                                    common::proto::ironclaw::StreamDelta {
                                        delta: "security halt: pending soul.md approval".to_string(),
                                        done: true,
                                    },
                                )),
                            };
                            msg_id = msg_id.saturating_add(1);
                            let payload = match serde_json::to_string(&envelope) {
                                Ok(value) => value,
                                Err(err) => {
                                    tracing::error!("serialize ws message failed: {err}");
                                    break;
                                }
                            };
                            if sender.send(Message::Text(payload.into())).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        Ok(false) => {}
                        Err(err) => {
                            tracing::warn!("soul guard pending check failed: {err}");
                        }
                    }

                    let timestamp_ms = match now_ms() {
                        Ok(value) => value,
                        Err(err) => {
                            tracing::warn!("time error: {err}");
                            0
                        }
                    };
                    if state.execution_mode == RuntimeExecutionMode::HostOnly {
                        let response_text = match run_host_turn(
                            &state,
                            &tool_user_id,
                            &host_allowed_tools,
                            text.as_str(),
                            None,
                        )
                        .await
                        {
                            Ok(value) => value,
                            Err(err) => {
                                tracing::error!("host turn failed: {err}");
                                "llm request failed".to_string()
                            }
                        };
                        let envelope = MessageEnvelope {
                            user_id: user_id.clone(),
                            session_id: session_id.clone(),
                            msg_id,
                            timestamp_ms,
                            cap_token: String::new(),
                            payload: Some(message_envelope::Payload::StreamDelta(
                                common::proto::ironclaw::StreamDelta {
                                    delta: response_text,
                                    done: true,
                                },
                            )),
                        };
                        msg_id += 1;
                        let payload = match serde_json::to_string(&envelope) {
                            Ok(value) => value,
                            Err(err) => {
                                tracing::error!("serialize ws message failed: {err}");
                                break;
                            }
                        };
                        if sender.send(Message::Text(payload.into())).await.is_err() {
                            break;
                        }
                    } else {
                        if guest_sleeping {
                            tracing::debug!(
                                "channel route source=websocket user_id={} session_id={} action=wake reason=user_message",
                                user_id,
                                session_id
                            );
                            let wake_result = send_agent_control(
                                &mut transport,
                                &user_id,
                                &session_id,
                                msg_id,
                                agent_control::Command::Wake,
                                "user_message",
                            )
                            .await;
                            if wake_result.is_err() {
                                break;
                            }
                            msg_id = msg_id.saturating_add(1);
                            guest_sleeping = false;
                        }
                        let (payload, outbound_msg_id) = ws_text_to_guest_payload(
                            text.as_str(),
                            msg_id,
                        );
                        let envelope = MessageEnvelope {
                            user_id: user_id.clone(),
                            session_id: session_id.clone(),
                            msg_id,
                            timestamp_ms,
                            cap_token: String::new(),
                            payload: Some(payload),
                        };
                        msg_id = outbound_msg_id;
                        if let Err(err) = transport.send(envelope).await {
                            tracing::error!("send to guest failed: {err}");
                            break;
                        }
                    }
                } else if let Message::Binary(data) = message {
                    let upload = match decode_websocket_upload(data.as_ref()) {
                        Ok(upload) => upload,
                        Err(err) => {
                            let envelope = MessageEnvelope {
                                user_id: user_id.clone(),
                                session_id: session_id.clone(),
                                msg_id,
                                timestamp_ms: now_ms().unwrap_or(0),
                                cap_token: String::new(),
                                payload: Some(message_envelope::Payload::StreamDelta(
                                    common::proto::ironclaw::StreamDelta {
                                        delta: err.to_string(),
                                        done: true,
                                    },
                                )),
                            };
                            msg_id = msg_id.saturating_add(1);
                            let Ok(payload) = serde_json::to_string(&envelope) else {
                                break;
                            };
                            if sender.send(Message::Text(payload.into())).await.is_err() {
                                break;
                            }
                            continue;
                        }
                    };
                    if let Err(err) = enforce_rate_limit(
                        &state,
                        &user_id,
                        "websocket",
                        upload.data.len() as u64,
                    ) {
                        tracing::warn!(
                            "rate limit hit user_id={} channel=websocket upload err={}",
                            user_id,
                            err
                        );
                        break;
                    }
                    if state.execution_mode == RuntimeExecutionMode::HostOnly {
                        tracing::warn!("websocket upload denied outside Firecracker");
                        break;
                    }
                    last_user_activity = std::time::Instant::now();
                    if guest_sleeping {
                        if send_agent_control(
                            &mut transport,
                            &user_id,
                            &session_id,
                            msg_id,
                            agent_control::Command::Wake,
                            "uploaded_file",
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        msg_id = msg_id.saturating_add(1);
                        guest_sleeping = false;
                    }
                    let envelope = MessageEnvelope {
                        user_id: user_id.clone(),
                        session_id: session_id.clone(),
                        msg_id,
                        timestamp_ms: now_ms().unwrap_or(0),
                        cap_token: String::new(),
                        payload: Some(message_envelope::Payload::UploadedFile(upload)),
                    };
                    msg_id = msg_id.saturating_add(1);
                    if let Err(err) = transport.send(envelope).await {
                        tracing::error!("send uploaded file to guest failed: {err}");
                        break;
                    }
                }
            }

            transport_msg = transport.recv() => {
                match transport_msg {
                    Ok(Some(envelope)) => {
                        if let Some(message_envelope::Payload::JobTrigger(trigger)) =
                            envelope.payload.clone()
                        {
                            if let Err(err) = handle_guest_job_trigger(
                                &mut transport,
                                &user_id,
                                &session_id,
                                &mut msg_id,
                                &mut guest_sleeping,
                                trigger,
                                "websocket",
                            )
                            .await
                            {
                                tracing::error!("scheduled job handling failed: {err}");
                                break;
                            }
                            continue;
                        }

                        if matches!(
                            envelope.payload,
                            Some(message_envelope::Payload::AgentState(_))
                        ) {
                            continue;
                        }

                        // Handle host-side tools requested by the guest.
                        if let Some(message_envelope::Payload::ToolCallRequest(req)) =
                            envelope.payload.clone()
                        {
                            let (ok, output) = execute_requested_host_tool(
                                &state,
                                &tool_user_id,
                                &envelope.session_id,
                                &req,
                                &host_allowed_tools,
                                None,
                                None,
                            )
                            .await;

                            let resp = MessageEnvelope {
                                user_id: envelope.user_id,
                                session_id: envelope.session_id,
                                msg_id: envelope.msg_id,
                                timestamp_ms: envelope.timestamp_ms,
                                cap_token: String::new(),
                                payload: Some(message_envelope::Payload::ToolCallResponse(
                                    common::proto::ironclaw::ToolCallResponse {
                                        call_id: req.call_id,
                                        ok,
                                        output,
                                    },
                                )),
                            };

                            if let Err(err) = transport.send(resp).await {
                                tracing::error!("tool response send failed: {err}");
                                break;
                            }
                            continue;
                        }

                        let payload = match serde_json::to_string(&envelope) {
                            Ok(value) => value,
                            Err(err) => {
                                tracing::error!("serialize ws message failed: {err}");
                                break;
                            }
                        };
                        if sender.send(Message::Text(payload.into())).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        tracing::error!("transport recv failed: {err}");
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(IDLE_CHECK_SECONDS)) => {
                if should_enter_idle_sleep(
                    state.execution_mode,
                    guest_sleeping,
                    last_user_activity,
                    idle_timeout,
                ) {
                    tracing::debug!(
                        "channel route source=websocket user_id={} session_id={} action=sleep reason=idle_timeout",
                        user_id,
                        session_id
                    );
                    let sleep_result = send_agent_control(
                        &mut transport,
                        &user_id,
                        &session_id,
                        msg_id,
                        agent_control::Command::Sleep,
                        "idle_timeout",
                    )
                    .await;
                    if sleep_result.is_err() {
                        break;
                    }
                    msg_id = msg_id.saturating_add(1);
                    guest_sleeping = true;
                }
            }
        }
    }

    let _ = send_agent_control(
        &mut transport,
        &user_id,
        &session_id,
        msg_id,
        agent_control::Command::Shutdown,
        "websocket_closed",
    )
    .await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), transport.recv()).await;
    drop(transport);
    if let Err(err) = state.vm_manager.stop_vm(&user_id).await {
        tracing::warn!("websocket vm cleanup failed for {}: {}", user_id, err);
    }
}

async fn send_agent_control(
    transport: &mut Box<dyn common::transport::Transport>,
    user_id: &str,
    session_id: &str,
    msg_id: u64,
    command: agent_control::Command,
    reason: &str,
) -> Result<(), IronclawError> {
    transport
        .send(MessageEnvelope {
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            msg_id,
            timestamp_ms: now_ms().unwrap_or(0),
            cap_token: String::new(),
            payload: Some(message_envelope::Payload::AgentControl(AgentControl {
                command: command as i32,
                reason: reason.to_string(),
            })),
        })
        .await
        .map_err(|err| IronclawError::new(format!("agent control send failed: {err}")))
}

async fn handle_guest_job_trigger(
    transport: &mut Box<dyn common::transport::Transport>,
    user_id: &str,
    session_id: &str,
    msg_id: &mut u64,
    guest_sleeping: &mut bool,
    trigger: common::proto::ironclaw::JobTrigger,
    source: &str,
) -> Result<String, IronclawError> {
    tracing::debug!(
        "job trigger route source={} user_id={} session_id={} job_id={} guest_sleeping={}",
        source,
        user_id,
        session_id,
        trigger.job_id,
        *guest_sleeping
    );
    if *guest_sleeping {
        tracing::debug!(
            "job trigger route source={} user_id={} session_id={} action=wake",
            source,
            user_id,
            session_id
        );
        send_agent_control(
            transport,
            user_id,
            session_id,
            *msg_id,
            agent_control::Command::Wake,
            "scheduled_job",
        )
        .await?;
        *msg_id = msg_id.saturating_add(1);
        *guest_sleeping = false;
    }

    let call_id = *msg_id;
    *msg_id = msg_id.saturating_add(1);
    tracing::debug!(
        "job trigger route source={} user_id={} session_id={} action=run_scheduled_job job_id={} call_id={}",
        source,
        user_id,
        session_id,
        trigger.job_id,
        call_id
    );
    let (status_text, notification) =
        match run_scheduled_job_via_guest(transport, user_id, session_id, call_id, &trigger.job_id)
            .await
        {
            Ok(output) => ("success", output),
            Err(err) => {
                tracing::error!("scheduled job execution failed: {err}");
                ("failed", "scheduled job failed".to_string())
            }
        };

    let status_envelope = MessageEnvelope {
        user_id: user_id.to_string(),
        session_id: session_id.to_string(),
        msg_id: *msg_id,
        timestamp_ms: now_ms().unwrap_or(0),
        cap_token: String::new(),
        payload: Some(message_envelope::Payload::JobStatus(
            common::proto::ironclaw::JobStatus {
                job_id: trigger.job_id,
                status: status_text.to_string(),
            },
        )),
    };
    *msg_id = msg_id.saturating_add(1);
    transport
        .send(status_envelope)
        .await
        .map_err(|err| IronclawError::new(format!("job status send failed: {err}")))?;
    tracing::debug!(
        "job trigger route source={} user_id={} session_id={} action=status_sent",
        source,
        user_id,
        session_id
    );
    Ok(notification)
}

async fn run_scheduled_job_via_guest(
    transport: &mut Box<dyn common::transport::Transport>,
    user_id: &str,
    session_id: &str,
    call_id: u64,
    job_id: &str,
) -> Result<String, IronclawError> {
    transport
        .send(MessageEnvelope {
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            msg_id: call_id,
            timestamp_ms: now_ms().unwrap_or(0),
            cap_token: String::new(),
            payload: Some(message_envelope::Payload::ToolCallRequest(
                common::proto::ironclaw::ToolCallRequest {
                    call_id,
                    tool: "run_scheduled_job".to_string(),
                    input: job_id.to_string(),
                },
            )),
        })
        .await
        .map_err(|err| IronclawError::new(format!("scheduled job send failed: {err}")))?;

    loop {
        let message = transport
            .recv()
            .await
            .map_err(|err| IronclawError::new(format!("scheduled job recv failed: {err}")))?;
        let Some(envelope) = message else {
            return Err(IronclawError::new("scheduled job channel closed"));
        };
        if let Some(message_envelope::Payload::ToolCallResponse(resp)) = envelope.payload {
            if resp.call_id == call_id {
                if resp.ok {
                    return Ok(resp.output);
                }
                return Ok("failed".to_string());
            }
        }
    }
}

async fn start_vm_pair(
    state: &AppState,
    user_id: &str,
) -> Result<(VmInstance, Option<common::transport::LocalTransport>), IronclawError> {
    let vm_running = state
        .vm_manager
        .is_vm_running(user_id)
        .await
        .map_err(|err| IronclawError::new(err.to_string()))?;
    tracing::debug!(
        "channel route user_id={} event=vm_lookup running={}",
        user_id,
        vm_running
    );
    let brain_path = brain_ext4_path(&state.host_config.storage.users_root, user_id)?;
    let allowed_domains = state.host_config.security.network.allowed_domains.clone();
    let config = VmConfig {
        user_id: user_id.to_string(),
        brain_path,
        allowed_domains,
    };
    if state.local_guest {
        if let Some(manager) = &state.stub_vm_manager {
            let (instance, guest) = manager
                .start_vm_with_guest(config)
                .map_err(|err| IronclawError::new(err.to_string()))?;
            tracing::debug!(
                "channel route user_id={} event=vm_spawned mode=local_guest",
                user_id
            );
            return Ok((instance, Some(guest)));
        }
    }
    let instance = state
        .vm_manager
        .start_vm(config)
        .await
        .map_err(|err| IronclawError::new(err.to_string()))?;
    tracing::debug!(
        "channel route user_id={} event=vm_spawned mode=firecracker",
        user_id
    );
    Ok((instance, None))
}

fn spawn_farm_task_dispatch(state: AppState, task: FarmTask) {
    tokio::spawn(async move {
        if let Err(error) = dispatch_farm_task(&state, &task).await {
            tracing::error!(task_id = %task.id, assignee = %task.assignee, "A2A task failed: {error}");
            let current = state.farm_tasks.get(&task.id).ok().flatten();
            if current.is_some_and(|current| !current.state.terminal()) {
                let _ = state.farm_tasks.transition(
                    &task.id,
                    TaskState::Failed,
                    Some(serde_json::json!({"error": error.to_string()})),
                    now_ms().unwrap_or(task.updated_at_ms),
                );
            }
        }
    });
}

struct FarmVmStopGuard {
    manager: Arc<dyn VmManager>,
    agent_id: Option<String>,
}

impl FarmVmStopGuard {
    fn new(manager: Arc<dyn VmManager>, agent_id: String) -> Self {
        Self {
            manager,
            agent_id: Some(agent_id),
        }
    }

    async fn stop(mut self) {
        if let Some(agent_id) = self.agent_id.take() {
            let _ = self.manager.stop_vm(&agent_id).await;
        }
    }
}

impl Drop for FarmVmStopGuard {
    fn drop(&mut self) {
        let Some(agent_id) = self.agent_id.take() else {
            return;
        };
        let manager = self.manager.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = manager.stop_vm(&agent_id).await;
            });
        }
    }
}

async fn dispatch_farm_task(state: &AppState, task: &FarmTask) -> Result<(), IronclawError> {
    let agent_lock = {
        let mut locks = state.farm_agent_locks.lock().await;
        locks
            .entry(task.assignee.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _agent_guard = agent_lock.lock().await;
    if state
        .vm_manager
        .is_vm_running(&task.assignee)
        .await
        .map_err(|err| IronclawError::new(err.to_string()))?
    {
        return Err(IronclawError::new(format!(
            "agent {} VM transport is already owned by another session",
            task.assignee
        )));
    }

    let (vm_instance, guest_transport) = start_vm_pair(state, &task.assignee).await?;
    let vm_guard = FarmVmStopGuard::new(state.vm_manager.clone(), task.assignee.clone());
    let mut transport = vm_instance.transport;
    if state.local_guest {
        if let Some(guest_transport) = guest_transport {
            let brain_root = state
                .host_config
                .storage
                .users_root
                .join(&task.assignee)
                .join("guest");
            let guest_config_path = (*state.guest_config_path).clone();
            std::fs::create_dir_all(&brain_root)
                .map_err(|err| IronclawError::new(format!("create brain root failed: {err}")))?;
            std::env::set_var("IRONCLAW_BRAIN_ROOT", &brain_root);
            tokio::spawn(async move {
                if let Err(err) =
                    irowclaw::runtime::run_with_transport(guest_transport, guest_config_path).await
                {
                    tracing::error!("farm guest runtime failed: {err}");
                }
            });
        }
    }

    let allowed_tools = allowed_tools_for_agent(state, &task.assignee);
    let cap_token = {
        use rand::Rng;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    };
    send_guest_auth_challenge(
        &mut transport,
        &task.assignee,
        &task.context_id,
        &cap_token,
        &allowed_tools,
        state.execution_mode,
        agent_manifest_toml_for(state, &task.assignee),
    )
    .await?;
    let mut transport: Box<dyn common::transport::Transport> =
        Box::new(AuthenticatedTransport::new(transport, cap_token));
    transport
        .send(MessageEnvelope {
            user_id: task.assignee.clone(),
            session_id: task.context_id.clone(),
            msg_id: task.created_at_ms,
            timestamp_ms: now_ms().unwrap_or(task.created_at_ms),
            cap_token: String::new(),
            payload: Some(message_envelope::Payload::AgentTaskRequest(
                common::proto::ironclaw::AgentTaskRequest {
                    task_id: task.id.clone(),
                    context_id: task.context_id.clone(),
                    parent_task_id: task.parent_task_id.clone().unwrap_or_default(),
                    requester: task.requester.clone(),
                    skill: task.skill.clone(),
                    input_json: serde_json::to_string(&task.input)
                        .map_err(|err| IronclawError::new(err.to_string()))?,
                    delegation_depth: u32::from(task.delegation_depth),
                },
            )),
        })
        .await
        .map_err(|err| IronclawError::new(format!("A2A task send failed: {err}")))?;

    let run = async {
        loop {
            let envelope = transport
                .recv()
                .await
                .map_err(|err| IronclawError::new(format!("A2A task receive failed: {err}")))?
                .ok_or_else(|| IronclawError::new("A2A agent transport closed"))?;
            match envelope.payload {
                Some(message_envelope::Payload::ToolCallRequest(request)) => {
                    let (ok, output) = execute_requested_host_tool(
                        state,
                        &task.assignee,
                        &task.context_id,
                        &request,
                        &allowed_tools,
                        None,
                        Some(&task.id),
                    )
                    .await;
                    transport
                        .send(MessageEnvelope {
                            user_id: task.assignee.clone(),
                            session_id: task.context_id.clone(),
                            msg_id: envelope.msg_id,
                            timestamp_ms: now_ms().unwrap_or(0),
                            cap_token: String::new(),
                            payload: Some(message_envelope::Payload::ToolCallResponse(
                                common::proto::ironclaw::ToolCallResponse {
                                    call_id: request.call_id,
                                    ok,
                                    output,
                                },
                            )),
                        })
                        .await
                        .map_err(|err| IronclawError::new(err.to_string()))?;
                }
                Some(message_envelope::Payload::AgentTaskUpdate(update)) => {
                    let next = match update.state.as_str() {
                        "working" => TaskState::Working,
                        "input_required" => TaskState::InputRequired,
                        "completed" => TaskState::Completed,
                        "failed" => TaskState::Failed,
                        "canceled" => TaskState::Canceled,
                        "rejected" => TaskState::Rejected,
                        other => {
                            return Err(IronclawError::new(format!(
                                "invalid A2A task state: {other}"
                            )))
                        }
                    };
                    let mut output = if update.output_json.trim().is_empty() {
                        None
                    } else {
                        Some(serde_json::from_str(&update.output_json).map_err(|err| {
                            IronclawError::new(format!("A2A output JSON failed: {err}"))
                        })?)
                    };
                    if next == TaskState::Failed && !update.error.trim().is_empty() {
                        output = Some(serde_json::json!({"error": update.error}));
                    }
                    state
                        .farm_tasks
                        .transition_with_artifacts(
                            &task.id,
                            next,
                            output,
                            Some(update.artifact_ids),
                            now_ms().unwrap_or(task.updated_at_ms),
                        )
                        .map_err(|err| IronclawError::new(err.to_string()))?;
                    if next.terminal() {
                        return Ok(());
                    }
                }
                Some(message_envelope::Payload::JobTrigger(_)) => {
                    tracing::debug!(agent_id = %task.assignee, "deferred scheduled job during A2A task");
                }
                _ => {}
            }
        }
    };
    // Engineering workflows can contain several sequential child tasks plus local
    // implementation and verification. Keep the watchdog finite, but large enough
    // for a real team delivery rather than a single chat turn.
    let result = tokio::time::timeout(std::time::Duration::from_secs(1_800), run)
        .await
        .map_err(|_| IronclawError::new("A2A task timed out"))?;
    vm_guard.stop().await;
    result
}

async fn ui_index_handler() -> Response {
    ui_file_response("index.html")
}

async fn ui_asset_handler(Path(path): Path<String>) -> Response {
    let path = if path.is_empty() {
        "index.html"
    } else {
        path.as_str()
    };
    ui_file_response(path)
}

#[derive(Deserialize)]
struct SoulGuardDecisionRequest {
    id: i64,
    decision: SoulDecision,
    note: Option<String>,
}

#[derive(Serialize)]
struct SoulGuardDecisionResponse {
    updated: bool,
}

async fn soul_guard_pending_handler(
    State(state): State<AppState>,
) -> Result<Json<Vec<soul_guard::PendingSoulApproval>>, (StatusCode, String)> {
    list_pending_approvals(&state.soul_guard_db_path)
        .map(Json)
        .map_err(|err| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("soul guard list failed: {err}"),
            )
        })
}

async fn soul_guard_decision_handler(
    State(state): State<AppState>,
    Json(request): Json<SoulGuardDecisionRequest>,
) -> Result<Json<SoulGuardDecisionResponse>, (StatusCode, String)> {
    let updated = decide_approval(
        &state.soul_guard_db_path,
        request.id,
        request.decision,
        request.note,
    )
    .map_err(|err| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("soul guard decision failed: {err}"),
        )
    })?;
    Ok(Json(SoulGuardDecisionResponse { updated }))
}

#[derive(Deserialize)]
struct GatewayPairStartRequest {
    node_id: String,
}

#[derive(Serialize)]
struct GatewayPairStartResponse {
    node_id: String,
    otp: String,
    expires_at: i64,
}

#[derive(Deserialize)]
struct GatewayPairVerifyRequest {
    node_id: String,
    otp: String,
}

#[derive(Serialize)]
struct GatewayPairVerifyResponse {
    node_id: String,
    bearer_token: String,
}

#[derive(Deserialize)]
struct GatewayStatusQuery {
    node_id: String,
}

#[derive(Serialize)]
struct GatewayStatusResponse {
    node_id: String,
    status: String,
}

#[derive(Serialize)]
struct WebhookResponse {
    accepted: bool,
}

async fn gateway_pair_start_handler(
    State(state): State<AppState>,
    Json(request): Json<GatewayPairStartRequest>,
) -> Result<Json<GatewayPairStartResponse>, (StatusCode, String)> {
    if !state.host_config.gateway.pairing.enabled {
        return Err((
            StatusCode::FORBIDDEN,
            "gateway pairing disabled".to_string(),
        ));
    }
    enforce_rate_limit(&state, &request.node_id, "gateway", 0).map_err(|_| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            "429 too many requests".to_string(),
        )
    })?;
    let now_seconds =
        now_epoch_seconds().map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let conn = open_security_db(&state)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let manager = PairingManager::new(state.host_config.gateway.pairing.otp_expiry_seconds);
    let otp = manager
        .begin_pairing(&conn, &request.node_id, now_seconds)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err))?;
    let expires_at =
        now_seconds.saturating_add(state.host_config.gateway.pairing.otp_expiry_seconds as i64);
    Ok(Json(GatewayPairStartResponse {
        node_id: request.node_id,
        otp,
        expires_at,
    }))
}

async fn gateway_pair_verify_handler(
    State(state): State<AppState>,
    Json(request): Json<GatewayPairVerifyRequest>,
) -> Result<Json<GatewayPairVerifyResponse>, (StatusCode, String)> {
    if !state.host_config.gateway.pairing.enabled {
        return Err((
            StatusCode::FORBIDDEN,
            "gateway pairing disabled".to_string(),
        ));
    }
    enforce_rate_limit(&state, &request.node_id, "gateway", 0).map_err(|_| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            "429 too many requests".to_string(),
        )
    })?;
    let now_seconds =
        now_epoch_seconds().map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let conn = open_security_db(&state)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let manager = PairingManager::new(state.host_config.gateway.pairing.otp_expiry_seconds);
    let token = manager
        .verify_otp_and_issue_bearer(&conn, &request.node_id, &request.otp, now_seconds)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err))?;
    let Some(bearer_token) = token else {
        return Err((StatusCode::UNAUTHORIZED, "invalid otp".to_string()));
    };
    Ok(Json(GatewayPairVerifyResponse {
        node_id: request.node_id,
        bearer_token,
    }))
}

async fn gateway_status_handler(
    State(state): State<AppState>,
    Query(query): Query<GatewayStatusQuery>,
) -> Result<Json<GatewayStatusResponse>, (StatusCode, String)> {
    enforce_rate_limit(&state, &query.node_id, "gateway", 0).map_err(|_| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            "429 too many requests".to_string(),
        )
    })?;
    let now_seconds =
        now_epoch_seconds().map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let conn = open_security_db(&state)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let manager = PairingManager::new(state.host_config.gateway.pairing.otp_expiry_seconds);
    let status = manager
        .current_status(&conn, &query.node_id, now_seconds)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err))?;
    Ok(Json(GatewayStatusResponse {
        node_id: query.node_id,
        status: pairing_status_name(status).to_string(),
    }))
}

async fn webhook_handler(
    Path(channel): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, (StatusCode, String)> {
    ensure_channel_allowed(&state, &channel)
        .map_err(|_| (StatusCode::FORBIDDEN, "channel not allowed".to_string()))?;

    let secret = headers
        .get("x-webhook-secret")
        .and_then(|value| value.to_str().ok());
    if !validate_webhook_secret(&state.host_config.security.webhook_secret, secret) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "invalid webhook secret".to_string(),
        ));
    }

    enforce_rate_limit(&state, "webhook", &channel, 0).map_err(|_| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            "429 too many requests".to_string(),
        )
    })?;

    match channel.as_str() {
        "slack" => handle_slack_webhook(&body, &headers).await,
        _ => Ok(Json(WebhookResponse { accepted: true }).into_response()),
    }
}

async fn handle_slack_webhook(
    body: &str,
    headers: &HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    if let Ok(url_verification) = serde_json::from_str::<SlackUrlVerification>(body) {
        if url_verification.type_ == "url_verification" {
            return Ok(Json(serde_json::json!({
                "challenge": url_verification.challenge
            }))
            .into_response());
        }
    }

    match parse_slack_message(body) {
        Ok(payload) => {
            let user_id = payload
                .user_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let text = payload.text.clone().unwrap_or_default();

            tracing::info!(
                "slack webhook: user_id={} channel={} text={}",
                user_id,
                payload.channel_id.as_deref().unwrap_or("unknown"),
                text
            );

            if text.is_empty() {
                return Ok(Json(SlackResponse::new("No message text provided")).into_response());
            }

            Ok(Json(SlackResponse::new(&format!(
                "Received: {}. Processing via agent...",
                text
            )))
            .into_response())
        }
        Err(e) => {
            tracing::warn!("slack parse error: {}", e);
            Ok(Json(SlackResponse::new("Error processing message")).into_response())
        }
    }
}

fn run_gateway_cli(config: &HostConfig, command: GatewayCommand) -> Result<(), IronclawError> {
    if !config.gateway.pairing.enabled {
        return Err(IronclawError::new("gateway pairing disabled in config"));
    }
    let db_path = security_db_path(&config.storage.users_root);
    let conn = open_security_db_path(&db_path)?;
    let now_seconds = now_epoch_seconds()?;
    let manager = PairingManager::new(config.gateway.pairing.otp_expiry_seconds);

    match command {
        GatewayCommand::Pair { node_id, otp } => {
            if let Some(value) = otp {
                let token = manager
                    .verify_otp_and_issue_bearer(&conn, &node_id, &value, now_seconds)
                    .map_err(IronclawError::new)?;
                if let Some(bearer_token) = token {
                    println!("node_id={node_id}");
                    println!("status=paired");
                    println!("bearer_token={bearer_token}");
                    return Ok(());
                }
                return Err(IronclawError::new("invalid or expired otp"));
            }
            let issued = manager
                .begin_pairing(&conn, &node_id, now_seconds)
                .map_err(IronclawError::new)?;
            let expires_at =
                now_seconds.saturating_add(config.gateway.pairing.otp_expiry_seconds as i64);
            println!("node_id={node_id}");
            println!("status=pairing");
            println!("otp={issued}");
            println!("expires_at={expires_at}");
            Ok(())
        }
        GatewayCommand::Status { node_id } => {
            let status = manager
                .current_status(&conn, &node_id, now_seconds)
                .map_err(IronclawError::new)?;
            println!("node_id={node_id}");
            println!("status={}", pairing_status_name(status));
            Ok(())
        }
    }
}

fn pairing_status_name(status: security::pairing::PairingStatus) -> &'static str {
    match status {
        security::pairing::PairingStatus::Unpaired => "unpaired",
        security::pairing::PairingStatus::Pairing => "pairing",
        security::pairing::PairingStatus::Paired => "paired",
    }
}

fn ensure_channel_allowed(state: &AppState, channel: &str) -> Result<(), IronclawError> {
    if channel_allowed(&state.host_config.security.allowed_channels, channel) {
        return Ok(());
    }
    Err(IronclawError::new(format!("channel denied: {channel}")))
}

fn ensure_gateway_request_authorized(
    state: &AppState,
    query: &WsQuery,
    headers: Option<&HeaderMap>,
) -> Result<(), IronclawError> {
    if !state.host_config.gateway.pairing.enabled {
        return Ok(());
    }
    let node_id = query
        .node_id
        .as_ref()
        .map(|value| value.as_str())
        .or_else(|| {
            headers.and_then(|all| {
                all.get("x-gateway-node-id")
                    .and_then(|value| value.to_str().ok())
            })
        })
        .ok_or_else(|| IronclawError::new("missing gateway node id"))?;

    let bearer = headers.and_then(extract_bearer_token);
    let Some(token) = bearer else {
        return Err(IronclawError::new("missing bearer token"));
    };

    let conn = open_security_db(state)?;
    let manager = PairingManager::new(state.host_config.gateway.pairing.otp_expiry_seconds);
    let valid = manager
        .validate_bearer(&conn, node_id, token)
        .map_err(IronclawError::new)?;
    if valid {
        return Ok(());
    }
    Err(IronclawError::new("invalid bearer token"))
}

fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get("authorization")?.to_str().ok()?;
    let trimmed = value.trim();
    trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
}

fn enforce_rate_limit(
    state: &AppState,
    user_id: &str,
    channel: &str,
    request_cost: u64,
) -> Result<(), IronclawError> {
    let conn = open_security_db(state)?;
    let limiter = RateLimiter::new(RateLimitConfig {
        requests_per_minute: state.host_config.security.rate_limit.requests_per_minute,
        requests_per_hour: state.host_config.security.rate_limit.requests_per_hour,
        cost_per_day_cap: state.host_config.security.rate_limit.cost_per_day_cap,
    });
    let now_seconds = now_epoch_seconds()?;
    let decision = limiter
        .check_and_record(&conn, user_id, channel, now_seconds, request_cost)
        .map_err(IronclawError::new)?;
    if decision.allowed {
        return Ok(());
    }
    let reason = decision
        .reason
        .unwrap_or_else(|| "rate limit exceeded".to_string());
    Err(IronclawError::new(format!(
        "{reason}; retry_after_seconds={}",
        decision.retry_after_seconds
    )))
}

fn ui_file_response(path: &str) -> Response {
    match UI_DIR.get_file(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let mut response = Response::new(file.contents().into());
            response.headers_mut().insert(
                "content-type",
                HeaderValue::from_str(mime.as_ref())
                    .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            );
            response
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

fn load_host_config_from_path(path: &StdPath) -> Result<HostConfig, IronclawError> {
    if path.exists() {
        let contents = std::fs::read_to_string(path)
            .map_err(|err| IronclawError::new(format!("config read failed: {err}")))?;
        toml::from_str(&contents)
            .map_err(|err| IronclawError::new(format!("config parse failed: {err}")))
    } else {
        let users_root = PathBuf::from("data/users");
        Ok(HostConfig::default_for_local(users_root))
    }
}

fn host_config_path() -> Result<PathBuf, IronclawError> {
    if let Ok(path) = std::env::var("IRONCLAWD_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    let home = dirs::home_dir().ok_or_else(|| IronclawError::new("home dir missing"))?;
    Ok(home.join(".config/ironclaw/ironclawd.toml"))
}

fn guest_config_path() -> PathBuf {
    std::env::var("IROWCLAW_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/mnt/brain/config/irowclaw.toml"))
}

fn brave_api_key_for_guest() -> String {
    std::env::var("BRAVE_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

fn brain_ext4_path(root: &StdPath, user_id: &str) -> Result<PathBuf, IronclawError> {
    if user_id.is_empty()
        || !user_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(IronclawError::new(
            "user id may contain only ASCII letters, digits, '-' and '_'",
        ));
    }
    let user_dir = root.join(user_id);
    std::fs::create_dir_all(&user_dir)
        .map_err(|err| IronclawError::new(format!("create user dir failed: {err}")))?;
    Ok(user_dir.join("brain.ext4"))
}

fn brain_db_path(root: &StdPath, user_id: &str) -> Result<PathBuf, IronclawError> {
    let db_dir = root.join(user_id).join("guest").join("db");
    std::fs::create_dir_all(&db_dir)
        .map_err(|err| IronclawError::new(format!("create memory db dir failed: {err}")))?;
    Ok(db_dir.join("ironclaw.db"))
}

fn security_db_path(root: &StdPath) -> PathBuf {
    root.join("security.db")
}

fn open_memory_db(state: &AppState, user_id: &str) -> Result<Connection, IronclawError> {
    let db_path = brain_db_path(&state.host_config.storage.users_root, user_id)?;
    let conn = Connection::open(db_path)
        .map_err(|err| IronclawError::new(format!("memory db open failed: {err}")))?;
    initialize_schema(&conn)
        .map_err(|err| IronclawError::new(format!("memory db schema failed: {err}")))?;
    Ok(conn)
}

fn open_security_db(state: &AppState) -> Result<Connection, IronclawError> {
    open_security_db_path(&state.security_db_path)
}

fn open_security_db_path(path: &StdPath) -> Result<Connection, IronclawError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| IronclawError::new(format!("create security db dir failed: {err}")))?;
    }
    let conn = Connection::open(path)
        .map_err(|err| IronclawError::new(format!("security db open failed: {err}")))?;
    security::initialize_schema(&conn).map_err(IronclawError::new)?;
    Ok(conn)
}

fn load_memory_block(
    state: &AppState,
    user_id: &str,
    query: &str,
    budget_chars: usize,
) -> Result<String, IronclawError> {
    let conn = open_memory_db(state, user_id)?;
    let now = now_ms().unwrap_or(0);
    let memories = retrieve_memories(&conn, user_id, query, MEMORY_RETRIEVAL_LIMIT, now)
        .map_err(|err| IronclawError::new(format!("memory retrieve failed: {err}")))?;
    Ok(build_memory_block(&memories, budget_chars))
}

fn summarize_telegram_session_memory(
    state: &AppState,
    session: &TelegramSession,
) -> Result<(), IronclawError> {
    let conn = open_memory_db(state, &session.user_id)?;
    let user_messages = session.user_messages();
    let _ = maybe_summarize_session(
        &conn,
        &session.user_id,
        &session.session_id,
        &user_messages,
        now_ms().unwrap_or(0),
    )
    .map_err(|err| IronclawError::new(format!("memory summarize failed: {err}")))?;
    Ok(())
}

enum MemoryCommand {
    Remember(String),
    Pins,
    Forget(String),
}

fn parse_memory_command(input: &str) -> Option<MemoryCommand> {
    let trimmed = input.trim();
    if let Some(rest) = trimmed.strip_prefix("remember ") {
        if !rest.trim().is_empty() {
            return Some(MemoryCommand::Remember(rest.trim().to_string()));
        }
    }
    if trimmed == "pins" {
        return Some(MemoryCommand::Pins);
    }
    if let Some(rest) = trimmed.strip_prefix("forget ") {
        if !rest.trim().is_empty() {
            return Some(MemoryCommand::Forget(rest.trim().to_string()));
        }
    }
    None
}

fn execute_memory_command(
    state: &AppState,
    user_id: &str,
    session_id: &str,
    command: MemoryCommand,
) -> Result<String, IronclawError> {
    let conn = open_memory_db(state, user_id)?;
    match command {
        MemoryCommand::Remember(text) => {
            let memory_id = upsert_memory(
                &conn,
                now_ms().unwrap_or(0),
                &NewMemory {
                    user_id: user_id.to_string(),
                    importance: 90,
                    pinned: true,
                    kind: "manual".to_string(),
                    text: redact_secrets(&text),
                    tags_json: "[\"manual\",\"pinned\"]".to_string(),
                    source_json: serde_json::json!({
                        "source": "telegram_command",
                        "session_id": session_id,
                    })
                    .to_string(),
                },
            )
            .map_err(|err| IronclawError::new(format!("remember failed: {err}")))?;
            Ok(format!("remembered pinned memory id={memory_id}"))
        }
        MemoryCommand::Pins => {
            let items = list_pinned_memories(&conn, user_id, 25)
                .map_err(|err| IronclawError::new(format!("pins failed: {err}")))?;
            if items.is_empty() {
                return Ok("no pinned memories".to_string());
            }
            let mut lines = Vec::new();
            for item in items {
                lines.push(format!("{}: {}", item.id, item.text));
            }
            Ok(lines.join("\n"))
        }
        MemoryCommand::Forget(target) => {
            if let Ok(id) = target.parse::<i64>() {
                let removed = forget_memory_by_id(&conn, user_id, id)
                    .map_err(|err| IronclawError::new(format!("forget failed: {err}")))?;
                if removed {
                    Ok(format!("forgot memory id={id}"))
                } else {
                    Ok(format!("no memory matched id={id}"))
                }
            } else {
                let removed = forget_memories_by_query(&conn, user_id, &target, 10)
                    .map_err(|err| IronclawError::new(format!("forget failed: {err}")))?;
                Ok(format!("forgot {removed} memories"))
            }
        }
    }
}

fn now_ms() -> Result<u64, IronclawError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| IronclawError::new(format!("time error: {err}")))
        .map(|duration| duration.as_millis() as u64)
}

fn now_epoch_seconds() -> Result<i64, IronclawError> {
    now_ms().map(|value| (value / 1000) as i64)
}

fn decode_websocket_upload(data: &[u8]) -> Result<UploadedFile, IronclawError> {
    let envelope = MessageEnvelope::decode(data)
        .map_err(|err| IronclawError::new(format!("invalid file upload envelope: {err}")))?;
    let Some(message_envelope::Payload::UploadedFile(mut upload)) = envelope.payload else {
        return Err(IronclawError::new(
            "binary WebSocket messages must contain an uploaded file",
        ));
    };
    validate_inbound_upload(&upload)?;
    if upload.mime_type.trim().is_empty() {
        upload.mime_type = "application/octet-stream".to_string();
    }
    Ok(upload)
}

fn validate_inbound_upload(upload: &UploadedFile) -> Result<(), IronclawError> {
    if upload.data.len() > MAX_INBOUND_FILE_BYTES {
        return Err(IronclawError::new(format!(
            "file exceeds {} byte upload limit",
            MAX_INBOUND_FILE_BYTES
        )));
    }
    let valid_filename = StdPath::new(&upload.filename)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| !name.is_empty() && name != "." && name != "..");
    if !valid_filename {
        return Err(IronclawError::new("uploaded filename is invalid"));
    }
    Ok(())
}

fn ws_text_to_guest_payload(text: &str, next_msg_id: u64) -> (message_envelope::Payload, u64) {
    if let Some(rest) = text.strip_prefix("!toolcall ") {
        let mut parts = rest.splitn(2, '\n');
        let tool = parts.next().unwrap_or("").trim().to_string();
        let input = parts.next().unwrap_or("").to_string();
        if !tool.is_empty() {
            return (
                message_envelope::Payload::ToolCallRequest(
                    common::proto::ironclaw::ToolCallRequest {
                        call_id: next_msg_id,
                        tool,
                        input,
                    },
                ),
                next_msg_id.saturating_add(1),
            );
        }
    }

    (
        message_envelope::Payload::UserMessage(common::proto::ironclaw::UserMessage {
            text: text.to_string(),
        }),
        next_msg_id.saturating_add(1),
    )
}

async fn host_plan_tool_response(
    state: &AppState,
    user_id: &str,
    raw_request: &str,
    allowed_tools: &[String],
    history: Option<&[ConversationMessage]>,
    allow_direct_a2a_shortcut: bool,
) -> Result<String, String> {
    let request = decode_host_plan_request(raw_request);
    if request.observations.is_empty() {
        if allow_direct_a2a_shortcut {
            if let Some(plan) = deterministic_a2a_plan(
                &state.farm_registry,
                user_id,
                &request.user_text,
                allowed_tools,
            ) {
                return tool_plan_to_json(&plan);
            }
        }
        if let Some(plan) = deterministic_guest_tools_plan(&request.user_text, allowed_tools) {
            return tool_plan_to_json(&plan);
        }
    } else if let Some(plan) = deterministic_await_a2a_plan(&request.observations, allowed_tools) {
        return tool_plan_to_json(&plan);
    }

    let mut memory_block = load_memory_block(
        state,
        user_id,
        &request.user_text,
        MEMORY_PROMPT_BUDGET_CHARS,
    )
    .map_err(|err| format!("memory retrieval failed: {err}"))?;
    let organization = agent_organization_context(&state.farm_registry, user_id);
    if !organization.is_empty() {
        memory_block.push_str("\n\n");
        memory_block.push_str(&organization);
    }
    const HOST_PLANNER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
    let planning = state.llm_client.plan_tool_or_answer(
        &request.user_text,
        allowed_tools,
        Some(memory_block.as_str()),
        history,
        Some(&request.observations),
    );
    let plan = match tokio::time::timeout(HOST_PLANNER_TIMEOUT, planning).await {
        Err(_) => {
            return Err(format!(
                "host planner timed out after {} seconds",
                HOST_PLANNER_TIMEOUT.as_secs()
            ));
        }
        Ok(result) => match result {
            Ok(plan) => plan,
            Err(err) => {
                // GuestTools mode should remain usable even when no LLM key is configured.
                // Fall back to a deterministic stub answer instead of failing the whole guest loop.
                let msg = err.to_string();
                if msg.contains("missing openai_api_key") {
                    ToolPlan::Answer {
                        text: format!("stub: {}", request.user_text.trim()),
                    }
                } else {
                    return Err(format!("host plan failed: {err}"));
                }
            }
        },
    };

    tool_plan_to_json(&plan)
}

#[derive(Deserialize)]
struct McpCallInput {
    server: String,
    tool: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

#[derive(Deserialize)]
struct DelegateTaskInput {
    assignee: String,
    skill: String,
    #[serde(default)]
    input: serde_json::Value,
}

#[derive(Deserialize)]
struct AwaitTaskInput {
    task_id: String,
    #[serde(default = "default_await_task_timeout_seconds")]
    timeout_seconds: u64,
}

fn default_await_task_timeout_seconds() -> u64 {
    300
}

async fn execute_requested_host_tool(
    state: &AppState,
    user_id: &str,
    session_id: &str,
    request: &common::proto::ironclaw::ToolCallRequest,
    host_allowed_tools: &[String],
    history: Option<&[ConversationMessage]>,
    active_task_id: Option<&str>,
) -> (bool, String) {
    let result = if request.tool == "host_plan" {
        host_plan_tool_response(
            state,
            user_id,
            &request.input,
            host_allowed_tools,
            history,
            active_task_id.is_none(),
        )
        .await
    } else if request.tool == "mcp_call" {
        let input = serde_json::from_str::<McpCallInput>(&request.input)
            .map_err(|err| format!("MCP call input is invalid: {err}"));
        match input {
            Ok(input) => state
                .mcp_gateway
                .call(
                    user_id,
                    &input.server,
                    &input.tool,
                    input.arguments,
                    &format!("{session_id}-{}", request.call_id),
                )
                .await
                .and_then(|value| {
                    serde_json::to_string(&value)
                        .map_err(|err| format!("MCP result encode failed: {err}"))
                }),
            Err(err) => Err(err),
        }
    } else if request.tool == "delegate_task" {
        match active_task_id {
            Some(parent_task_id) => {
                create_delegated_task(state, user_id, parent_task_id, &request.input)
                    .await
                    .and_then(|task| {
                        serde_json::to_string(&task)
                            .map_err(|err| format!("delegated task encode failed: {err}"))
                    })
            }
            None => create_direct_delegated_task(state, user_id, &request.input).and_then(|task| {
                serde_json::to_string(&task)
                    .map_err(|err| format!("delegated task encode failed: {err}"))
            }),
        }
    } else if request.tool == "await_task" {
        match active_task_id {
            Some(parent_task_id) => {
                await_delegated_task(state, user_id, parent_task_id, &request.input).await
            }
            None => await_direct_delegated_task(state, user_id, &request.input).await,
        }
    } else {
        run_host_tool(
            host_allowed_tools,
            &state.host_config.security.network.allowed_domains,
            user_id,
            &request.tool,
            &request.input,
        )
        .await
    };
    finalize_requested_host_tool_response(&request.tool, result)
}

fn finalize_requested_host_tool_response(
    tool: &str,
    result: Result<String, String>,
) -> (bool, String) {
    match result {
        // `host_plan` is already a bounded model response and must remain valid JSON.
        // Truncating it mid-string makes the guest reject otherwise valid plans.
        Ok(output) if tool == "host_plan" => (true, output),
        Ok(output) => (true, truncate_tool_output(&output)),
        Err(error) => (false, truncate_tool_output(&error)),
    }
}

fn create_direct_delegated_task(
    state: &AppState,
    requester: &str,
    raw_input: &str,
) -> Result<FarmTask, String> {
    let input: DelegateTaskInput = serde_json::from_str(raw_input)
        .map_err(|err| format!("delegate task input is invalid: {err}"))?;
    create_channel_farm_task(state, requester, &input.assignee, &input.skill, input.input)
        .map_err(|err| err.to_string())
}

async fn await_direct_delegated_task(
    state: &AppState,
    requester: &str,
    raw_input: &str,
) -> Result<String, String> {
    let input: AwaitTaskInput = serde_json::from_str(raw_input)
        .map_err(|err| format!("await task input is invalid: {err}"))?;
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(input.timeout_seconds.clamp(1, 300));
    loop {
        let task = state
            .farm_tasks
            .get(&input.task_id)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("delegated task not found: {}", input.task_id))?;
        if task.requester != requester || task.parent_task_id.is_some() {
            return Err("agent may await only its own direct delegated task".to_string());
        }
        if task.state.terminal() {
            return serde_json::to_string(&task)
                .map_err(|err| format!("delegated task encode failed: {err}"));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for delegated task {}", task.id));
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

async fn await_delegated_task(
    state: &AppState,
    requester: &str,
    parent_task_id: &str,
    raw_input: &str,
) -> Result<String, String> {
    let input: AwaitTaskInput = serde_json::from_str(raw_input)
        .map_err(|err| format!("await task input is invalid: {err}"))?;
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(input.timeout_seconds.clamp(1, 300));
    loop {
        let task = state
            .farm_tasks
            .get(&input.task_id)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("child task not found: {}", input.task_id))?;
        if task.requester != requester || task.parent_task_id.as_deref() != Some(parent_task_id) {
            return Err("agent may await only its own direct child task".to_string());
        }
        if task.state.terminal() {
            return serde_json::to_string(&task)
                .map_err(|err| format!("child task encode failed: {err}"));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for child task {}", task.id));
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

async fn create_delegated_task(
    state: &AppState,
    requester: &str,
    parent_task_id: &str,
    raw_input: &str,
) -> Result<FarmTask, String> {
    let input: DelegateTaskInput = serde_json::from_str(raw_input)
        .map_err(|err| format!("delegate task input is invalid: {err}"))?;
    let capability: farm::CapabilityUri = format!("agent://{}/{}", input.assignee, input.skill)
        .parse()
        .map_err(|err| format!("delegate capability is invalid: {err}"))?;
    let allowed = state
        .farm_registry
        .capabilities_for(requester)
        .map_err(|err| err.to_string())?
        .into_iter()
        .any(|candidate| candidate.uri == capability);
    if !allowed {
        return Err(format!(
            "agent {requester} may not delegate {} to {}",
            input.skill, input.assignee
        ));
    }
    let parent = state
        .farm_tasks
        .get(parent_task_id)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| format!("parent task not found: {parent_task_id}"))?;
    if parent.assignee != requester || parent.state.terminal() {
        return Err("requester does not own an active parent task".to_string());
    }
    let depth = parent.delegation_depth.saturating_add(1);
    let requester_record = state
        .farm_registry
        .get(requester)
        .ok_or_else(|| format!("unknown requester: {requester}"))?;
    if depth > requester_record.manifest.a2a.max_delegation_depth {
        return Err("delegation depth limit exceeded".to_string());
    }
    let assignee_record = state
        .farm_registry
        .get(&input.assignee)
        .ok_or_else(|| format!("unknown assignee: {}", input.assignee))?;
    let active_tasks = state
        .farm_tasks
        .list()
        .map_err(|err| err.to_string())?
        .into_iter()
        .filter(|task| task.assignee == input.assignee && !task.state.terminal())
        .count();
    if active_tasks >= usize::from(assignee_record.manifest.a2a.max_concurrent_tasks) {
        return Err(format!(
            "agent {} has reached its concurrent task limit",
            input.assignee
        ));
    }
    let now = now_ms().map_err(|err| err.to_string())?;
    let task = FarmTask {
        id: format!("task-{now}-{:08x}", rand::random::<u32>()),
        context_id: parent.context_id,
        parent_task_id: Some(parent_task_id.to_string()),
        requester: requester.to_string(),
        assignee: input.assignee,
        skill: input.skill,
        state: TaskState::Submitted,
        input: input.input,
        output: None,
        artifact_ids: Vec::new(),
        delegation_depth: depth,
        created_at_ms: now,
        updated_at_ms: now,
    };
    state
        .farm_tasks
        .insert(task.clone())
        .map_err(|err| err.to_string())?;
    spawn_farm_task_dispatch(state.clone(), task.clone());
    Ok(task)
}

#[derive(Debug, Deserialize)]
struct HostPlanRequest {
    version: u8,
    user_text: String,
    #[serde(default)]
    observations: Vec<ToolLoopObservation>,
}

fn decode_host_plan_request(raw: &str) -> HostPlanRequest {
    match serde_json::from_str::<HostPlanRequest>(raw) {
        Ok(request) if request.version == 1 && !request.user_text.trim().is_empty() => request,
        _ => HostPlanRequest {
            version: 0,
            user_text: raw.to_string(),
            observations: Vec::new(),
        },
    }
}

fn agent_organization_context(registry: &farm::FarmRegistry, agent_id: &str) -> String {
    let Some(agent) = registry.get(agent_id) else {
        return String::new();
    };
    let mut lines = vec![format!(
        "Organization chart (authoritative): You are {} ({}) with agent id `{}`.",
        agent.manifest.name, agent.manifest.role, agent.manifest.id
    )];
    for record in registry.agents() {
        let manager = record
            .manifest
            .reports_to
            .as_deref()
            .and_then(|id| registry.get(id))
            .map(|manager| {
                format!(
                    "; reports to {} (`{}`)",
                    manager.manifest.name, manager.manifest.id
                )
            })
            .unwrap_or_default();
        lines.push(format!(
            "- {} — {} (`{}`){manager}",
            record.manifest.name, record.manifest.role, record.manifest.id
        ));
    }
    lines.push("Authorized A2A routes available to you:".to_string());
    for target_id in &agent.manifest.a2a.delegate_to {
        let Some(target) = registry.get(target_id) else {
            continue;
        };
        for skill in &target.manifest.skills {
            lines.push(format!(
                "- Ask {} (`{}`) using delegate_task with assignee=`{}`, skill=`{}`: {}",
                target.manifest.name,
                target.manifest.id,
                target.manifest.id,
                skill.id,
                skill.description
            ));
        }
    }
    lines.push(
        "When the user asks you to ask, contact, or check with a teammate, call delegate_task and then await_task; do not claim that you cannot reach them. For a request about something the teammate remembers (including a secret or fact previously told to them), set input.purpose=`authorized_memory_request` and put the complete request in input.request. Private memory is disclosed only through this explicit authorized A2A task."
            .to_string(),
    );
    lines.join("\n")
}

fn deterministic_a2a_plan(
    registry: &farm::FarmRegistry,
    requester: &str,
    user_text: &str,
    allowed_tools: &[String],
) -> Option<ToolPlan> {
    if !allowed_tools.iter().any(|tool| tool == "delegate_task") {
        return None;
    }
    let normalized = user_text.to_ascii_lowercase();
    let requests_contact = ["ask ", "contact ", "message ", "check with ", "talk to "]
        .iter()
        .any(|phrase| normalized.contains(phrase));
    if !requests_contact {
        return None;
    }
    let requester_record = registry.get(requester)?;
    for target_id in &requester_record.manifest.a2a.delegate_to {
        let target = registry.get(target_id)?;
        let mentions_target = normalized.contains(&target.manifest.name.to_ascii_lowercase())
            || normalized.contains(&target.manifest.id.to_ascii_lowercase());
        if !mentions_target {
            continue;
        }
        let memory_request = [
            "secret", "remember", "memory", "told", "knows", "know ", "fact",
        ]
        .iter()
        .any(|word| normalized.contains(word));
        // Memory hand-offs have a strict host-side purpose marker, so make this common
        // natural-language route deterministic. Other work remains with the planner so it
        // can select the best specialization from the full organization context.
        if !memory_request {
            return None;
        }
        let skill = target.manifest.skills.first()?;
        let mut input = serde_json::json!({"request": user_text});
        input["purpose"] = serde_json::Value::String("authorized_memory_request".to_string());
        return Some(ToolPlan::Tool {
            tool: "delegate_task".to_string(),
            input: serde_json::json!({
                "assignee": target.manifest.id,
                "skill": skill.id,
                "input": input,
            })
            .to_string(),
        });
    }
    None
}

fn deterministic_await_a2a_plan(
    observations: &[ToolLoopObservation],
    allowed_tools: &[String],
) -> Option<ToolPlan> {
    if !allowed_tools.iter().any(|tool| tool == "await_task") {
        return None;
    }
    let observation = observations.last()?;
    if observation.tool != "delegate_task" || !observation.ok {
        return None;
    }
    let task: FarmTask = serde_json::from_str(&observation.output).ok()?;
    Some(ToolPlan::Tool {
        tool: "await_task".to_string(),
        input: serde_json::json!({"task_id": task.id, "timeout_seconds": 300}).to_string(),
    })
}

fn deterministic_guest_tools_plan(user_text: &str, allowed_tools: &[String]) -> Option<ToolPlan> {
    let text = user_text.trim();

    // Heuristic: when the user explicitly asks to run a shell command, force bash.
    // This avoids the planner "answering" instead of executing.
    if let Some(cmd) = text.strip_prefix("run ") {
        if allowed_tools.iter().any(|tool| tool == "bash") {
            let cmd = cmd.trim();
            if !cmd.is_empty() {
                return Some(ToolPlan::Tool {
                    tool: "bash".to_string(),
                    input: cmd.to_string(),
                });
            }
        }
    }

    // Also handle common direct shell commands.
    for prefix in ["cat ", "ls", "pwd", "whoami", "uname"] {
        if text.starts_with(prefix) {
            if allowed_tools.iter().any(|tool| tool == "bash") {
                return Some(ToolPlan::Tool {
                    tool: "bash".to_string(),
                    input: text.to_string(),
                });
            }
        }
    }

    let text = user_text.trim_start();
    let rest = text.strip_prefix("TOOLTEST ")?;

    if let Some(write_rest) = rest.strip_prefix("WRITE ") {
        if !allowed_tools.iter().any(|tool| tool == "file_write") {
            return Some(ToolPlan::Answer {
                text: "tooltest write unavailable: file_write is not allowed".to_string(),
            });
        }

        let mut parts = write_rest.splitn(2, '\n');
        let path = parts.next().unwrap_or("").trim();
        if path.is_empty() {
            return Some(ToolPlan::Answer {
                text: "tooltest write failed: missing path".to_string(),
            });
        }
        let contents = parts.next().unwrap_or("");
        return Some(ToolPlan::Tool {
            tool: "file_write".to_string(),
            input: format!("{path}\n{contents}"),
        });
    }

    if let Some(path) = rest.strip_prefix("READ ") {
        if !allowed_tools.iter().any(|tool| tool == "file_read") {
            return Some(ToolPlan::Answer {
                text: "tooltest read unavailable: file_read is not allowed".to_string(),
            });
        }
        let path = path.trim();
        if path.is_empty() {
            return Some(ToolPlan::Answer {
                text: "tooltest read failed: missing path".to_string(),
            });
        }
        return Some(ToolPlan::Tool {
            tool: "file_read".to_string(),
            input: path.to_string(),
        });
    }

    Some(ToolPlan::Answer {
        text: "tooltest supports TOOLTEST WRITE <path>\\n<contents> or TOOLTEST READ <path>"
            .to_string(),
    })
}

fn tool_plan_to_json(plan: &ToolPlan) -> Result<String, String> {
    let value = match plan {
        ToolPlan::Answer { text } => serde_json::json!({
            "action": "answer",
            "text": text,
        }),
        ToolPlan::Tool { tool, input } => serde_json::json!({
            "action": "tool",
            "tool": tool,
            "input": input,
        }),
    };
    Ok(value.to_string())
}

async fn run_host_turn(
    state: &AppState,
    user_id: &str,
    allowed_tools: &[String],
    user_text: &str,
    history: Option<&[ConversationMessage]>,
) -> Result<String, IronclawError> {
    let memory_block = load_memory_block(state, user_id, user_text, MEMORY_PROMPT_BUDGET_CHARS)?;
    let plan = state
        .llm_client
        .plan_tool_or_answer(
            user_text,
            allowed_tools,
            Some(memory_block.as_str()),
            history,
            None,
        )
        .await
        .map_err(|err| IronclawError::new(format!("tool planning failed: {err}")))?;

    match plan {
        ToolPlan::Answer { text } => {
            tracing::info!("tool plan action=answer");
            Ok(text)
        }
        ToolPlan::Tool { tool, input } => {
            tracing::info!("tool plan action=tool tool={tool}");
            let tool_result = run_host_tool(
                allowed_tools,
                &state.host_config.security.network.allowed_domains,
                user_id,
                &tool,
                &input,
            )
            .await;
            let (ok, raw_output) = match tool_result {
                Ok(output) => (true, output),
                Err(output) => (false, output),
            };
            let output = truncate_tool_output(&raw_output);
            tracing::info!(
                "tool execution tool={} ok={} output_len={}",
                tool,
                ok,
                output.len()
            );
            state
                .llm_client
                .finalize_with_tool_output(
                    user_text,
                    &tool,
                    &input,
                    ok,
                    &output,
                    Some(memory_block.as_str()),
                    history,
                )
                .await
                .map_err(|err| IronclawError::new(format!("tool finalize failed: {err}")))
        }
    }
}

fn should_enable_telegram(config: &HostConfig) -> bool {
    if std::env::args().any(|arg| arg == "--telegram") {
        return true;
    }
    config.telegram.enabled
}

#[derive(Clone)]
struct TelegramSettings {
    bot_token: String,
    owner_chat_id: i64,
    poll_timeout_seconds: u64,
    offset_file: PathBuf,
}

impl TelegramSettings {
    fn from_config(config: &HostConfig) -> Result<Self, IronclawError> {
        let bot_token = std::env::var("TELEGRAM_BOT_TOKEN")
            .ok()
            .or_else(|| config.telegram.bot_token.clone())
            .ok_or_else(|| IronclawError::new("telegram enabled but bot token is missing"))?;
        let owner_chat_id = std::env::var("OWNER_TELEGRAM_CHAT_ID")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .or(config.telegram.owner_chat_id)
            .ok_or_else(|| {
                IronclawError::new("telegram enabled but owner telegram chat id is missing")
            })?;
        let poll_timeout_seconds = if config.telegram.poll_timeout_seconds == 0 {
            30
        } else {
            config.telegram.poll_timeout_seconds
        };
        Ok(Self {
            bot_token,
            owner_chat_id,
            poll_timeout_seconds,
            offset_file: PathBuf::from("data/telegram.offset"),
        })
    }
}

#[derive(Clone)]
struct TelegramClient {
    bot_token: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct TelegramApiResponse<T> {
    ok: bool,
    result: T,
}

#[derive(Clone, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Clone, Deserialize)]
struct TelegramMessage {
    text: Option<String>,
    caption: Option<String>,
    document: Option<TelegramDocument>,
    chat: TelegramChat,
}

#[derive(Clone, Deserialize)]
struct TelegramDocument {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
    file_size: Option<u64>,
}

#[derive(Clone, Deserialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Deserialize)]
struct TelegramFile {
    file_path: Option<String>,
}

#[derive(Serialize)]
struct TelegramGetUpdatesRequest<'a> {
    offset: i64,
    timeout: u64,
    allowed_updates: &'a [&'a str],
}

#[derive(Serialize)]
struct TelegramSendMessageRequest<'a> {
    chat_id: i64,
    text: &'a str,
    parse_mode: &'static str,
}

impl TelegramClient {
    fn new(bot_token: String) -> Self {
        Self {
            bot_token,
            client: reqwest::Client::new(),
        }
    }

    async fn get_updates(
        &self,
        offset: i64,
        timeout: u64,
    ) -> Result<Vec<TelegramUpdate>, IronclawError> {
        let request = TelegramGetUpdatesRequest {
            offset,
            timeout,
            allowed_updates: &["message"],
        };
        let response = self
            .client
            .post(self.url("getUpdates"))
            .json(&request)
            .send()
            .await
            .map_err(|err| {
                IronclawError::new(format!("telegram getupdates request failed: {err}"))
            })?;
        let response = response
            .error_for_status()
            .map_err(|err| IronclawError::new(format!("telegram getupdates failed: {err}")))?;
        let body: TelegramApiResponse<Vec<TelegramUpdate>> =
            response.json().await.map_err(|err| {
                IronclawError::new(format!("telegram getupdates decode failed: {err}"))
            })?;
        if !body.ok {
            return Err(IronclawError::new("telegram getupdates returned not ok"));
        }
        Ok(body.result)
    }

    async fn download_document(
        &self,
        document: &TelegramDocument,
        prompt: &str,
    ) -> Result<UploadedFile, IronclawError> {
        if document
            .file_size
            .is_some_and(|size| size > MAX_INBOUND_FILE_BYTES as u64)
        {
            return Err(IronclawError::new(format!(
                "file exceeds {} byte upload limit",
                MAX_INBOUND_FILE_BYTES
            )));
        }
        let response = self
            .client
            .post(self.url("getFile"))
            .json(&serde_json::json!({"file_id": document.file_id}))
            .send()
            .await
            .map_err(|err| IronclawError::new(format!("telegram getfile request failed: {err}")))?
            .error_for_status()
            .map_err(|err| IronclawError::new(format!("telegram getfile failed: {err}")))?;
        let body: TelegramApiResponse<TelegramFile> = response
            .json()
            .await
            .map_err(|err| IronclawError::new(format!("telegram getfile decode failed: {err}")))?;
        if !body.ok {
            return Err(IronclawError::new("telegram getfile returned not ok"));
        }
        let file_path = body
            .result
            .file_path
            .ok_or_else(|| IronclawError::new("telegram getfile omitted file path"))?;
        let response = self
            .client
            .get(format!(
                "https://api.telegram.org/file/bot{}/{}",
                self.bot_token, file_path
            ))
            .send()
            .await
            .map_err(|err| IronclawError::new(format!("telegram file download failed: {err}")))?
            .error_for_status()
            .map_err(|err| IronclawError::new(format!("telegram file download failed: {err}")))?;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_INBOUND_FILE_BYTES as u64)
        {
            return Err(IronclawError::new(format!(
                "file exceeds {} byte upload limit",
                MAX_INBOUND_FILE_BYTES
            )));
        }
        let data = response
            .bytes()
            .await
            .map_err(|err| IronclawError::new(format!("telegram file read failed: {err}")))?
            .to_vec();
        let filename = document
            .file_name
            .as_deref()
            .or_else(|| {
                StdPath::new(&file_path)
                    .file_name()
                    .and_then(|name| name.to_str())
            })
            .unwrap_or("upload.bin")
            .to_string();
        let upload = UploadedFile {
            filename,
            mime_type: document
                .mime_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".to_string()),
            data,
            prompt: prompt.to_string(),
        };
        validate_inbound_upload(&upload)?;
        Ok(upload)
    }

    async fn send_message(&self, chat_id: i64, text: &str) -> Result<(), IronclawError> {
        let request = TelegramSendMessageRequest {
            chat_id,
            text,
            parse_mode: "HTML",
        };
        let response = self
            .client
            .post(self.url("sendMessage"))
            .json(&request)
            .send()
            .await
            .map_err(|err| {
                IronclawError::new(format!("telegram sendmessage request failed: {err}"))
            })?;
        let response = response
            .error_for_status()
            .map_err(|err| IronclawError::new(format!("telegram sendmessage failed: {err}")))?;
        let body: TelegramApiResponse<serde_json::Value> =
            response.json().await.map_err(|err| {
                IronclawError::new(format!("telegram sendmessage decode failed: {err}"))
            })?;
        if !body.ok {
            return Err(IronclawError::new("telegram sendmessage returned not ok"));
        }
        Ok(())
    }

    async fn send_artifact(&self, chat_id: i64, artifact: &Artifact) -> Result<(), IronclawError> {
        const TELEGRAM_ARTIFACT_MAX_BYTES: usize = 8 * 1024 * 1024;
        if artifact.data.len() > TELEGRAM_ARTIFACT_MAX_BYTES {
            return Err(IronclawError::new("artifact exceeds Telegram size limit"));
        }
        let is_photo = matches!(artifact.mime_type.as_str(), "image/png" | "image/jpeg");
        let method = if is_photo {
            "sendPhoto"
        } else {
            "sendDocument"
        };
        let field = if is_photo { "photo" } else { "document" };
        let filename = StdPath::new(&artifact.filename)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| IronclawError::new("artifact filename is invalid"))?;
        let part = reqwest::multipart::Part::bytes(artifact.data.clone())
            .file_name(filename.to_string())
            .mime_str(&artifact.mime_type)
            .map_err(|err| IronclawError::new(format!("artifact MIME failed: {err}")))?;
        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part(field, part);
        if !artifact.caption.trim().is_empty() {
            form = form.text(
                "caption",
                artifact.caption.chars().take(1024).collect::<String>(),
            );
        }
        let response = self
            .client
            .post(self.url(method))
            .multipart(form)
            .send()
            .await
            .map_err(|err| IronclawError::new(format!("telegram artifact request failed: {err}")))?
            .error_for_status()
            .map_err(|err| IronclawError::new(format!("telegram artifact failed: {err}")))?;
        let body: TelegramApiResponse<serde_json::Value> = response
            .json()
            .await
            .map_err(|err| IronclawError::new(format!("telegram artifact decode failed: {err}")))?;
        if !body.ok {
            return Err(IronclawError::new("telegram artifact returned not ok"));
        }
        Ok(())
    }

    fn url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.bot_token, method)
    }
}

struct TelegramSession {
    chat_id: i64,
    user_id: String,
    session_id: String,
    msg_id: u64,
    transport: Option<Box<dyn common::transport::Transport>>,
    transcript: TelegramTranscript,
    transcript_path: PathBuf,
    transport_failures: u32,
    runtime_banner_sent: bool,
    guest_sleeping: bool,
    last_user_message_ms: u64,
}

impl TelegramSession {
    fn load(
        chat_id: i64,
        users_root: &StdPath,
        entry_agent: Option<&str>,
    ) -> Result<Self, IronclawError> {
        let user_id = entry_agent
            .map(str::to_string)
            .unwrap_or_else(|| resolve_owner_user_id(ChannelSource::Telegram, None));
        let session_id = telegram_agent_session_id(chat_id, &user_id);
        let transcript_path = telegram_transcript_path(users_root, &session_id);
        let transcript = load_telegram_transcript(&transcript_path)?;
        Ok(Self {
            chat_id,
            user_id,
            session_id,
            msg_id: 1,
            transport: None,
            transcript,
            transcript_path,
            transport_failures: 0,
            runtime_banner_sent: false,
            guest_sleeping: false,
            last_user_message_ms: 0,
        })
    }

    fn switch_agent(&mut self, agent_id: &str, users_root: &StdPath) -> Result<(), IronclawError> {
        self.user_id = agent_id.to_string();
        self.session_id = telegram_agent_session_id(self.chat_id, agent_id);
        self.transcript_path = telegram_transcript_path(users_root, &self.session_id);
        self.transcript = load_telegram_transcript(&self.transcript_path)?;
        self.transport = None;
        self.transport_failures = 0;
        self.runtime_banner_sent = false;
        self.guest_sleeping = false;
        self.last_user_message_ms = 0;
        Ok(())
    }

    fn append_user_message(&mut self, text: &str, timestamp_ms: u64) -> Result<(), IronclawError> {
        self.transcript.push("user", text, timestamp_ms);
        save_telegram_transcript(&self.transcript_path, &self.transcript)
    }

    fn append_assistant_message(
        &mut self,
        text: &str,
        timestamp_ms: u64,
    ) -> Result<(), IronclawError> {
        self.transcript.push("assistant", text, timestamp_ms);
        save_telegram_transcript(&self.transcript_path, &self.transcript)
    }

    fn prompt_history(&self) -> Vec<ConversationMessage> {
        self.transcript
            .messages
            .iter()
            .map(|message| ConversationMessage {
                role: message.role.clone(),
                text: message.text.clone(),
            })
            .collect::<Vec<_>>()
    }

    fn user_messages(&self) -> Vec<String> {
        self.transcript
            .messages
            .iter()
            .filter(|message| message.role == "user")
            .map(|message| message.text.clone())
            .collect()
    }

    fn transport_backoff(&self) -> std::time::Duration {
        let shifted = self.transport_failures.min(6);
        let delay_ms = 250u64.saturating_mul(1u64 << shifted);
        std::time::Duration::from_millis(delay_ms)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TelegramTranscriptMessage {
    role: String,
    text: String,
    timestamp_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct TelegramTranscript {
    messages: Vec<TelegramTranscriptMessage>,
}

impl TelegramTranscript {
    fn push(&mut self, role: &str, text: &str, timestamp_ms: u64) {
        self.messages.push(TelegramTranscriptMessage {
            role: role.to_string(),
            text: text.to_string(),
            timestamp_ms,
        });
        let max_messages = TELEGRAM_TRANSCRIPT_MAX_TURNS.saturating_mul(2);
        if self.messages.len() > max_messages {
            let trim = self.messages.len().saturating_sub(max_messages);
            self.messages.drain(0..trim);
        }
    }
}

async fn run_telegram_loop(
    state: AppState,
    settings: TelegramSettings,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), IronclawError> {
    if let Err(err) = ensure_channel_allowed(&state, "telegram") {
        tracing::warn!("telegram disabled by channel allowlist: {err}");
        return Ok(());
    }
    tracing::info!(
        "telegram loop started owner_chat_id={} offset_file={}",
        settings.owner_chat_id,
        settings.offset_file.display()
    );
    let client = TelegramClient::new(settings.bot_token.clone());
    let mut offset = load_telegram_offset(&settings.offset_file)?;
    let mut sessions = HashMap::<i64, TelegramSession>::new();
    let mut owner_session = TelegramSession::load(
        settings.owner_chat_id,
        &state.host_config.storage.users_root,
        state.host_config.farm.entry_agent.as_deref(),
    )?;
    if state.execution_mode != RuntimeExecutionMode::HostOnly {
        match ensure_telegram_session_transport(&state, &mut owner_session).await {
            Ok(()) => tracing::info!("telegram owner Firecracker session is ready"),
            Err(err) => tracing::warn!(
                "telegram owner Firecracker session startup failed; will retry on message: {err}"
            ),
        }
    }
    sessions.insert(settings.owner_chat_id, owner_session);

    loop {
        if *shutdown.borrow() {
            break;
        }

        let updates = tokio::select! {
            value = client.get_updates(offset, settings.poll_timeout_seconds) => value,
            changed = shutdown.changed() => {
                let _ = changed;
                continue;
            }
        };

        match updates {
            Ok(list) => {
                for update in list {
                    offset = min(i64::MAX - 1, update.update_id.saturating_add(1));
                    save_telegram_offset(&settings.offset_file, offset)?;

                    let Some(message) = update.message else {
                        continue;
                    };
                    let chat_id = message.chat.id;
                    tracing::debug!(
                        "channel route source=telegram chat_id={} event=ingress",
                        chat_id
                    );
                    if chat_id != settings.owner_chat_id {
                        tracing::warn!("telegram message denied from chat {}", chat_id);
                        continue;
                    }
                    if !sessions.contains_key(&chat_id) {
                        let session = TelegramSession::load(
                            chat_id,
                            &state.host_config.storage.users_root,
                            state.host_config.farm.entry_agent.as_deref(),
                        )?;
                        sessions.insert(chat_id, session);
                    }
                    let upload = if let Some(document) = message.document.as_ref() {
                        let prompt = message
                            .caption
                            .as_deref()
                            .filter(|caption| !caption.trim().is_empty())
                            .unwrap_or("Analyze this file and report the important findings.");
                        match client.download_document(document, prompt).await {
                            Ok(upload) => Some(upload),
                            Err(err) => {
                                tracing::warn!("telegram document download rejected: {err}");
                                let _ = client.send_message(chat_id, &err.to_string()).await;
                                continue;
                            }
                        }
                    } else {
                        None
                    };
                    let text = message.text.as_deref().or(message.caption.as_deref());
                    if text.is_none() && upload.is_none() {
                        continue;
                    }
                    let Some(session) = sessions.get_mut(&chat_id) else {
                        continue;
                    };
                    let request_cost = upload.as_ref().map_or(0, |file| file.data.len() as u64);
                    if let Err(err) =
                        enforce_rate_limit(&state, &session.user_id, "telegram", request_cost)
                    {
                        tracing::warn!(
                            "rate limit hit user_id={} channel=telegram err={}",
                            session.user_id,
                            err
                        );
                        let _ = client
                            .send_message(session.chat_id, "429 too many requests")
                            .await;
                        continue;
                    }
                    let prompt =
                        text.unwrap_or("Analyze this file and report the important findings.");
                    if let Err(err) =
                        handle_telegram_text(&state, &client, session, prompt, upload.as_ref())
                            .await
                    {
                        tracing::error!("telegram message handling failed: {err}");
                        let _ = session
                            .append_assistant_message("request failed", now_ms().unwrap_or(0));
                        let _ = client.send_message(session.chat_id, "request failed").await;
                    }
                }
            }
            Err(err) => {
                tracing::error!("telegram polling failed: {err}");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }

        if let Err(err) = drain_telegram_background_events(&client, &mut sessions).await {
            tracing::warn!("telegram background event drain failed: {err}");
        }
        if let Err(err) = enforce_telegram_idle_timeouts(&state, &mut sessions).await {
            tracing::warn!("telegram idle timeout check failed: {err}");
        }
    }

    Ok(())
}

async fn drain_telegram_background_events(
    client: &TelegramClient,
    sessions: &mut HashMap<i64, TelegramSession>,
) -> Result<(), IronclawError> {
    const EVENT_POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(5);
    for session in sessions.values_mut() {
        let Some(transport) = session.transport.as_mut() else {
            continue;
        };
        loop {
            let received = tokio::time::timeout(EVENT_POLL_TIMEOUT, transport.recv()).await;
            let envelope = match received {
                Err(_) => break,
                Ok(Ok(Some(envelope))) => envelope,
                Ok(Ok(None)) => break,
                Ok(Err(err)) => {
                    return Err(IronclawError::new(format!(
                        "telegram background transport recv failed: {err}"
                    )));
                }
            };
            match envelope.payload {
                Some(message_envelope::Payload::JobTrigger(trigger)) => {
                    let notification = handle_guest_job_trigger(
                        transport,
                        &session.user_id,
                        &session.session_id,
                        &mut session.msg_id,
                        &mut session.guest_sleeping,
                        trigger,
                        "telegram",
                    )
                    .await?;
                    if !notification.trim().is_empty() {
                        send_stream_to_telegram(client, session.chat_id, notification.trim())
                            .await?;
                    }
                }
                Some(message_envelope::Payload::AgentState(_)) => {}
                Some(other) => {
                    tracing::warn!(
                        "telegram ignored unexpected idle guest payload: {:?}",
                        other
                    );
                }
                None => {}
            }
        }
    }
    Ok(())
}

async fn enforce_telegram_idle_timeouts(
    state: &AppState,
    sessions: &mut HashMap<i64, TelegramSession>,
) -> Result<(), IronclawError> {
    if state.execution_mode == RuntimeExecutionMode::HostOnly {
        return Ok(());
    }

    let timeout_ms = state
        .host_config
        .idle_timeout_minutes
        .saturating_mul(60_000);
    let now = now_ms()?;
    for session in sessions.values_mut() {
        if session.guest_sleeping || session.last_user_message_ms == 0 {
            continue;
        }
        if now.saturating_sub(session.last_user_message_ms) < timeout_ms {
            continue;
        }
        if let Some(transport) = session.transport.as_mut() {
            tracing::debug!(
                "channel route source=telegram user_id={} session_id={} action=sleep reason=idle_timeout",
                session.user_id,
                session.session_id
            );
            send_agent_control(
                transport,
                &session.user_id,
                &session.session_id,
                session.msg_id,
                agent_control::Command::Sleep,
                "idle_timeout",
            )
            .await?;
            session.msg_id = session.msg_id.saturating_add(1);
            session.guest_sleeping = true;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TelegramTeamCommand {
    Help,
    Team,
    Agent(Option<String>),
    Assign {
        assignee: String,
        skill: String,
        request: String,
    },
    Tasks,
    Invalid(String),
}

fn parse_telegram_team_command(text: &str) -> Option<TelegramTeamCommand> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let mut parts = trimmed.split_whitespace();
    let command = parts
        .next()
        .unwrap_or_default()
        .split('@')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    Some(match command.as_str() {
        "/start" | "/help" => TelegramTeamCommand::Help,
        "/team" => TelegramTeamCommand::Team,
        "/agent" => TelegramTeamCommand::Agent(parts.next().map(str::to_string)),
        "/tasks" => TelegramTeamCommand::Tasks,
        "/assign" | "/task" => {
            let assignee = parts.next().unwrap_or_default().to_string();
            let skill = parts.next().unwrap_or_default().to_string();
            let request = parts.collect::<Vec<_>>().join(" ");
            if assignee.is_empty() || skill.is_empty() || request.is_empty() {
                TelegramTeamCommand::Invalid(
                    "usage: /assign <agent-id> <skill-id> <request>".to_string(),
                )
            } else {
                TelegramTeamCommand::Assign {
                    assignee,
                    skill,
                    request,
                }
            }
        }
        _ => TelegramTeamCommand::Invalid(
            "unknown command; use /help to see the engineering-team commands".to_string(),
        ),
    })
}

async fn execute_telegram_team_command(
    state: &AppState,
    session: &mut TelegramSession,
    command: TelegramTeamCommand,
) -> Result<String, IronclawError> {
    match command {
        TelegramTeamCommand::Help => Ok(format!(
            "Engineering team commands\n\n\
             /team — list the five agents and active work\n\
             /agent <agent-id> — switch who you are talking to\n\
             /assign <agent-id> <skill-id> <request> — assign A2A work as {}\n\
             /tasks — show recent work involving the selected agent\n\n\
             Plain messages go to the selected agent's private VM.",
            session.user_id
        )),
        TelegramTeamCommand::Team => {
            let tasks = state
                .farm_tasks
                .list()
                .map_err(|err| IronclawError::new(err.to_string()))?;
            let mut lines = vec!["Engineering team".to_string()];
            for record in state.farm_registry.agents() {
                let active = tasks
                    .iter()
                    .filter(|task| task.assignee == record.manifest.id && !task.state.terminal())
                    .count();
                let selected = if record.manifest.id == session.user_id {
                    "→"
                } else {
                    "•"
                };
                lines.push(format!(
                    "{selected} {} — {} ({active} active)",
                    record.manifest.id, record.manifest.role
                ));
            }
            if lines.len() == 1 {
                lines.push("No farm agents are configured.".to_string());
            }
            Ok(lines.join("\n"))
        }
        TelegramTeamCommand::Agent(None) => Ok(format!(
            "Selected agent: {}\nUse /team to list available agents.",
            session.user_id
        )),
        TelegramTeamCommand::Agent(Some(agent_id)) => {
            let Some(record) = state.farm_registry.get(&agent_id) else {
                return Ok(format!(
                    "Unknown agent: {agent_id}. Use /team to list agents."
                ));
            };
            let agent_name = record.manifest.name.clone();
            let agent_role = record.manifest.role.clone();
            if session.user_id == agent_id {
                return Ok(format!(
                    "Already talking to {} — {}.",
                    agent_name, agent_role
                ));
            }
            if session.transport.take().is_some() {
                let _ = state.vm_manager.stop_vm(&session.user_id).await;
            }
            session.switch_agent(&agent_id, &state.host_config.storage.users_root)?;
            Ok(format!(
                "Now talking to {} — {}. This agent has a separate VM, transcript, workspace, and memory.",
                agent_name, agent_role
            ))
        }
        TelegramTeamCommand::Assign {
            assignee,
            skill,
            request,
        } => {
            let task = match create_channel_farm_task(
                state,
                &session.user_id,
                &assignee,
                &skill,
                serde_json::json!({"request": request, "source": "telegram"}),
            ) {
                Ok(task) => task,
                Err(err) => return Ok(format!("Task rejected: {err}")),
            };
            Ok(format!(
                "Task submitted\nID: {}\n{} → {} / {}\nUse /tasks to follow it.",
                task.id, task.requester, task.assignee, task.skill
            ))
        }
        TelegramTeamCommand::Tasks => {
            let mut tasks = state
                .farm_tasks
                .list()
                .map_err(|err| IronclawError::new(err.to_string()))?
                .into_iter()
                .filter(|task| {
                    task.requester == session.user_id || task.assignee == session.user_id
                })
                .collect::<Vec<_>>();
            tasks.sort_by_key(|task| std::cmp::Reverse(task.updated_at_ms));
            let mut lines = vec![format!("Recent tasks for {}", session.user_id)];
            for task in tasks.into_iter().take(10) {
                lines.push(format!(
                    "• {} [{}] {} → {} / {}",
                    task.id,
                    format!("{:?}", task.state).to_ascii_lowercase(),
                    task.requester,
                    task.assignee,
                    task.skill
                ));
            }
            if lines.len() == 1 {
                lines.push("No tasks yet.".to_string());
            }
            Ok(lines.join("\n"))
        }
        TelegramTeamCommand::Invalid(message) => Ok(message),
    }
}

fn create_channel_farm_task(
    state: &AppState,
    requester: &str,
    assignee: &str,
    skill: &str,
    input: serde_json::Value,
) -> Result<FarmTask, IronclawError> {
    let capability: farm::CapabilityUri = format!("agent://{assignee}/{skill}")
        .parse()
        .map_err(|err| IronclawError::new(format!("invalid task capability: {err}")))?;
    let allowed = state
        .farm_registry
        .capabilities_for(requester)
        .map_err(|err| IronclawError::new(err.to_string()))?
        .into_iter()
        .any(|candidate| candidate.uri == capability);
    if !allowed {
        return Err(IronclawError::new(format!(
            "{requester} may not assign {skill} to {assignee}"
        )));
    }
    let assignee_record = state
        .farm_registry
        .get(assignee)
        .ok_or_else(|| IronclawError::new(format!("unknown assignee: {assignee}")))?;
    let active_tasks = state
        .farm_tasks
        .list()
        .map_err(|err| IronclawError::new(err.to_string()))?
        .into_iter()
        .filter(|task| task.assignee == assignee && !task.state.terminal())
        .count();
    if active_tasks >= usize::from(assignee_record.manifest.a2a.max_concurrent_tasks) {
        return Err(IronclawError::new(format!(
            "agent {assignee} has reached its concurrent task limit"
        )));
    }
    let now = now_ms()?;
    let task = FarmTask {
        id: format!("task-{now}-{:08x}", rand::random::<u32>()),
        context_id: format!("context-{now}-{:08x}", rand::random::<u32>()),
        parent_task_id: None,
        requester: requester.to_string(),
        assignee: assignee.to_string(),
        skill: skill.to_string(),
        state: TaskState::Submitted,
        input,
        output: None,
        artifact_ids: Vec::new(),
        delegation_depth: 0,
        created_at_ms: now,
        updated_at_ms: now,
    };
    state
        .farm_tasks
        .insert(task.clone())
        .map_err(|err| IronclawError::new(err.to_string()))?;
    spawn_farm_task_dispatch(state.clone(), task.clone());
    Ok(task)
}

async fn handle_telegram_text(
    state: &AppState,
    client: &TelegramClient,
    session: &mut TelegramSession,
    text: &str,
    upload: Option<&UploadedFile>,
) -> Result<(), IronclawError> {
    if let Some(command) = parse_telegram_team_command(text) {
        let output = execute_telegram_team_command(state, session, command).await?;
        send_stream_to_telegram(client, session.chat_id, &output).await?;
        session.append_assistant_message(&output, now_ms().unwrap_or(0))?;
        return Ok(());
    }
    if !session.runtime_banner_sent {
        let banner = telegram_runtime_banner(state.local_guest);
        send_stream_to_telegram(client, session.chat_id, banner).await?;
        session.append_assistant_message(banner, now_ms().unwrap_or(0))?;
        session.runtime_banner_sent = true;
    }

    let history = session.prompt_history();
    let now = now_ms().unwrap_or(0);
    session.last_user_message_ms = now;
    session.append_user_message(text, now)?;
    if let Err(err) = summarize_telegram_session_memory(state, session) {
        tracing::warn!("memory summarize failed: {err}");
    }

    if let Some(command) = parse_memory_command(text) {
        let output = execute_memory_command(state, &session.user_id, &session.session_id, command)?;
        send_stream_to_telegram(client, session.chat_id, &output).await?;
        session.append_assistant_message(&output, now_ms().unwrap_or(0))?;
        return Ok(());
    }

    let mut attempt = 0usize;
    let mut last_error = IronclawError::new("telegram request failed");
    while attempt < TELEGRAM_RETRY_MAX_ATTEMPTS {
        match handle_telegram_text_once(state, client, session, text, upload, &history).await {
            Ok(assistant_text) => {
                session.append_assistant_message(&assistant_text, now_ms().unwrap_or(0))?;
                session.transport_failures = 0;
                return Ok(());
            }
            Err(err) => {
                if !is_transport_failure(&err) || attempt + 1 >= TELEGRAM_RETRY_MAX_ATTEMPTS {
                    return Err(err);
                }
                last_error = err;
                session.transport_failures = session.transport_failures.saturating_add(1);
                session.transport = None;
                session.guest_sleeping = false;
                let _ = state.vm_manager.stop_vm(&session.user_id).await;
                let delay = session.transport_backoff();
                tracing::warn!(
                    "telegram transport restart user_id={} backoff_ms={}",
                    session.user_id,
                    delay.as_millis()
                );
                tokio::time::sleep(delay).await;
            }
        }
        attempt = attempt.saturating_add(1);
    }

    Err(last_error)
}

async fn handle_telegram_text_once(
    state: &AppState,
    client: &TelegramClient,
    session: &mut TelegramSession,
    text: &str,
    upload: Option<&UploadedFile>,
    history: &[ConversationMessage],
) -> Result<String, IronclawError> {
    if let Some(message) =
        telegram_firecracker_requirement_error(state.execution_mode, state.local_guest)
    {
        send_stream_to_telegram(client, session.chat_id, message).await?;
        return Ok(message.to_string());
    }

    let host_allowed_tools = allowed_tools_for_agent(state, &session.user_id);
    if state.execution_mode == RuntimeExecutionMode::HostOnly {
        if upload.is_some() {
            return Err(IronclawError::new(
                "file analysis requires a Firecracker guest",
            ));
        }
        let output = run_host_turn(
            state,
            &session.user_id,
            &host_allowed_tools,
            text,
            Some(history),
        )
        .await?;
        send_stream_to_telegram(client, session.chat_id, &output).await?;
        return Ok(output);
    }

    ensure_telegram_session_transport(state, session).await?;
    if session.guest_sleeping {
        tracing::debug!(
            "channel route source=telegram user_id={} session_id={} action=wake reason=user_message",
            session.user_id,
            session.session_id
        );
        let transport = session
            .transport
            .as_mut()
            .ok_or_else(|| IronclawError::new("missing telegram session transport"))?;
        send_agent_control(
            transport,
            &session.user_id,
            &session.session_id,
            session.msg_id,
            agent_control::Command::Wake,
            "user_message",
        )
        .await?;
        session.msg_id = session.msg_id.saturating_add(1);
        session.guest_sleeping = false;
    }

    let payload = upload.map_or_else(
        || {
            message_envelope::Payload::UserMessage(common::proto::ironclaw::UserMessage {
                text: text.to_string(),
            })
        },
        |file| message_envelope::Payload::UploadedFile(file.clone()),
    );
    let timestamp_ms = now_ms().unwrap_or(0);
    let envelope = MessageEnvelope {
        user_id: session.user_id.clone(),
        session_id: session.session_id.clone(),
        msg_id: session.msg_id,
        timestamp_ms,
        cap_token: String::new(),
        payload: Some(payload),
    };
    session.msg_id = session.msg_id.saturating_add(1);
    let transport = session
        .transport
        .as_mut()
        .ok_or_else(|| IronclawError::new("missing telegram session transport"))?;
    transport
        .send(envelope)
        .await
        .map_err(|err| IronclawError::new(format!("send to guest failed: {err}")))?;

    let host_allowed_tools = allowed_tools_for_agent(state, &session.user_id);

    let mut streamed_any = false;
    let mut output = String::new();
    let mut pending_job_triggers = Vec::new();
    loop {
        let maybe = transport
            .recv()
            .await
            .map_err(|err| IronclawError::new(format!("transport recv failed: {err}")))?;
        let Some(envelope) = maybe else {
            return Err(IronclawError::new("guest transport closed"));
        };
        if let Some(message_envelope::Payload::JobTrigger(trigger)) = envelope.payload.clone() {
            pending_job_triggers.push(trigger);
            continue;
        }
        if matches!(
            envelope.payload,
            Some(message_envelope::Payload::AgentState(_))
        ) {
            continue;
        }
        if let Some(message_envelope::Payload::ToolCallRequest(req)) = envelope.payload.clone() {
            let (ok, output) = execute_requested_host_tool(
                state,
                &session.user_id,
                &session.session_id,
                &req,
                &host_allowed_tools,
                Some(history),
                None,
            )
            .await;
            let resp = MessageEnvelope {
                user_id: envelope.user_id,
                session_id: envelope.session_id,
                msg_id: envelope.msg_id,
                timestamp_ms: envelope.timestamp_ms,
                cap_token: String::new(),
                payload: Some(message_envelope::Payload::ToolCallResponse(
                    common::proto::ironclaw::ToolCallResponse {
                        call_id: req.call_id,
                        ok,
                        output,
                    },
                )),
            };
            transport
                .send(resp)
                .await
                .map_err(|err| IronclawError::new(format!("tool response send failed: {err}")))?;
            continue;
        }
        if let Some(message_envelope::Payload::Artifact(artifact)) = envelope.payload.clone() {
            client.send_artifact(session.chat_id, &artifact).await?;
            output = if artifact.caption.trim().is_empty() {
                format!("sent artifact {}", artifact.filename)
            } else {
                artifact.caption
            };
            streamed_any = true;
            break;
        }
        if let Some(message_envelope::Payload::StreamDelta(delta)) = envelope.payload {
            if !delta.delta.is_empty() {
                output.push_str(&delta.delta);
                send_stream_to_telegram(client, session.chat_id, &delta.delta).await?;
                streamed_any = true;
            }
            if delta.done {
                break;
            }
        }
    }

    for trigger in pending_job_triggers {
        let notification = handle_guest_job_trigger(
            transport,
            &session.user_id,
            &session.session_id,
            &mut session.msg_id,
            &mut session.guest_sleeping,
            trigger,
            "telegram",
        )
        .await
        .map_err(|err| IronclawError::new(format!("job trigger handling failed: {err}")))?;
        if !notification.trim().is_empty() {
            send_stream_to_telegram(client, session.chat_id, notification.trim()).await?;
        }
    }

    if !streamed_any {
        send_stream_to_telegram(client, session.chat_id, "done").await?;
        output.push_str("done");
    }

    Ok(output)
}

async fn send_guest_auth_challenge(
    transport: &mut Box<dyn common::transport::Transport>,
    user_id: &str,
    session_id: &str,
    cap_token: &str,
    guest_allowed_tools: &[String],
    execution_mode: RuntimeExecutionMode,
    agent_manifest_toml: String,
) -> Result<(), IronclawError> {
    let challenge = MessageEnvelope {
        user_id: user_id.to_string(),
        session_id: session_id.to_string(),
        msg_id: 0,
        timestamp_ms: now_ms().unwrap_or(0),
        cap_token: cap_token.to_string(),
        payload: Some(message_envelope::Payload::AuthChallenge(
            common::proto::ironclaw::AuthChallenge {
                cap_token: cap_token.to_string(),
                allowed_tools: guest_allowed_tools.to_vec(),
                execution_mode: execution_mode.to_wire().to_string(),
                brave_api_key: brave_api_key_for_guest(),
                agent_manifest_toml,
            },
        )),
    };
    transport
        .send(challenge)
        .await
        .map_err(|err| IronclawError::new(format!("auth challenge send failed: {err}")))?;
    match tokio::time::timeout(std::time::Duration::from_secs(5), transport.recv()).await {
        Ok(Ok(Some(msg))) => match msg.payload {
            Some(message_envelope::Payload::AuthAck(ack)) if ack.cap_token == cap_token => Ok(()),
            _ => Err(IronclawError::new("invalid auth ack")),
        },
        Ok(Ok(None)) => Err(IronclawError::new(
            "guest closed while waiting for auth ack",
        )),
        Ok(Err(err)) => Err(IronclawError::new(format!("auth ack recv failed: {err}"))),
        Err(_) => Err(IronclawError::new("auth ack timed out")),
    }
}

async fn ensure_telegram_session_transport(
    state: &AppState,
    session: &mut TelegramSession,
) -> Result<(), IronclawError> {
    if session.transport.is_some() {
        return Ok(());
    }

    if let Some(message) =
        telegram_firecracker_requirement_error(state.execution_mode, state.local_guest)
    {
        return Err(IronclawError::new(message));
    }

    let (vm_instance, guest_transport) = start_vm_pair(state, &session.user_id).await?;
    tracing::debug!(
        "channel route source=telegram user_id={} session_id={} event=transport_create",
        session.user_id,
        session.session_id
    );
    let guest_allowed_tools = allowed_tools_for_agent(state, &session.user_id);
    let cap_token = {
        use rand::Rng;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    };

    let mut transport = vm_instance.transport;
    if state.local_guest {
        if let Some(guest_transport) = guest_transport {
            let guest_user_id = session.user_id.clone();
            let users_root = state.host_config.storage.users_root.clone();
            let guest_config_path = (*state.guest_config_path).clone();
            tokio::spawn(async move {
                let brain_root = users_root.join(&guest_user_id).join("guest");
                if let Err(err) = std::fs::create_dir_all(&brain_root) {
                    tracing::warn!("create brain root failed: {err}");
                }
                std::env::set_var("IRONCLAW_BRAIN_ROOT", &brain_root);
                if let Err(err) =
                    irowclaw::runtime::run_with_transport(guest_transport, guest_config_path).await
                {
                    tracing::error!("guest runtime failed: {err}");
                }
            });
        }
    }
    send_guest_auth_challenge(
        &mut transport,
        &session.user_id,
        &session.session_id,
        &cap_token,
        &guest_allowed_tools,
        state.execution_mode,
        agent_manifest_toml_for(state, &session.user_id),
    )
    .await?;
    session.transport = Some(Box::new(AuthenticatedTransport::new(transport, cap_token)));
    session.guest_sleeping = false;
    Ok(())
}

fn is_transport_failure(err: &IronclawError) -> bool {
    let msg = err.to_string();
    msg.contains("send to guest failed")
        || msg.contains("transport recv failed")
        || msg.contains("guest transport closed")
        || msg.contains("tool response send failed")
        || msg.contains("auth challenge send failed")
        || msg.contains("auth ack recv failed")
        || msg.contains("auth ack timed out")
        || msg.contains("invalid auth ack")
        || msg.contains("vm start failed")
}

fn should_enter_idle_sleep(
    execution_mode: RuntimeExecutionMode,
    guest_sleeping: bool,
    last_user_activity: std::time::Instant,
    idle_timeout: std::time::Duration,
) -> bool {
    if execution_mode == RuntimeExecutionMode::HostOnly || guest_sleeping {
        return false;
    }
    last_user_activity.elapsed() >= idle_timeout
}

fn load_guest_tool_flags(config_path: &StdPath) -> (bool, bool) {
    let raw = match std::fs::read_to_string(config_path) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                "guest config read failed at {}: {}",
                config_path.display(),
                err
            );
            return (false, false);
        }
    };
    match toml::from_str::<GuestConfig>(&raw) {
        Ok(config) => (config.tools.allow_bash, config.tools.allow_browser),
        Err(err) => {
            tracing::warn!(
                "guest config parse failed at {}: {}",
                config_path.display(),
                err
            );
            (false, false)
        }
    }
}

fn allowed_tools_for_runtime(
    local_guest: bool,
    guest_allow_bash: bool,
    guest_allow_browser: bool,
) -> Vec<String> {
    let mut tools = vec![
        "file_read".to_string(),
        "file_write".to_string(),
        "schedule_job".to_string(),
        "list_jobs".to_string(),
        "weather".to_string(),
        "publish_artifact".to_string(),
        "code_exec".to_string(),
        "tool_install".to_string(),
        "tool_call".to_string(),
    ];
    if bash_allowed(local_guest, guest_allow_bash) {
        tools.push("bash".to_string());
    }
    if guest_allow_browser {
        tools.push("browser".to_string());
    }
    tools
}

fn allowed_tools_for_agent(state: &AppState, agent_id: &str) -> Vec<String> {
    let mut tools = allowed_tools_for_runtime(
        state.local_guest,
        state.guest_allow_bash,
        state.guest_allow_browser,
    );
    let Some(record) = state.farm_registry.get(agent_id) else {
        return tools;
    };
    // Farm agents install reusable tools exclusively as Wasm modules.
    tools.retain(|tool| !matches!(tool.as_str(), "tool_install" | "tool_call"));
    tools.extend(
        record
            .manifest
            .wasm_tools
            .iter()
            .map(|tool| tool.id.clone()),
    );
    if !record.manifest.mcp.is_empty() {
        tools.push("mcp_call".to_string());
    }
    if !record.manifest.a2a.delegate_to.is_empty() {
        tools.push("delegate_task".to_string());
        tools.push("await_task".to_string());
    }
    tools.sort();
    tools.dedup();
    tools
}

fn agent_manifest_toml_for(state: &AppState, agent_id: &str) -> String {
    state
        .farm_registry
        .get(agent_id)
        .and_then(|record| toml::to_string(&record.manifest).ok())
        .unwrap_or_default()
}

fn bash_allowed(local_guest: bool, guest_allow_bash: bool) -> bool {
    !local_guest && guest_allow_bash
}

fn telegram_runtime_banner(local_guest: bool) -> &'static str {
    if local_guest {
        "tools disabled: firecracker not enabled"
    } else {
        "running in firecracker vm"
    }
}

fn telegram_firecracker_requirement_error(
    execution_mode: RuntimeExecutionMode,
    local_guest: bool,
) -> Option<&'static str> {
    if local_guest && execution_mode != RuntimeExecutionMode::HostOnly {
        Some("Firecracker required for Telegram tool execution")
    } else {
        None
    }
}

async fn send_stream_to_telegram(
    client: &TelegramClient,
    chat_id: i64,
    text: &str,
) -> Result<(), IronclawError> {
    let normalized = if text.trim().is_empty() { " " } else { text };
    for chunk in split_telegram_chunks(normalized, TELEGRAM_CHUNK_MAX_CHARS) {
        let html = render_telegram_html(&chunk);
        client.send_message(chat_id, &html).await?;
    }
    Ok(())
}

fn split_telegram_chunks(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        let hard_end = min(chars.len(), index.saturating_add(max_chars));
        let end = if hard_end == chars.len() {
            hard_end
        } else {
            chars[index..hard_end]
                .iter()
                .rposition(|character| *character == '\n')
                .map(|relative| index + relative + 1)
                .filter(|boundary| *boundary > index)
                .unwrap_or(hard_end)
        };
        out.push(chars[index..end].iter().collect::<String>());
        index = end;
    }
    out
}

fn render_telegram_html(markdown: &str) -> String {
    let mut output = String::new();
    let mut in_code_block = false;
    let lines = markdown.lines().collect::<Vec<_>>();
    let mut line_index = 0usize;

    while line_index < lines.len() {
        let line = lines[line_index];
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_code_block {
                output.push_str("</code></pre>\n");
            } else {
                output.push_str("<pre><code>");
            }
            in_code_block = !in_code_block;
            line_index += 1;
            continue;
        }

        if in_code_block {
            output.push_str(&escape_telegram_html(line));
            output.push('\n');
            line_index += 1;
            continue;
        }

        if trimmed.is_empty() {
            output.push('\n');
            line_index += 1;
            continue;
        }

        if line_index + 1 < lines.len()
            && telegram_table_cells(line).is_some()
            && telegram_table_separator(lines[line_index + 1])
        {
            let mut table_rows = vec![telegram_table_cells(line).unwrap_or_default()];
            line_index += 2;
            while line_index < lines.len() {
                let Some(row) = telegram_table_cells(lines[line_index]) else {
                    break;
                };
                table_rows.push(row);
                line_index += 1;
            }
            output.push_str(&render_telegram_table(&table_rows));
            output.push('\n');
            continue;
        }

        if trimmed.chars().count() >= 3
            && trimmed
                .chars()
                .all(|character| matches!(character, '-' | '_'))
        {
            output.push_str("────────\n");
            line_index += 1;
            continue;
        }

        if let Some(heading) = trimmed
            .strip_prefix("### ")
            .or_else(|| trimmed.strip_prefix("## "))
            .or_else(|| trimmed.strip_prefix("# "))
        {
            output.push_str("<b>");
            output.push_str(&render_telegram_inline(heading));
            output.push_str("</b>\n");
            line_index += 1;
            continue;
        }

        if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            output.push_str("• ");
            output.push_str(&render_telegram_inline(item));
            output.push('\n');
            line_index += 1;
            continue;
        }

        if let Some((number, item)) = telegram_ordered_list_item(trimmed) {
            output.push_str(number);
            output.push_str(". ");
            output.push_str(&render_telegram_inline(item));
            output.push('\n');
            line_index += 1;
            continue;
        }

        if let Some(quote) = trimmed.strip_prefix("> ") {
            output.push_str("<blockquote>");
            output.push_str(&render_telegram_inline(quote));
            output.push_str("</blockquote>\n");
            line_index += 1;
            continue;
        }

        output.push_str(&render_telegram_inline(line));
        output.push('\n');
        line_index += 1;
    }

    if in_code_block {
        output.push_str("</code></pre>\n");
    }
    let output = output.trim_end();
    if output.is_empty() {
        " ".to_string()
    } else {
        output.to_string()
    }
}

fn telegram_table_cells(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
        return None;
    }
    let cells = trimmed
        .trim_matches('|')
        .split('|')
        .map(|cell| telegram_table_cell_text(cell.trim()))
        .collect::<Vec<_>>();
    (cells.len() >= 2).then_some(cells)
}

fn telegram_table_separator(line: &str) -> bool {
    let Some(cells) = telegram_table_cells(line) else {
        return false;
    };
    cells.iter().all(|cell| {
        !cell.is_empty()
            && cell
                .chars()
                .all(|character| matches!(character, '-' | ':' | ' '))
            && cell.contains('-')
    })
}

fn telegram_table_cell_text(cell: &str) -> String {
    cell.replace("**", "").replace('`', "")
}

fn render_telegram_table(rows: &[Vec<String>]) -> String {
    const MAX_CELL_CHARS: usize = 32;
    let column_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; column_count];
    let normalized = rows
        .iter()
        .map(|row| {
            (0..column_count)
                .map(|column| {
                    let cell = row.get(column).map(String::as_str).unwrap_or_default();
                    let truncated = truncate_telegram_table_cell(cell, MAX_CELL_CHARS);
                    widths[column] = widths[column].max(truncated.chars().count());
                    truncated
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    let mut table = String::from("<pre>");
    for (row_index, row) in normalized.iter().enumerate() {
        for (column, cell) in row.iter().enumerate() {
            if column > 0 {
                table.push_str(" | ");
            }
            table.push_str(&escape_telegram_html(cell));
            let padding = widths[column].saturating_sub(cell.chars().count());
            table.extend(std::iter::repeat_n(' ', padding));
        }
        table.push('\n');
        if row_index == 0 {
            for (column, width) in widths.iter().enumerate() {
                if column > 0 {
                    table.push_str("-+-");
                }
                table.extend(std::iter::repeat_n('-', *width));
            }
            table.push('\n');
        }
    }
    table.push_str("</pre>");
    table
}

fn truncate_telegram_table_cell(cell: &str, max_chars: usize) -> String {
    let count = cell.chars().count();
    if count <= max_chars {
        return cell.to_string();
    }
    cell.chars()
        .take(max_chars.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

fn telegram_ordered_list_item(line: &str) -> Option<(&str, &str)> {
    let (number, item) = line.split_once(". ")?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((number, item))
}

fn render_telegram_inline(input: &str) -> String {
    let mut output = String::new();
    let mut remaining = input;

    while !remaining.is_empty() {
        if let Some(rest) = remaining.strip_prefix("**") {
            if let Some(end) = rest.find("**") {
                output.push_str("<b>");
                output.push_str(&render_telegram_inline(&rest[..end]));
                output.push_str("</b>");
                remaining = &rest[end + 2..];
                continue;
            }
        }
        if let Some(rest) = remaining.strip_prefix("~~") {
            if let Some(end) = rest.find("~~") {
                output.push_str("<s>");
                output.push_str(&render_telegram_inline(&rest[..end]));
                output.push_str("</s>");
                remaining = &rest[end + 2..];
                continue;
            }
        }
        if let Some(rest) = remaining.strip_prefix('`') {
            if let Some(end) = rest.find('`') {
                output.push_str("<code>");
                output.push_str(&escape_telegram_html(&rest[..end]));
                output.push_str("</code>");
                remaining = &rest[end + 1..];
                continue;
            }
        }
        if let Some(label) = remaining.strip_prefix('[') {
            if let Some(label_end) = label.find("](") {
                let url_start = label_end + 2;
                if let Some(url_end) = label[url_start..].find(')') {
                    let url = &label[url_start..url_start + url_end];
                    if url.starts_with("https://") || url.starts_with("http://") {
                        output.push_str("<a href=\"");
                        output.push_str(&escape_telegram_html(url));
                        output.push_str("\">");
                        output.push_str(&render_telegram_inline(&label[..label_end]));
                        output.push_str("</a>");
                        remaining = &label[url_start + url_end + 1..];
                        continue;
                    }
                }
            }
        }

        let character = remaining
            .chars()
            .next()
            .expect("remaining is known to be non-empty");
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            _ => output.push(character),
        }
        remaining = &remaining[character.len_utf8()..];
    }
    output
}

fn escape_telegram_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn telegram_transcript_path(users_root: &StdPath, session_id: &str) -> PathBuf {
    users_root.join(session_id).join("telegram.transcript.json")
}

fn telegram_agent_session_id(chat_id: i64, agent_id: &str) -> String {
    format!("telegram-{chat_id}-{agent_id}")
}

fn load_telegram_transcript(path: &StdPath) -> Result<TelegramTranscript, IronclawError> {
    if !path.exists() {
        return Ok(TelegramTranscript::default());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|err| IronclawError::new(format!("telegram transcript read failed: {err}")))?;
    let transcript: TelegramTranscript = serde_json::from_str(&raw)
        .map_err(|err| IronclawError::new(format!("telegram transcript decode failed: {err}")))?;
    Ok(transcript)
}

fn save_telegram_transcript(
    path: &StdPath,
    transcript: &TelegramTranscript,
) -> Result<(), IronclawError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| IronclawError::new(format!("telegram transcript dir failed: {err}")))?;
    }
    let data = serde_json::to_string_pretty(transcript)
        .map_err(|err| IronclawError::new(format!("telegram transcript encode failed: {err}")))?;
    std::fs::write(path, format!("{data}\n"))
        .map_err(|err| IronclawError::new(format!("telegram transcript write failed: {err}")))
}

fn load_telegram_offset(path: &StdPath) -> Result<i64, IronclawError> {
    if !path.exists() {
        return Ok(0);
    }
    let value = std::fs::read_to_string(path)
        .map_err(|err| IronclawError::new(format!("telegram offset read failed: {err}")))?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    trimmed
        .parse::<i64>()
        .map_err(|err| IronclawError::new(format!("telegram offset parse failed: {err}")))
}

fn save_telegram_offset(path: &StdPath, offset: i64) -> Result<(), IronclawError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            IronclawError::new(format!("telegram offset dir create failed: {err}"))
        })?;
    }
    std::fs::write(path, format!("{offset}\n"))
        .map_err(|err| IronclawError::new(format!("telegram offset write failed: {err}")))
}

// ---------------------------------------------------------------------------
// WhatsApp integration (whatsapp-rust crate, event-driven)
// ---------------------------------------------------------------------------

async fn run_whatsapp_loop(
    state: AppState,
    config: common::config::HostWhatsAppConfig,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), IronclawError> {
    if let Err(err) = ensure_channel_allowed(&state, "whatsapp") {
        tracing::warn!("whatsapp disabled by channel allowlist: {err}");
        return Ok(());
    }
    tracing::info!(
        "whatsapp: starting bot (session_dir={}, qr_timeout_ms={})",
        config.session_dir,
        config.qr_timeout_ms
    );

    let (client, mut rx) = whatsapp::start_whatsapp_bot(&config).await?;

    tracing::info!("whatsapp: bot started, waiting for messages");

    let mut sessions = HashMap::<String, whatsapp::WhatsAppSession>::new();

    loop {
        tokio::select! {
            maybe_msg = rx.recv() => {
                let Some(incoming) = maybe_msg else {
                    tracing::warn!("whatsapp message channel closed");
                    break;
                };

                if !whatsapp::is_allowed(&config, &incoming.sender_jid) {
                    tracing::warn!("whatsapp message denied from {}", incoming.sender_jid);
                    continue;
                }

                if !sessions.contains_key(&incoming.sender_jid) {
                    let users_root = StdPath::new(&state.host_config.storage.users_root);
                    match whatsapp::WhatsAppSession::load(
                        &incoming.sender_jid,
                        users_root,
                    ) {
                        Ok(mut session) => {
                            session.user_id = resolve_owner_user_id(
                                ChannelSource::WhatsApp,
                                Some(&incoming.sender_jid),
                            );
                            sessions.insert(incoming.sender_jid.clone(), session);
                        }
                        Err(err) => {
                            tracing::error!("whatsapp session load failed: {err}");
                            continue;
                        }
                    }
                }
                let Some(session) = sessions.get_mut(&incoming.sender_jid) else {
                    continue;
                };
                if let Err(err) = enforce_rate_limit(&state, &session.user_id, "whatsapp", 0) {
                    tracing::warn!(
                        "rate limit hit user_id={} channel=whatsapp err={}",
                        session.user_id,
                        err
                    );
                    let _ = whatsapp::send_whatsapp_message(
                        &client,
                        &session.sender_jid,
                        "429 too many requests",
                    ).await;
                    continue;
                }
                tracing::debug!(
                    "channel route source=whatsapp sender={} user_id={} session_id={} event=ingress",
                    incoming.sender_jid,
                    session.user_id,
                    session.session_id
                );

                if let Err(err) = handle_whatsapp_text(
                    &state,
                    &client,
                    session,
                    &incoming.text,
                ).await {
                    tracing::error!("whatsapp message handling failed: {err}");
                    let _ = session.append_assistant_message(
                        "request failed",
                        now_ms().unwrap_or(0),
                    );
                    let _ = whatsapp::send_whatsapp_message(
                        &client,
                        &session.sender_jid,
                        "request failed",
                    ).await;
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(IDLE_CHECK_SECONDS)) => {
                if let Err(err) = enforce_whatsapp_idle_timeouts(&state, &mut sessions).await {
                    tracing::warn!("whatsapp idle timeout check failed: {err}");
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("whatsapp: shutdown signal received");
                    break;
                }
            }
        }
    }

    Ok(())
}

async fn enforce_whatsapp_idle_timeouts(
    state: &AppState,
    sessions: &mut HashMap<String, whatsapp::WhatsAppSession>,
) -> Result<(), IronclawError> {
    if state.execution_mode == RuntimeExecutionMode::HostOnly {
        return Ok(());
    }
    let timeout_ms = state
        .host_config
        .idle_timeout_minutes
        .saturating_mul(60_000);
    let now = now_ms()?;
    for session in sessions.values_mut() {
        if session.guest_sleeping || session.last_user_message_ms == 0 {
            continue;
        }
        if now.saturating_sub(session.last_user_message_ms) < timeout_ms {
            continue;
        }
        if let Some(transport) = session.transport.as_mut() {
            tracing::debug!(
                "channel route source=whatsapp user_id={} session_id={} action=sleep reason=idle_timeout",
                session.user_id,
                session.session_id
            );
            send_agent_control(
                transport,
                &session.user_id,
                &session.session_id,
                session.msg_id,
                agent_control::Command::Sleep,
                "idle_timeout",
            )
            .await?;
            session.msg_id = session.msg_id.saturating_add(1);
            session.guest_sleeping = true;
        }
    }
    Ok(())
}

async fn handle_whatsapp_text(
    state: &AppState,
    client: &whatsapp_rust::Client,
    session: &mut whatsapp::WhatsAppSession,
    text: &str,
) -> Result<(), IronclawError> {
    let history = session.prompt_history();
    let now = now_ms()?;
    session.last_user_message_ms = now;
    session.append_user_message(text, now)?;

    if let Err(err) = summarize_whatsapp_session_memory(state, session) {
        tracing::warn!("whatsapp memory summarize failed: {err}");
    }

    // Handle memory commands (remember, pins, forget).
    if let Some(command) = parse_memory_command(text) {
        let output = execute_memory_command(state, &session.user_id, &session.session_id, command)?;
        whatsapp::send_whatsapp_message(client, &session.sender_jid, &output).await?;
        session.append_assistant_message(&output, now_ms()?)?;
        return Ok(());
    }

    let mut attempt = 0usize;
    let mut last_error = IronclawError::new("whatsapp request failed");
    while attempt < TELEGRAM_RETRY_MAX_ATTEMPTS {
        match handle_whatsapp_text_once(state, client, session, text, &history).await {
            Ok(output) => {
                session.append_assistant_message(&output, now_ms()?)?;
                session.transport_failures = 0;
                return Ok(());
            }
            Err(err) => {
                if !is_transport_failure(&err) || attempt + 1 >= TELEGRAM_RETRY_MAX_ATTEMPTS {
                    return Err(err);
                }
                last_error = err;
                session.transport_failures = session.transport_failures.saturating_add(1);
                session.transport = None;
                session.guest_sleeping = false;
                let _ = state.vm_manager.stop_vm(&session.user_id).await;
                let delay = session.transport_backoff();
                tracing::warn!(
                    "whatsapp transport restart user_id={} backoff_ms={}",
                    session.user_id,
                    delay.as_millis()
                );
                tokio::time::sleep(delay).await;
            }
        }
        attempt = attempt.saturating_add(1);
    }

    Err(last_error)
}

async fn handle_whatsapp_text_once(
    state: &AppState,
    client: &whatsapp_rust::Client,
    session: &mut whatsapp::WhatsAppSession,
    text: &str,
    history: &[ConversationMessage],
) -> Result<String, IronclawError> {
    if let Some(message) =
        telegram_firecracker_requirement_error(state.execution_mode, state.local_guest)
    {
        whatsapp::send_whatsapp_message(client, &session.sender_jid, message).await?;
        return Ok(message.to_string());
    }

    let host_allowed_tools = allowed_tools_for_agent(state, &session.user_id);
    if state.execution_mode == RuntimeExecutionMode::HostOnly {
        let output = run_host_turn(
            state,
            &session.user_id,
            &host_allowed_tools,
            text,
            Some(history),
        )
        .await?;
        whatsapp::send_whatsapp_message(client, &session.sender_jid, &output).await?;
        return Ok(output);
    }

    ensure_whatsapp_session_transport(state, session).await?;
    if session.guest_sleeping {
        tracing::debug!(
            "channel route source=whatsapp user_id={} session_id={} action=wake reason=user_message",
            session.user_id,
            session.session_id
        );
        let transport = session
            .transport
            .as_mut()
            .ok_or_else(|| IronclawError::new("missing whatsapp session transport"))?;
        send_agent_control(
            transport,
            &session.user_id,
            &session.session_id,
            session.msg_id,
            agent_control::Command::Wake,
            "user_message",
        )
        .await?;
        session.msg_id = session.msg_id.saturating_add(1);
        session.guest_sleeping = false;
    }

    let payload = message_envelope::Payload::UserMessage(common::proto::ironclaw::UserMessage {
        text: text.to_string(),
    });
    let timestamp_ms = now_ms().unwrap_or(0);
    let envelope = MessageEnvelope {
        user_id: session.user_id.clone(),
        session_id: session.session_id.clone(),
        msg_id: session.msg_id,
        timestamp_ms,
        cap_token: String::new(),
        payload: Some(payload),
    };
    session.msg_id = session.msg_id.saturating_add(1);
    let transport = session
        .transport
        .as_mut()
        .ok_or_else(|| IronclawError::new("missing whatsapp session transport"))?;
    transport
        .send(envelope)
        .await
        .map_err(|err| IronclawError::new(format!("send to guest failed: {err}")))?;

    let mut streamed_any = false;
    let mut output = String::new();
    loop {
        let maybe = transport
            .recv()
            .await
            .map_err(|err| IronclawError::new(format!("transport recv failed: {err}")))?;
        let Some(envelope) = maybe else {
            return Err(IronclawError::new("guest transport closed"));
        };
        if let Some(message_envelope::Payload::JobTrigger(trigger)) = envelope.payload.clone() {
            handle_guest_job_trigger(
                transport,
                &session.user_id,
                &session.session_id,
                &mut session.msg_id,
                &mut session.guest_sleeping,
                trigger,
                "whatsapp",
            )
            .await
            .map_err(|err| IronclawError::new(format!("job trigger handling failed: {err}")))?;
            continue;
        }
        if matches!(
            envelope.payload,
            Some(message_envelope::Payload::AgentState(_))
        ) {
            continue;
        }
        if let Some(message_envelope::Payload::ToolCallRequest(req)) = envelope.payload.clone() {
            let (ok, output) = execute_requested_host_tool(
                state,
                &session.user_id,
                &session.session_id,
                &req,
                &host_allowed_tools,
                Some(history),
                None,
            )
            .await;
            let resp = MessageEnvelope {
                user_id: envelope.user_id,
                session_id: envelope.session_id,
                msg_id: envelope.msg_id,
                timestamp_ms: envelope.timestamp_ms,
                cap_token: String::new(),
                payload: Some(message_envelope::Payload::ToolCallResponse(
                    common::proto::ironclaw::ToolCallResponse {
                        call_id: req.call_id,
                        ok,
                        output,
                    },
                )),
            };
            transport
                .send(resp)
                .await
                .map_err(|err| IronclawError::new(format!("tool response send failed: {err}")))?;
            continue;
        }
        if let Some(message_envelope::Payload::StreamDelta(delta)) = envelope.payload {
            if !delta.delta.is_empty() {
                output.push_str(&delta.delta);
                whatsapp::send_whatsapp_message(client, &session.sender_jid, &delta.delta).await?;
                streamed_any = true;
            }
            if delta.done {
                break;
            }
        }
    }

    if !streamed_any {
        whatsapp::send_whatsapp_message(client, &session.sender_jid, "done").await?;
        output.push_str("done");
    }

    Ok(output)
}

async fn ensure_whatsapp_session_transport(
    state: &AppState,
    session: &mut whatsapp::WhatsAppSession,
) -> Result<(), IronclawError> {
    if session.transport.is_some() {
        return Ok(());
    }
    if let Some(message) =
        telegram_firecracker_requirement_error(state.execution_mode, state.local_guest)
    {
        return Err(IronclawError::new(message));
    }
    let (vm_instance, guest_transport) = start_vm_pair(state, &session.user_id).await?;
    tracing::debug!(
        "channel route source=whatsapp user_id={} session_id={} event=transport_create",
        session.user_id,
        session.session_id
    );
    let guest_allowed_tools = allowed_tools_for_agent(state, &session.user_id);
    let cap_token = {
        use rand::Rng;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    };

    let mut transport = vm_instance.transport;
    if state.local_guest {
        if let Some(guest_transport) = guest_transport {
            let guest_user_id = session.user_id.clone();
            let users_root = state.host_config.storage.users_root.clone();
            let guest_config_path = (*state.guest_config_path).clone();
            tokio::spawn(async move {
                let brain_root = users_root.join(&guest_user_id).join("guest");
                if let Err(err) = std::fs::create_dir_all(&brain_root) {
                    tracing::warn!("create brain root failed: {err}");
                }
                std::env::set_var("IRONCLAW_BRAIN_ROOT", &brain_root);
                if let Err(err) =
                    irowclaw::runtime::run_with_transport(guest_transport, guest_config_path).await
                {
                    tracing::error!("guest runtime failed: {err}");
                }
            });
        }
    }
    send_guest_auth_challenge(
        &mut transport,
        &session.user_id,
        &session.session_id,
        &cap_token,
        &guest_allowed_tools,
        state.execution_mode,
        agent_manifest_toml_for(state, &session.user_id),
    )
    .await?;
    session.transport = Some(Box::new(AuthenticatedTransport::new(transport, cap_token)));
    session.guest_sleeping = false;
    Ok(())
}

fn summarize_whatsapp_session_memory(
    state: &AppState,
    session: &whatsapp::WhatsAppSession,
) -> Result<(), IronclawError> {
    let conn = open_memory_db(state, &session.user_id)?;
    let user_messages = session.user_messages();
    let _ = maybe_summarize_session(
        &conn,
        &session.user_id,
        &session.session_id,
        &user_messages,
        now_ms()?,
    )
    .map_err(|err| IronclawError::new(format!("memory summarize failed: {err}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        agent_organization_context, allowed_tools_for_runtime, brain_ext4_path,
        decode_host_plan_request, decode_websocket_upload, deterministic_a2a_plan,
        deterministic_await_a2a_plan, deterministic_guest_tools_plan,
        finalize_requested_host_tool_response, handle_guest_job_trigger, load_telegram_offset,
        load_telegram_transcript, parse_telegram_team_command, render_telegram_html,
        resolve_owner_user_id, save_telegram_offset, save_telegram_transcript,
        should_enter_idle_sleep, split_telegram_chunks, telegram_agent_session_id,
        telegram_firecracker_requirement_error, telegram_transcript_path, validate_inbound_upload,
        ws_text_to_guest_payload, ChannelSource, RuntimeExecutionMode, TelegramApiResponse,
        TelegramSendMessageRequest, TelegramTeamCommand, TelegramTranscript,
        TelegramTranscriptMessage, TelegramUpdate, ToolPlan, MAX_INBOUND_FILE_BYTES,
        TELEGRAM_TRANSCRIPT_MAX_TURNS,
    };
    use common::proto::ironclaw::{message_envelope, MessageEnvelope, UploadedFile};
    use common::transport::{LocalTransport, Transport};
    use prost::Message;

    #[test]
    fn host_plan_response_is_not_truncated_mid_json() {
        let text = "x".repeat(12_000);
        let plan = serde_json::json!({"action": "answer", "text": text}).to_string();
        let (ok, output) = finalize_requested_host_tool_response("host_plan", Ok(plan.clone()));
        assert!(ok);
        assert_eq!(output, plan);
        assert!(serde_json::from_str::<serde_json::Value>(&output).is_ok());

        let (_, shell_output) =
            finalize_requested_host_tool_response("shell", Ok("x".repeat(12_000)));
        assert!(shell_output.chars().count() < 12_000);
    }

    #[test]
    fn tooltest_write_plans_file_write() {
        let allowed_tools = vec!["file_read".to_string(), "file_write".to_string()];
        let plan = deterministic_guest_tools_plan(
            "TOOLTEST WRITE notes/tool.txt\nhello-tool",
            &allowed_tools,
        );
        assert!(matches!(
            plan,
            Some(ToolPlan::Tool { tool, input })
            if tool == "file_write" && input == "notes/tool.txt\nhello-tool"
        ));
    }

    #[test]
    fn tooltest_read_plans_file_read() {
        let allowed_tools = vec!["file_read".to_string(), "file_write".to_string()];
        let plan = deterministic_guest_tools_plan("TOOLTEST READ notes/tool.txt", &allowed_tools);
        assert!(matches!(
            plan,
            Some(ToolPlan::Tool { tool, input })
            if tool == "file_read" && input == "notes/tool.txt"
        ));
    }

    #[test]
    fn telegram_team_commands_parse_assignments_and_bot_suffixes() {
        assert_eq!(
            parse_telegram_team_command(
                "/assign backend-engineer implement_backend Build the billing API"
            ),
            Some(TelegramTeamCommand::Assign {
                assignee: "backend-engineer".to_string(),
                skill: "implement_backend".to_string(),
                request: "Build the billing API".to_string(),
            })
        );
        assert_eq!(
            parse_telegram_team_command("/team@ironclaw_demo_bot"),
            Some(TelegramTeamCommand::Team)
        );
        assert_eq!(parse_telegram_team_command("hello"), None);
    }

    #[test]
    fn telegram_agents_get_separate_session_ids() {
        assert_eq!(
            telegram_agent_session_id(42, "engineering-lead"),
            "telegram-42-engineering-lead"
        );
        assert_ne!(
            telegram_agent_session_id(42, "backend-engineer"),
            telegram_agent_session_id(42, "frontend-engineer")
        );
    }

    #[test]
    fn non_tooltest_input_uses_llm_path() {
        let allowed_tools = vec!["file_read".to_string(), "file_write".to_string()];
        let plan = deterministic_guest_tools_plan("read notes/tool.txt", &allowed_tools);
        assert!(plan.is_none());
    }

    #[test]
    fn telegram_chunks_split_large_text() {
        let text = "x".repeat(9000);
        let chunks = split_telegram_chunks(&text, 4096);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chars().count(), 4096);
        assert_eq!(chunks[1].chars().count(), 4096);
        assert_eq!(chunks[2].chars().count(), 808);
    }

    #[test]
    fn telegram_chunks_prefer_line_boundaries() {
        let chunks = split_telegram_chunks("first line\nsecond line\nthird", 13);
        assert_eq!(chunks, vec!["first line\n", "second line\n", "third"]);
    }

    #[test]
    fn telegram_commonmark_is_rendered_as_safe_html() {
        let markdown = r#"The IP address **5.32.57.218** appears to be from a cloud provider.

---

Done! Here's what I did:

1. **Cleared** the old cron schedule
2. **Created** a new cron job

- **ID**: thesis-reminder
- **Schedule**: Every 30 minutes (`*/30 * * * *`)
- **Message**: "<remind & notify>""#;
        let html = render_telegram_html(markdown);
        assert!(html.contains("<b>5.32.57.218</b>"));
        assert!(html.contains("1. <b>Cleared</b>"));
        assert!(html.contains("• <b>ID</b>: thesis-reminder"));
        assert!(html.contains("<code>*/30 * * * *</code>"));
        assert!(html.contains("&quot;&lt;remind &amp; notify&gt;&quot;"));
        assert!(!html.contains("**"));
    }

    #[test]
    fn telegram_markdown_tables_render_as_aligned_code_tables() {
        let markdown = r#"**Database Summary**

| Table | Rows | Description |
|-------|------|-------------|
| users | 50,000 | Registered users |
| order_items | 347,266 | Individual mod purchases |

__________

**Top Mod Buyer**

| Metric | Value |
|--------|-------|
| Mods Purchased | 45 |
| Total Spent | $107.10 |"#;
        let html = render_telegram_html(markdown);
        assert!(html.contains("<b>Database Summary</b>"));
        assert!(html.contains("<pre>Table"));
        assert!(html.contains("users"));
        assert!(html.contains("order_items"));
        assert!(html.contains("Metric"));
        assert!(html.contains("Total Spent"));
        assert!(html.contains("────────"));
        assert_eq!(html.matches("<pre>").count(), 2);
        assert_eq!(html.matches("</pre>").count(), 2);
        assert!(!html.contains("|-------|"));
        assert!(!html.contains("__________"));
    }

    #[test]
    fn telegram_send_message_selects_html_parse_mode() {
        let request = TelegramSendMessageRequest {
            chat_id: 42,
            text: "<b>formatted</b>",
            parse_mode: "HTML",
        };
        let json = serde_json::to_value(request).expect("serialize sendMessage request");
        assert_eq!(json["parse_mode"], "HTML");
    }

    #[test]
    fn telegram_offset_roundtrip() {
        let path = std::env::temp_dir().join("ironclaw-telegram-offset-test.txt");
        let _ = std::fs::remove_file(&path);
        save_telegram_offset(&path, 44).expect("save offset");
        let loaded = load_telegram_offset(&path).expect("load offset");
        assert_eq!(loaded, 44);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn telegram_transcript_keeps_last_fifty_turns() {
        let mut transcript = TelegramTranscript::default();
        for idx in 0..60u64 {
            transcript.push("user", &format!("u{idx}"), idx);
            transcript.push("assistant", &format!("a{idx}"), idx);
        }
        let max_messages = TELEGRAM_TRANSCRIPT_MAX_TURNS * 2;
        assert_eq!(transcript.messages.len(), max_messages);
        assert_eq!(
            transcript.messages.first().map(|m| m.text.clone()),
            Some("u10".to_string())
        );
        assert_eq!(
            transcript.messages.last().map(|m| m.text.clone()),
            Some("a59".to_string())
        );
    }

    #[test]
    fn telegram_transcript_path_and_persistence_roundtrip() {
        let root = std::env::temp_dir().join("ironclaw-transcript-test");
        let _ = std::fs::remove_dir_all(&root);
        let path = telegram_transcript_path(&root, "telegram-7");
        let transcript = TelegramTranscript {
            messages: vec![TelegramTranscriptMessage {
                role: "user".to_string(),
                text: "hello".to_string(),
                timestamp_ms: 1,
            }],
        };
        let save = save_telegram_transcript(&path, &transcript);
        assert!(save.is_ok());
        let loaded = load_telegram_transcript(&path);
        assert!(loaded.is_ok());
        assert_eq!(loaded.unwrap_or_default().messages.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn telegram_guest_modes_require_firecracker_without_local_guest_fallback() {
        let guest_tools =
            telegram_firecracker_requirement_error(RuntimeExecutionMode::GuestTools, true);
        assert_eq!(
            guest_tools,
            Some("Firecracker required for Telegram tool execution")
        );

        let guest_autonomous =
            telegram_firecracker_requirement_error(RuntimeExecutionMode::GuestAutonomous, true);
        assert_eq!(
            guest_autonomous,
            Some("Firecracker required for Telegram tool execution")
        );

        let host_only =
            telegram_firecracker_requirement_error(RuntimeExecutionMode::HostOnly, true);
        assert_eq!(host_only, None);
    }

    #[test]
    fn local_runtime_never_offers_bash_tool() {
        let local_tools = allowed_tools_for_runtime(true, true, true);
        assert!(!local_tools.iter().any(|tool| tool == "bash"));
        assert!(local_tools.iter().any(|tool| tool == "browser"));

        let firecracker_without_bash = allowed_tools_for_runtime(false, false, false);
        assert!(!firecracker_without_bash.iter().any(|tool| tool == "bash"));
        assert!(!firecracker_without_bash
            .iter()
            .any(|tool| tool == "browser"));

        let firecracker_with_bash = allowed_tools_for_runtime(false, true, true);
        assert!(firecracker_with_bash.iter().any(|tool| tool == "bash"));
        assert!(firecracker_with_bash.iter().any(|tool| tool == "browser"));
    }

    #[test]
    fn channel_owner_identity_routes_across_sources() {
        assert_eq!(
            resolve_owner_user_id(ChannelSource::Telegram, None),
            "owner".to_string()
        );
        assert_eq!(
            resolve_owner_user_id(ChannelSource::Telegram, Some("123456")),
            "owner".to_string()
        );
        assert_eq!(
            resolve_owner_user_id(ChannelSource::WhatsApp, None),
            "owner".to_string()
        );
        assert_eq!(
            resolve_owner_user_id(ChannelSource::WhatsApp, Some("15551234567@s.whatsapp.net")),
            "owner".to_string()
        );
        assert_eq!(
            resolve_owner_user_id(ChannelSource::WebSocket, Some("owner")),
            "owner".to_string()
        );
        assert_eq!(
            resolve_owner_user_id(ChannelSource::WebSocket, Some("alice")),
            "alice".to_string()
        );
    }

    #[test]
    fn telegram_update_json_parses_text_message() {
        let json = r#"{
            "ok": true,
            "result": [
                {
                    "update_id": 77,
                    "message": {
                        "text": "hello from telegram",
                        "chat": { "id": 12345 }
                    }
                }
            ]
        }"#;
        let parsed: TelegramApiResponse<Vec<TelegramUpdate>> = match serde_json::from_str(json) {
            Ok(value) => value,
            Err(err) => panic!("telegram parse failed: {err}"),
        };

        assert!(parsed.ok);
        assert_eq!(parsed.result.len(), 1);
        let update = &parsed.result[0];
        assert_eq!(update.update_id, 77);
        let message = match &update.message {
            Some(value) => value,
            None => panic!("message missing"),
        };
        assert_eq!(message.chat.id, 12345);
        assert_eq!(message.text.as_deref(), Some("hello from telegram"));
    }

    #[test]
    fn telegram_update_json_parses_pdf_document_and_caption() {
        let json = r#"{
            "ok": true,
            "result": [{
                "update_id": 78,
                "message": {
                    "caption": "Summarize this draft",
                    "document": {
                        "file_id": "telegram-file-id",
                        "file_name": "thesis.tex",
                        "mime_type": "application/x-tex",
                        "file_size": 321
                    },
                    "chat": { "id": 12345 }
                }
            }]
        }"#;
        let parsed: TelegramApiResponse<Vec<TelegramUpdate>> =
            serde_json::from_str(json).expect("parse document update");
        let message = parsed.result[0].message.as_ref().expect("message");
        let document = message.document.as_ref().expect("document");
        assert_eq!(message.caption.as_deref(), Some("Summarize this draft"));
        assert_eq!(document.file_name.as_deref(), Some("thesis.tex"));
        assert_eq!(document.mime_type.as_deref(), Some("application/x-tex"));
        assert_eq!(document.file_size, Some(321));
    }

    #[test]
    fn websocket_binary_upload_decodes_protobuf_envelope() {
        let envelope = MessageEnvelope {
            user_id: "cli".to_string(),
            session_id: "test".to_string(),
            msg_id: 9,
            timestamp_ms: 0,
            cap_token: "cap".to_string(),
            payload: Some(message_envelope::Payload::UploadedFile(UploadedFile {
                filename: "draft.pdf".to_string(),
                mime_type: "application/pdf".to_string(),
                data: b"%PDF".to_vec(),
                prompt: "Summarize".to_string(),
            })),
        };
        let encoded = envelope.encode_to_vec();
        let upload = decode_websocket_upload(&encoded).expect("decode upload");
        assert_eq!(upload.filename, "draft.pdf");
        assert_eq!(upload.data, b"%PDF");
        assert_eq!(upload.prompt, "Summarize");
    }

    #[test]
    fn inbound_upload_rejects_files_over_eight_megabytes() {
        let upload = UploadedFile {
            filename: "too-large.bin".to_string(),
            mime_type: "application/octet-stream".to_string(),
            data: vec![0; MAX_INBOUND_FILE_BYTES + 1],
            prompt: String::new(),
        };
        let error = validate_inbound_upload(&upload).expect_err("must reject oversized upload");
        assert!(error.to_string().contains("upload limit"));
    }

    #[test]
    fn websocket_toolcall_messages_route_as_tool_calls() {
        let (payload, next_msg_id) =
            ws_text_to_guest_payload("!toolcall file_read\nnotes/a.txt", 9);
        assert_eq!(next_msg_id, 10);
        match payload {
            message_envelope::Payload::ToolCallRequest(request) => {
                assert_eq!(request.call_id, 9);
                assert_eq!(request.tool, "file_read");
                assert_eq!(request.input, "notes/a.txt");
            }
            _ => panic!("expected tool call request"),
        }
    }

    #[test]
    fn websocket_plain_text_routes_as_user_message() {
        let (payload, next_msg_id) = ws_text_to_guest_payload("hello guest", 41);
        assert_eq!(next_msg_id, 42);
        match payload {
            message_envelope::Payload::UserMessage(message) => {
                assert_eq!(message.text, "hello guest");
            }
            _ => panic!("expected user message payload"),
        }
    }

    #[tokio::test]
    async fn cron_job_trigger_wakes_agent_and_reports_status() {
        let (host, mut guest) = LocalTransport::pair(16);
        let mut host_box: Box<dyn Transport> = Box::new(host);
        let guest_task = tokio::spawn(async move {
            // host should wake sleeping agent before scheduling the job
            let wake = guest
                .recv()
                .await
                .expect("wake recv")
                .expect("wake envelope");
            assert!(matches!(
                wake.payload,
                Some(message_envelope::Payload::AgentControl(_))
            ));
            // host requests scheduled job execution
            let run = guest.recv().await.expect("run recv").expect("run envelope");
            let call_id = match run.payload {
                Some(message_envelope::Payload::ToolCallRequest(req)) => {
                    assert_eq!(req.tool, "run_scheduled_job".to_string());
                    assert_eq!(req.input, "cron-test".to_string());
                    req.call_id
                }
                _ => panic!("expected run_scheduled_job call"),
            };
            guest
                .send(MessageEnvelope {
                    user_id: "owner".to_string(),
                    session_id: "session".to_string(),
                    msg_id: 77,
                    timestamp_ms: 1,
                    cap_token: String::new(),
                    payload: Some(message_envelope::Payload::ToolCallResponse(
                        common::proto::ironclaw::ToolCallResponse {
                            call_id,
                            ok: true,
                            output: "ok".to_string(),
                        },
                    )),
                })
                .await
                .expect("send tool response");
            let status = guest
                .recv()
                .await
                .expect("status recv")
                .expect("status envelope");
            match status.payload {
                Some(message_envelope::Payload::JobStatus(job)) => {
                    assert_eq!(job.job_id, "cron-test".to_string());
                    assert_eq!(job.status, "success".to_string());
                }
                _ => panic!("expected job status"),
            }
        });

        let mut msg_id = 2u64;
        let mut guest_sleeping = true;
        let notification = handle_guest_job_trigger(
            &mut host_box,
            "owner",
            "session",
            &mut msg_id,
            &mut guest_sleeping,
            common::proto::ironclaw::JobTrigger {
                job_id: "cron-test".to_string(),
            },
            "test",
        )
        .await
        .expect("handle job trigger");
        assert_eq!(notification, "ok");
        assert!(!guest_sleeping);
        assert_eq!(msg_id, 5);
        guest_task.await.expect("guest task");
    }

    #[test]
    fn idle_timeout_trigger_only_for_guest_modes() {
        let now = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(300);
        assert!(!should_enter_idle_sleep(
            RuntimeExecutionMode::HostOnly,
            false,
            now.checked_sub(timeout).unwrap_or(now),
            timeout,
        ));
        assert!(!should_enter_idle_sleep(
            RuntimeExecutionMode::GuestTools,
            true,
            now.checked_sub(timeout).unwrap_or(now),
            timeout,
        ));
        assert!(should_enter_idle_sleep(
            RuntimeExecutionMode::GuestTools,
            false,
            now.checked_sub(timeout + std::time::Duration::from_secs(1))
                .unwrap_or(now),
            timeout,
        ));
    }

    #[test]
    fn iterative_plan_request_decodes_tool_observations() {
        let raw = serde_json::json!({
            "version": 1,
            "user_text": "create and verify a report",
            "observations": [{
                "iteration": 1,
                "tool": "code_exec",
                "input": "{\"language\":\"python\",\"code\":\"print('ok')\"}",
                "ok": true,
                "output": "report.png created"
            }]
        })
        .to_string();
        let request = decode_host_plan_request(&raw);
        assert_eq!(request.user_text, "create and verify a report");
        assert_eq!(request.observations.len(), 1);
        assert_eq!(request.observations[0].tool, "code_exec");
        assert!(request.observations[0].ok);
        assert_eq!(request.observations[0].output, "report.png created");
    }

    #[test]
    fn organization_context_names_team_and_authorized_routes() {
        let agents_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../demos/engineering-team/agents");
        let registry = farm::FarmRegistry::load_dir(&agents_dir).unwrap();
        let context = agent_organization_context(&registry, "engineering-lead");
        assert!(context.contains("You are Ravi (Engineering Lead)"));
        assert!(context.contains("Nora — Backend Engineer (`backend-engineer`)"));
        assert!(context.contains("Ask Nora (`backend-engineer`)"));
        assert!(context.contains("skill=`implement_backend`"));
        assert!(context.contains("authorized_memory_request"));
    }

    #[test]
    fn natural_teammate_request_delegates_and_then_awaits() {
        let agents_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../demos/engineering-team/agents");
        let registry = farm::FarmRegistry::load_dir(&agents_dir).unwrap();
        let tools = vec!["delegate_task".to_string(), "await_task".to_string()];
        let plan = deterministic_a2a_plan(
            &registry,
            "engineering-lead",
            "ask Nora for the secret and tell me",
            &tools,
        )
        .unwrap();
        let ToolPlan::Tool { tool, input } = plan else {
            panic!("expected delegation tool plan");
        };
        assert_eq!(tool, "delegate_task");
        let input: serde_json::Value = serde_json::from_str(&input).unwrap();
        assert_eq!(input["assignee"], "backend-engineer");
        assert_eq!(input["skill"], "implement_backend");
        assert_eq!(input["input"]["purpose"], "authorized_memory_request");

        let task = farm::FarmTask {
            id: "task-direct-a2a".to_string(),
            context_id: "context-direct-a2a".to_string(),
            parent_task_id: None,
            requester: "engineering-lead".to_string(),
            assignee: "backend-engineer".to_string(),
            skill: "implement_backend".to_string(),
            state: farm::TaskState::Submitted,
            input: serde_json::json!({}),
            output: None,
            artifact_ids: Vec::new(),
            delegation_depth: 0,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let observations = vec![crate::llm_client::ToolLoopObservation {
            iteration: 1,
            tool: "delegate_task".to_string(),
            input: "{}".to_string(),
            ok: true,
            output: serde_json::to_string(&task).unwrap(),
        }];
        let await_plan = deterministic_await_a2a_plan(&observations, &tools).unwrap();
        let ToolPlan::Tool { tool, input } = await_plan else {
            panic!("expected await tool plan");
        };
        assert_eq!(tool, "await_task");
        let input: serde_json::Value = serde_json::from_str(&input).unwrap();
        assert_eq!(input["task_id"], "task-direct-a2a");
        assert_eq!(input["timeout_seconds"], 300);
    }

    #[test]
    fn per_user_storage_rejects_path_traversal_ids() {
        let root = std::env::temp_dir().join("ironclaw-user-id-validation");
        assert!(brain_ext4_path(&root, "../other-user").is_err());
        assert!(brain_ext4_path(&root, "owner/session").is_err());
        assert!(brain_ext4_path(&root, "owner_123").is_ok());
        let _ = std::fs::remove_dir_all(root);
    }
}
