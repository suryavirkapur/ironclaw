use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};

mod auth_transport;
mod host_tools;
mod llm_client;
mod soul_guard;
mod whatsapp;

use auth_transport::AuthenticatedTransport;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use common::config::{GuestConfig, HostConfig, HostExecutionMode};
#[cfg(feature = "firecracker")]
use common::firecracker::{FirecrackerManager, FirecrackerManagerConfig};
use common::firecracker::{StubVmManager, VmConfig, VmInstance, VmManager};
use common::proto::ironclaw::{agent_control, message_envelope, AgentControl, MessageEnvelope};
use futures::{SinkExt, StreamExt};
use host_tools::{run_host_tool, truncate_tool_output};
use include_dir::{include_dir, Dir};
use llm_client::{ConversationMessage, LlmClient, ToolPlan};
use memory::{
    build_memory_block, forget_memories_by_query, forget_memory_by_id, initialize_schema,
    list_pinned_memories, maybe_summarize_session, redact_secrets, retrieve_memories,
    upsert_memory, NewMemory,
};
use rusqlite::Connection;
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
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use whatsapp::should_enable_whatsapp;

static UI_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../ui");
const TELEGRAM_CHUNK_MAX_CHARS: usize = 4096;
const TELEGRAM_TRANSCRIPT_MAX_TURNS: usize = 50;
const TELEGRAM_RETRY_MAX_ATTEMPTS: usize = 2;
const MEMORY_RETRIEVAL_LIMIT: usize = 10;
const MEMORY_PROMPT_BUDGET_CHARS: usize = 3200;
const IDLE_CHECK_SECONDS: u64 = 10;

#[tokio::main]
async fn main() -> Result<(), IronclawError> {
    let config = load_host_config()?;
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
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/ui", get(ui_index_handler))
        .route("/ui/{*path}", get(ui_asset_handler))
        .route("/api/soul-guard/pending", get(soul_guard_pending_handler))
        .route(
            "/api/soul-guard/decision",
            post(soul_guard_decision_handler),
        )
        .with_state(state.clone());

    tracing_subscriber::fmt::init();
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

    let server_task = tokio::spawn(async move {
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

    let telegram_task = if let Some(settings) = telegram_settings {
        let telegram_state = state.clone();
        let rx = shutdown_rx.clone();
        Some(tokio::spawn(async move {
            run_telegram_loop(telegram_state, settings, rx).await
        }))
    } else {
        None
    };

    let whatsapp_task = if let Some(wa_config) = whatsapp_config {
        let whatsapp_state = state.clone();
        let rx = shutdown_rx.clone();
        Some(tokio::spawn(async move {
            run_whatsapp_loop(whatsapp_state, wa_config, rx).await
        }))
    } else {
        None
    };

    tokio::select! {
        result = server_task => {
            let _ = shutdown_tx.send(true);
            match result {
                Ok(value) => value,
                Err(err) => Err(IronclawError::new(format!("server task join failed: {err}"))),
            }
        }
        result = async {
            if let Some(task) = telegram_task {
                match task.await {
                    Ok(value) => value,
                    Err(err) => {
                        Err(IronclawError::new(format!(
                            "telegram task join failed: {err}"
                        )))
                    }
                }
            } else {
                std::future::pending::<Result<(), IronclawError>>().await
            }
        } => {
            let _ = shutdown_tx.send(true);
            result
        }
        result = async {
            if let Some(task) = whatsapp_task {
                match task.await {
                    Ok(value) => value,
                    Err(err) => {
                        Err(IronclawError::new(format!(
                            "whatsapp task join failed: {err}"
                        )))
                    }
                }
            } else {
                std::future::pending::<Result<(), IronclawError>>().await
            }
        } => {
            let _ = shutdown_tx.send(true);
            result
        }
        signal = tokio::signal::ctrl_c() => {
            let _ = signal;
            let _ = shutdown_tx.send(true);
            Ok(())
        }
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
    soul_guard_db_path: Arc<PathBuf>,
}

impl AppState {
    fn new(config: HostConfig) -> Result<Self, IronclawError> {
        let llm_client = Arc::new(
            LlmClient::new(config.llm.clone())
                .map_err(|err| IronclawError::new(format!("llm client init failed: {err}")))?,
        );
        let firecracker_runtime_enabled =
            config.firecracker.enabled && cfg!(feature = "firecracker");
        if config.firecracker.enabled && !cfg!(feature = "firecracker") {
            eprintln!("firecracker requested but feature is disabled; falling back to local guest");
        }
        let local_guest = !firecracker_runtime_enabled;
        let guest_config_path = Arc::new(guest_config_path());
        let guest_allow_bash = load_guest_allow_bash(&guest_config_path);
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
        Ok(Self {
            host_config: Arc::new(config),
            llm_client,
            vm_manager,
            guest_config_path,
            local_guest,
            stub_vm_manager,
            execution_mode,
            guest_allow_bash,
            soul_guard_db_path: Arc::new(soul_guard_db),
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

#[derive(Debug, thiserror::Error)]
#[error("ironclawd error: {message}")]
pub struct IronclawError {
    message: String,
}

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
        ChannelSource::Telegram | ChannelSource::WhatsApp => {
            inbound_user_id.unwrap_or("owner").to_string()
        }
    }
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, query))
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
    let host_allowed_tools = allowed_tools_for_runtime(state.local_guest, state.guest_allow_bash);
    let guest_allowed_tools = allowed_tools_for_runtime(state.local_guest, state.guest_allow_bash);

    let cap_token = {
        use rand::RngCore;
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
            let guest_config_path = (*state.guest_config_path).clone();
            tokio::spawn(async move {
                // Local guest mode runs irowclaw in-process. Use a writable brain root.
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
    // Send AuthChallenge and wait for AuthAck before starting WS bridge.
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
            })),
        };
        if let Err(err) = transport.send(challenge).await {
            tracing::error!("auth challenge send failed: {err}");
            return;
        }

        match tokio::time::timeout(std::time::Duration::from_secs(5), transport.recv()).await {
            Ok(Ok(Some(msg))) => match msg.payload {
                Some(message_envelope::Payload::AuthAck(ack)) if ack.cap_token == cap_token => {}
                other => {
                    tracing::error!("invalid auth ack: {other:?}");
                    return;
                }
            },
            Ok(Ok(None)) => return,
            Ok(Err(err)) => {
                tracing::error!("auth ack recv failed: {err}");
                return;
            }
            Err(_) => {
                tracing::error!("auth ack timed out");
                return;
            }
        }

        transport = Box::new(AuthenticatedTransport::new(transport, cap_token.clone()));
    }

    let (mut sender, mut receiver) = socket.split();

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
                            let (ok, output) = if req.tool == "host_plan" {
                                match host_plan_tool_response(
                                    &state,
                                    &tool_user_id,
                                    &req.input,
                                    &host_allowed_tools,
                                    None,
                                )
                                .await {
                                    Ok(out) => (true, out),
                                    Err(err) => (false, truncate_tool_output(&err)),
                                }
                            } else {
                                let result = run_host_tool(
                                    &host_allowed_tools,
                                    &tool_user_id,
                                    &req.tool,
                                    &req.input,
                                )
                                .await;
                                match result {
                                    Ok(out) => (true, truncate_tool_output(&out)),
                                    Err(err) => (false, truncate_tool_output(&err)),
                                }
                            };

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
) -> Result<(), IronclawError> {
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
    let status_text =
        run_scheduled_job_via_guest(transport, user_id, session_id, call_id, &trigger.job_id)
            .await
            .unwrap_or_else(|err| {
                tracing::error!("scheduled job execution failed: {err}");
                "failed".to_string()
            });

    let status_envelope = MessageEnvelope {
        user_id: user_id.to_string(),
        session_id: session_id.to_string(),
        msg_id: *msg_id,
        timestamp_ms: now_ms().unwrap_or(0),
        cap_token: String::new(),
        payload: Some(message_envelope::Payload::JobStatus(
            common::proto::ironclaw::JobStatus {
                job_id: trigger.job_id,
                status: status_text,
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
    Ok(())
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
                    return Ok("success".to_string());
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
    let config = VmConfig {
        user_id: user_id.to_string(),
        brain_path,
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

fn load_host_config() -> Result<HostConfig, IronclawError> {
    let path = host_config_path()?;
    if path.exists() {
        let contents = std::fs::read_to_string(&path)
            .map_err(|err| IronclawError::new(format!("config read failed: {err}")))?;
        let config: HostConfig = toml::from_str(&contents)
            .map_err(|err| IronclawError::new(format!("config parse failed: {err}")))?;
        Ok(config)
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

fn brain_ext4_path(root: &StdPath, user_id: &str) -> Result<PathBuf, IronclawError> {
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

fn open_memory_db(state: &AppState, user_id: &str) -> Result<Connection, IronclawError> {
    let db_path = brain_db_path(&state.host_config.storage.users_root, user_id)?;
    let conn = Connection::open(db_path)
        .map_err(|err| IronclawError::new(format!("memory db open failed: {err}")))?;
    initialize_schema(&conn)
        .map_err(|err| IronclawError::new(format!("memory db schema failed: {err}")))?;
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
    user_text: &str,
    allowed_tools: &[String],
    history: Option<&[ConversationMessage]>,
) -> Result<String, String> {
    if let Some(plan) = deterministic_guest_tools_plan(user_text, allowed_tools) {
        return tool_plan_to_json(&plan);
    }

    let memory_block = load_memory_block(state, user_id, user_text, MEMORY_PROMPT_BUDGET_CHARS)
        .map_err(|err| format!("memory retrieval failed: {err}"))?;

    let plan = match state
        .llm_client
        .plan_tool_or_answer(
            user_text,
            allowed_tools,
            Some(memory_block.as_str()),
            history,
        )
        .await
    {
        Ok(plan) => plan,
        Err(err) => {
            // GuestTools mode should remain usable even when no LLM key is configured.
            // Fall back to a deterministic stub answer instead of failing the whole guest loop.
            let msg = err.to_string();
            if msg.contains("missing openai_api_key") {
                ToolPlan::Answer {
                    text: format!("stub: {}", user_text.trim()),
                }
            } else {
                return Err(format!("host plan failed: {err}"));
            }
        }
    };

    tool_plan_to_json(&plan)
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
            let tool_result = run_host_tool(allowed_tools, user_id, &tool, &input).await;
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
    chat: TelegramChat,
}

#[derive(Clone, Deserialize)]
struct TelegramChat {
    id: i64,
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

    async fn send_message(&self, chat_id: i64, text: &str) -> Result<(), IronclawError> {
        let request = TelegramSendMessageRequest { chat_id, text };
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
    fn load(chat_id: i64, users_root: &StdPath) -> Result<Self, IronclawError> {
        let session_id = format!("telegram-{chat_id}");
        let transcript_path = telegram_transcript_path(users_root, &session_id);
        let transcript = load_telegram_transcript(&transcript_path)?;
        Ok(Self {
            chat_id,
            user_id: resolve_owner_user_id(ChannelSource::Telegram, None),
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
    tracing::info!(
        "telegram loop started owner_chat_id={} offset_file={}",
        settings.owner_chat_id,
        settings.offset_file.display()
    );
    let client = TelegramClient::new(settings.bot_token.clone());
    let mut offset = load_telegram_offset(&settings.offset_file)?;
    let mut sessions = HashMap::<i64, TelegramSession>::new();

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
                    let Some(text) = message.text else {
                        continue;
                    };
                    tracing::debug!(
                        "channel route source=telegram chat_id={} event=ingress",
                        message.chat.id
                    );
                    if message.chat.id != settings.owner_chat_id {
                        tracing::warn!("telegram message denied from chat {}", message.chat.id);
                        continue;
                    }
                    if !sessions.contains_key(&message.chat.id) {
                        let session = TelegramSession::load(
                            message.chat.id,
                            &state.host_config.storage.users_root,
                        )?;
                        sessions.insert(message.chat.id, session);
                    }
                    let Some(session) = sessions.get_mut(&message.chat.id) else {
                        continue;
                    };
                    if let Err(err) =
                        handle_telegram_text(&state, &client, session, text.as_str()).await
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

        if let Err(err) = enforce_telegram_idle_timeouts(&state, &mut sessions).await {
            tracing::warn!("telegram idle timeout check failed: {err}");
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

async fn handle_telegram_text(
    state: &AppState,
    client: &TelegramClient,
    session: &mut TelegramSession,
    text: &str,
) -> Result<(), IronclawError> {
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
        match handle_telegram_text_once(state, client, session, text, &history).await {
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
    history: &[ConversationMessage],
) -> Result<String, IronclawError> {
    if let Some(message) =
        telegram_firecracker_requirement_error(state.execution_mode, state.local_guest)
    {
        send_stream_to_telegram(client, session.chat_id, message).await?;
        return Ok(message.to_string());
    }

    let host_allowed_tools = allowed_tools_for_runtime(state.local_guest, state.guest_allow_bash);
    if state.execution_mode == RuntimeExecutionMode::HostOnly {
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
        .ok_or_else(|| IronclawError::new("missing telegram session transport"))?;
    transport
        .send(envelope)
        .await
        .map_err(|err| IronclawError::new(format!("send to guest failed: {err}")))?;

    let host_allowed_tools = allowed_tools_for_runtime(state.local_guest, state.guest_allow_bash);

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
                "telegram",
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
            let (ok, output) = if req.tool == "host_plan" {
                match host_plan_tool_response(
                    state,
                    &session.user_id,
                    &req.input,
                    &host_allowed_tools,
                    Some(history),
                )
                .await
                {
                    Ok(out) => (true, out),
                    Err(err) => (false, truncate_tool_output(&err)),
                }
            } else {
                match run_host_tool(&host_allowed_tools, &session.user_id, &req.tool, &req.input)
                    .await
                {
                    Ok(out) => (true, truncate_tool_output(&out)),
                    Err(err) => (false, truncate_tool_output(&err)),
                }
            };
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
                send_stream_to_telegram(client, session.chat_id, &delta.delta).await?;
                streamed_any = true;
            }
            if delta.done {
                break;
            }
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
    let guest_allowed_tools = allowed_tools_for_runtime(state.local_guest, state.guest_allow_bash);
    let cap_token = {
        use rand::RngCore;
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

fn load_guest_allow_bash(config_path: &StdPath) -> bool {
    let raw = match std::fs::read_to_string(config_path) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                "guest config read failed at {}: {}",
                config_path.display(),
                err
            );
            return false;
        }
    };
    match toml::from_str::<GuestConfig>(&raw) {
        Ok(config) => config.tools.allow_bash,
        Err(err) => {
            tracing::warn!(
                "guest config parse failed at {}: {}",
                config_path.display(),
                err
            );
            false
        }
    }
}

fn allowed_tools_for_runtime(local_guest: bool, guest_allow_bash: bool) -> Vec<String> {
    let mut tools = vec!["file_read".to_string(), "file_write".to_string()];
    if bash_allowed(local_guest, guest_allow_bash) {
        tools.push("bash".to_string());
    }
    tools
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
        client.send_message(chat_id, &chunk).await?;
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
        let end = min(chars.len(), index.saturating_add(max_chars));
        out.push(chars[index..end].iter().collect::<String>());
        index = end;
    }
    out
}

fn telegram_transcript_path(users_root: &StdPath, session_id: &str) -> PathBuf {
    users_root.join(session_id).join("telegram.transcript.json")
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
    tracing::info!(
        "whatsapp: starting bot (session_db={})",
        config.session_db_path
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
                            session.user_id =
                                resolve_owner_user_id(ChannelSource::WhatsApp, None);
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

    let host_allowed_tools = allowed_tools_for_runtime(state.local_guest, state.guest_allow_bash);
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
            let (ok, output) = if req.tool == "host_plan" {
                match host_plan_tool_response(
                    state,
                    &session.user_id,
                    &req.input,
                    &host_allowed_tools,
                    Some(history),
                )
                .await
                {
                    Ok(out) => (true, out),
                    Err(err) => (false, truncate_tool_output(&err)),
                }
            } else {
                match run_host_tool(&host_allowed_tools, &session.user_id, &req.tool, &req.input)
                    .await
                {
                    Ok(out) => (true, truncate_tool_output(&out)),
                    Err(err) => (false, truncate_tool_output(&err)),
                }
            };
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
    let guest_allowed_tools = allowed_tools_for_runtime(state.local_guest, state.guest_allow_bash);
    let cap_token = {
        use rand::RngCore;
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
        allowed_tools_for_runtime, deterministic_guest_tools_plan, handle_guest_job_trigger,
        load_telegram_offset, load_telegram_transcript, resolve_owner_user_id,
        save_telegram_offset, save_telegram_transcript, should_enter_idle_sleep,
        split_telegram_chunks, telegram_firecracker_requirement_error, telegram_transcript_path,
        ChannelSource, RuntimeExecutionMode, TelegramTranscript, TelegramTranscriptMessage,
        ToolPlan, TELEGRAM_TRANSCRIPT_MAX_TURNS,
    };
    use common::proto::ironclaw::{message_envelope, MessageEnvelope};
    use common::transport::{LocalTransport, Transport};

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
        let local_tools = allowed_tools_for_runtime(true, true);
        assert!(!local_tools.iter().any(|tool| tool == "bash"));

        let firecracker_without_bash = allowed_tools_for_runtime(false, false);
        assert!(!firecracker_without_bash.iter().any(|tool| tool == "bash"));

        let firecracker_with_bash = allowed_tools_for_runtime(false, true);
        assert!(firecracker_with_bash.iter().any(|tool| tool == "bash"));
    }

    #[test]
    fn channel_owner_identity_routes_across_sources() {
        assert_eq!(
            resolve_owner_user_id(ChannelSource::Telegram, None),
            "owner".to_string()
        );
        assert_eq!(
            resolve_owner_user_id(ChannelSource::WhatsApp, None),
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
        handle_guest_job_trigger(
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
}
