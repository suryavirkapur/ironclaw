use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};

mod auth_transport;
mod host_tools;
mod llm_client;

use auth_transport::AuthenticatedTransport;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use common::config::{HostConfig, HostExecutionMode};
#[cfg(feature = "firecracker")]
use common::firecracker::{FirecrackerManager, FirecrackerManagerConfig};
use common::firecracker::{StubVmManager, VmConfig, VmInstance, VmManager};
use common::proto::ironclaw::{message_envelope, MessageEnvelope};
use futures::{SinkExt, StreamExt};
use host_tools::{run_host_tool, truncate_tool_output};
use include_dir::{include_dir, Dir};
use llm_client::{LlmClient, ToolPlan};
use serde::Deserialize;
use serde::Serialize;
use std::cmp::min;
use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

static UI_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../ui");
const TELEGRAM_CHUNK_MAX_CHARS: usize = 4096;

#[tokio::main]
async fn main() -> Result<(), IronclawError> {
    let config = load_host_config()?;
    let run_telegram = should_enable_telegram(&config);
    let telegram_settings = if run_telegram {
        Some(TelegramSettings::from_config(&config)?)
    } else {
        None
    };
    let addr = format!("{}:{}", config.server.bind, config.server.port);
    let state = AppState::new(config)?;
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/ui", get(ui_index_handler))
        .route("/ui/{*path}", get(ui_asset_handler))
        .with_state(state.clone());

    tracing_subscriber::fmt::init();
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|err| IronclawError::new(format!("bind failed: {err}")))?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let shutdown_server_rx = shutdown_rx.clone();

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
}

impl AppState {
    fn new(config: HostConfig) -> Result<Self, IronclawError> {
        let llm_client = Arc::new(
            LlmClient::new(config.llm.clone())
                .map_err(|err| IronclawError::new(format!("llm client init failed: {err}")))?,
        );
        let local_guest = !config.firecracker.enabled;
        let guest_config_path = Arc::new(guest_config_path());
        let (vm_manager, stub_vm_manager) = if config.firecracker.enabled {
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
                return Err(IronclawError::new(
                    "firecracker enabled but feature is disabled",
                ));
            }
        } else {
            let stub_vm_manager = Arc::new(StubVmManager::new(32));
            let vm_manager: Arc<dyn VmManager> = stub_vm_manager.clone();
            (vm_manager, Some(stub_vm_manager))
        };
        let execution_mode = RuntimeExecutionMode::from_config(&config);
        Ok(Self {
            host_config: Arc::new(config),
            llm_client,
            vm_manager,
            guest_config_path,
            local_guest,
            stub_vm_manager,
            execution_mode,
        })
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

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, query))
}

async fn handle_socket(socket: WebSocket, state: AppState, query: WsQuery) {
    let user_id = query.user_id.unwrap_or_else(|| "local".to_string());
    let session_id = query.session_id.unwrap_or_else(|| "session".to_string());
    let (vm_instance, guest_transport) = match start_vm_pair(&state, &user_id).await {
        Ok(pair) => pair,
        Err(err) => {
            tracing::error!("vm start failed: {err}");
            return;
        }
    };

    // host tools used only in host-only mode and explicit host fallbacks.
    let host_allowed_tools = vec![
        "bash".to_string(),
        "file_read".to_string(),
        "file_write".to_string(),
    ];
    let guest_allowed_tools = vec!["file_read".to_string(), "file_write".to_string()];

    let cap_token = {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    };

    let mut transport = vm_instance.transport;

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

    loop {
        tokio::select! {
            ws_msg = receiver.next() => {
                let Some(Ok(message)) = ws_msg else { break; };
                if let Message::Text(text) = message {
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
                        // Handle host-side tools requested by the guest.
                        if let Some(message_envelope::Payload::ToolCallRequest(req)) =
                            envelope.payload.clone()
                        {
                            let (ok, output) = if req.tool == "host_plan" {
                                match host_plan_tool_response(
                                    &state,
                                    &req.input,
                                    &host_allowed_tools,
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
        }
    }
}

async fn start_vm_pair(
    state: &AppState,
    user_id: &str,
) -> Result<(VmInstance, Option<common::transport::LocalTransport>), IronclawError> {
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
            return Ok((instance, Some(guest)));
        }
    }
    let instance = state
        .vm_manager
        .start_vm(config)
        .await
        .map_err(|err| IronclawError::new(err.to_string()))?;
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
    user_text: &str,
    allowed_tools: &[String],
) -> Result<String, String> {
    if let Some(plan) = deterministic_guest_tools_plan(user_text, allowed_tools) {
        return tool_plan_to_json(&plan);
    }

    let plan = match state
        .llm_client
        .plan_tool_or_answer(user_text, allowed_tools)
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
) -> Result<String, IronclawError> {
    let plan = state
        .llm_client
        .plan_tool_or_answer(user_text, allowed_tools)
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
                .finalize_with_tool_output(user_text, &tool, &input, ok, &output)
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
}

impl TelegramSession {
    fn new(chat_id: i64) -> Self {
        Self {
            chat_id,
            user_id: format!("telegram-{chat_id}"),
            session_id: format!("telegram-{chat_id}"),
            msg_id: 1,
            transport: None,
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
                    if message.chat.id != settings.owner_chat_id {
                        tracing::warn!("telegram message denied from chat {}", message.chat.id);
                        continue;
                    }
                    let session = sessions
                        .entry(message.chat.id)
                        .or_insert_with(|| TelegramSession::new(message.chat.id));
                    if let Err(err) =
                        handle_telegram_text(&state, &client, session, text.as_str()).await
                    {
                        tracing::error!("telegram message handling failed: {err}");
                        let _ = client.send_message(session.chat_id, "request failed").await;
                    }
                }
            }
            Err(err) => {
                tracing::error!("telegram polling failed: {err}");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
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
    let host_allowed_tools = vec![
        "bash".to_string(),
        "file_read".to_string(),
        "file_write".to_string(),
    ];
    if state.execution_mode == RuntimeExecutionMode::HostOnly {
        let output = run_host_turn(state, &session.user_id, &host_allowed_tools, text).await?;
        send_stream_to_telegram(client, session.chat_id, &output).await?;
        return Ok(());
    }

    if session.transport.is_none() {
        let (vm_instance, guest_transport) = start_vm_pair(state, &session.user_id).await?;
        let guest_allowed_tools = vec![
            "file_read".to_string(),
            "file_write".to_string(),
            "bash".to_string(),
        ];
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
                        irowclaw::runtime::run_with_transport(guest_transport, guest_config_path)
                            .await
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

    let host_allowed_tools = vec![
        "bash".to_string(),
        "file_read".to_string(),
        "file_write".to_string(),
    ];

    let mut streamed_any = false;
    loop {
        let maybe = transport
            .recv()
            .await
            .map_err(|err| IronclawError::new(format!("transport recv failed: {err}")))?;
        let Some(envelope) = maybe else {
            return Err(IronclawError::new("guest transport closed"));
        };
        if let Some(message_envelope::Payload::ToolCallRequest(req)) = envelope.payload.clone() {
            let (ok, output) = if req.tool == "host_plan" {
                match host_plan_tool_response(state, &req.input, &host_allowed_tools).await {
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
    }

    Ok(())
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

#[cfg(test)]
mod tests {
    use super::{
        deterministic_guest_tools_plan, load_telegram_offset, save_telegram_offset,
        split_telegram_chunks, ToolPlan,
    };

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
}
