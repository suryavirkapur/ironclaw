use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use common::config::HostConfig;
use common::firecracker::{StubVmManager, VmConfig, VmInstance, VmManager};
#[cfg(feature = "firecracker")]
use common::firecracker::{FirecrackerManager, FirecrackerManagerConfig};
use common::protocol::{MessageEnvelope, MessagePayload};
use futures::{SinkExt, StreamExt};
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

static UI_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../ui");

#[tokio::main]
async fn main() -> Result<(), IronclawError> {
    let config = load_host_config()?;
    let addr = format!("{}:{}", config.server.bind, config.server.port);
    let state = AppState::new(config)?;
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/ui", get(ui_index_handler))
        .route("/ui/*path", get(ui_asset_handler))
        .with_state(state);

    tracing_subscriber::fmt::init();
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|err| IronclawError::new(format!("bind failed: {err}")))?;
    axum::serve(listener, app)
        .await
        .map_err(|err| IronclawError::new(format!("server failed: {err}")))
}

#[derive(Clone)]
struct AppState {
    host_config: Arc<HostConfig>,
    vm_manager: Arc<dyn VmManager>,
    guest_config_path: Arc<PathBuf>,
    local_guest: bool,
    stub_vm_manager: Option<Arc<StubVmManager>>,
}

impl AppState {
    fn new(config: HostConfig) -> Result<Self, IronclawError> {
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
        Ok(Self {
            host_config: Arc::new(config),
            vm_manager,
            guest_config_path,
            local_guest,
            stub_vm_manager,
        })
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

    let transport = vm_instance.transport;
    if state.local_guest {
        if let Some(guest_transport) = guest_transport {
            tokio::spawn(async move {
                if let Err(err) = irowclaw::runtime::run_with_transport(
                    guest_transport,
                    (*state.guest_config_path).clone(),
                )
                .await
                {
                    tracing::error!("guest runtime failed: {err}");
                }
            });
        }
    }

    let (mut sender, mut receiver) = socket.split();
    let transport = Arc::new(Mutex::new(transport));
    let inbound = transport.clone();
    let outbound = transport.clone();

    let mut inbound_task = tokio::spawn(async move {
        let mut msg_id = 1u64;
        while let Some(Ok(message)) = receiver.next().await {
            if let Message::Text(text) = message {
                let timestamp_ms = match now_ms() {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::warn!("time error: {err}");
                        0
                    }
                };
                let envelope = MessageEnvelope {
                    user_id: user_id.clone(),
                    session_id: session_id.clone(),
                    msg_id,
                    timestamp_ms,
                    payload: MessagePayload::UserMessage {
                        text: text.to_string(),
                    },
                };
                msg_id += 1;
                let mut guard = inbound.lock().await;
                if let Err(err) = guard.send(envelope).await {
                    tracing::error!("transport send failed: {err}");
                    break;
                }
            }
        }
    });

    let mut outbound_task = tokio::spawn(async move {
        loop {
            let mut guard = outbound.lock().await;
            let message = guard.recv().await;
            drop(guard);
            match message {
                Ok(Some(envelope)) => {
                    match serde_json::to_string(&envelope) {
                        Ok(payload) => {
                            if sender.send(Message::Text(payload.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(err) => {
                            tracing::error!("serialize ws message failed: {err}");
                            break;
                        }
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    tracing::error!("transport recv failed: {err}");
                    break;
                }
            }
        }
    });

    tokio::select! {
        _ = (&mut inbound_task) => {},
        _ = (&mut outbound_task) => {},
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
    let path = if path.is_empty() { "index.html" } else { path.as_str() };
    ui_file_response(path)
}

fn ui_file_response(path: &str) -> Response {
    match UI_DIR.get_file(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let mut response = Response::new(file.contents().into());
            response
                .headers_mut()
                .insert("content-type", HeaderValue::from_str(mime.as_ref()).unwrap_or_else(|_| {
                    HeaderValue::from_static("application/octet-stream")
                }));
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
