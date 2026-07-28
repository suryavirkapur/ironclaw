use common::proto::ironclaw::{message_envelope, Artifact, AuthAck, MessageEnvelope, UploadedFile};
use futures::{SinkExt, StreamExt};
use prost::Message as ProstMessage;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const DEFAULT_URL: &str = "ws://127.0.0.1:9938/ws";
const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_UPLOAD_BYTES: u64 = 8 * 1024 * 1024;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type CliResult<T> = Result<T, CliError>;

#[derive(Debug)]
struct CliError(String);

impl CliError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl Display for CliError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for CliError {}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConnectionOptions {
    url: String,
    user_id: String,
    session_id: String,
    timeout_secs: u64,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            url: DEFAULT_URL.to_string(),
            user_id: "cli".to_string(),
            session_id: "cli-session".to_string(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Chat(ConnectionOptions),
    Ask(ConnectionOptions, String, Option<PathBuf>),
    Doctor(ConnectionOptions),
    Help,
}

struct IronclawClient {
    socket: Socket,
    timeout: Duration,
    user_id: String,
    session_id: String,
    cap_token: String,
}

#[derive(Debug, PartialEq)]
enum Reply {
    Text(String),
    Tool { ok: bool, output: String },
    Artifact(Artifact),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    match parse_args(std::env::args().skip(1).collect())? {
        Command::Chat(options) => run_chat(options).await?,
        Command::Ask(options, prompt, file) => {
            let mut client = IronclawClient::connect(&options).await?;
            let reply = if let Some(path) = file {
                client.send_file(&path, &prompt).await?
            } else {
                client.send(&prompt).await?
            };
            print_reply(reply)?;
        }
        Command::Doctor(options) => run_doctor(options).await?,
        Command::Help => print_usage(),
    }
    Ok(())
}

impl IronclawClient {
    async fn connect(options: &ConnectionOptions) -> CliResult<Self> {
        let url = websocket_url(options);
        let connect = tokio::time::timeout(
            Duration::from_secs(options.timeout_secs),
            tokio_tungstenite::connect_async(&url),
        )
        .await
        .map_err(|_| CliError::new(format!("timed out connecting to {url}")))?
        .map_err(|error| CliError::new(format!("connect to {url} failed: {error}")))?;

        let (mut socket, _) = connect;
        let challenge = receive_envelope(
            &mut socket,
            Duration::from_secs(options.timeout_secs),
            "Firecracker authentication challenge",
        )
        .await?;

        let auth = match challenge.payload {
            Some(message_envelope::Payload::AuthChallenge(auth)) => auth,
            other => {
                return Err(CliError::new(format!(
                    "expected auth challenge, received {other:?}"
                )));
            }
        };
        validate_execution_mode(&auth.execution_mode)?;

        let ack = MessageEnvelope {
            user_id: challenge.user_id,
            session_id: challenge.session_id,
            msg_id: challenge.msg_id,
            timestamp_ms: now_ms(),
            cap_token: auth.cap_token.clone(),
            payload: Some(message_envelope::Payload::AuthAck(AuthAck {
                cap_token: auth.cap_token.clone(),
            })),
        };
        send_envelope(&mut socket, &ack).await?;

        Ok(Self {
            socket,
            timeout: Duration::from_secs(options.timeout_secs),
            user_id: options.user_id.clone(),
            session_id: options.session_id.clone(),
            cap_token: auth.cap_token,
        })
    }

    async fn send(&mut self, prompt: &str) -> CliResult<Reply> {
        self.socket
            .send(Message::Text(prompt.to_string().into()))
            .await
            .map_err(|error| CliError::new(format!("send failed: {error}")))?;

        self.receive_reply().await
    }

    async fn send_file(&mut self, path: &Path, prompt: &str) -> CliResult<Reply> {
        let metadata = std::fs::metadata(path)
            .map_err(|error| CliError::new(format!("inspect upload failed: {error}")))?;
        if !metadata.is_file() {
            return Err(CliError::new("upload path is not a regular file"));
        }
        if metadata.len() > MAX_UPLOAD_BYTES {
            return Err(CliError::new(format!(
                "file exceeds {MAX_UPLOAD_BYTES} byte upload limit"
            )));
        }
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| CliError::new("upload filename is invalid"))?;
        let data = std::fs::read(path)
            .map_err(|error| CliError::new(format!("read upload failed: {error}")))?;
        let mime_type = mime_guess::from_path(path)
            .first_raw()
            .unwrap_or("application/octet-stream");
        let envelope = MessageEnvelope {
            user_id: self.user_id.clone(),
            session_id: self.session_id.clone(),
            msg_id: now_ms(),
            timestamp_ms: now_ms(),
            cap_token: self.cap_token.clone(),
            payload: Some(message_envelope::Payload::UploadedFile(UploadedFile {
                filename: filename.to_string(),
                mime_type: mime_type.to_string(),
                data,
                prompt: prompt.to_string(),
            })),
        };
        let mut encoded = Vec::with_capacity(envelope.encoded_len());
        envelope
            .encode(&mut encoded)
            .map_err(|error| CliError::new(format!("encode upload failed: {error}")))?;
        self.socket
            .send(Message::Binary(encoded.into()))
            .await
            .map_err(|error| CliError::new(format!("send upload failed: {error}")))?;
        self.receive_reply().await
    }

    async fn receive_reply(&mut self) -> CliResult<Reply> {
        let mut response = String::new();
        loop {
            let envelope =
                receive_envelope(&mut self.socket, self.timeout, "Ironclaw response").await?;
            match envelope.payload {
                Some(message_envelope::Payload::StreamDelta(delta)) => {
                    response.push_str(&delta.delta);
                    if delta.done {
                        return Ok(Reply::Text(response));
                    }
                }
                Some(message_envelope::Payload::ToolCallResponse(result)) => {
                    return Ok(Reply::Tool {
                        ok: result.ok,
                        output: result.output,
                    });
                }
                Some(message_envelope::Payload::Artifact(artifact)) => {
                    return Ok(Reply::Artifact(artifact));
                }
                Some(message_envelope::Payload::AgentState(_)) => {}
                Some(other) => {
                    return Err(CliError::new(format!(
                        "unexpected response payload: {other:?}"
                    )));
                }
                None => return Err(CliError::new("response had no payload")),
            }
        }
    }
}

async fn run_chat(options: ConnectionOptions) -> CliResult<()> {
    let mut client = IronclawClient::connect(&options).await?;
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();

    stdout
        .write_all(
            b"Connected to Ironclaw through the Firecracker guest sandbox.\n\
Type /file <PATH> to analyze a file, /doctor to test sandboxed file I/O, or /quit.\n\n",
        )
        .await
        .map_err(io_error)?;

    loop {
        stdout.write_all(b"you> ").await.map_err(io_error)?;
        stdout.flush().await.map_err(io_error)?;
        let Some(line) = lines.next_line().await.map_err(io_error)? else {
            break;
        };
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if matches!(input, "/quit" | "/exit") {
            break;
        }
        if input == "/doctor" {
            doctor_roundtrip(&mut client).await?;
            stdout
                .write_all(b"doctor> PASS: Firecracker guest file write/read round-trip\n")
                .await
                .map_err(io_error)?;
            continue;
        }
        if let Some(raw_path) = input.strip_prefix("/file ") {
            let path = PathBuf::from(raw_path.trim());
            match client
                .send_file(
                    &path,
                    "Analyze this file and report the important findings.",
                )
                .await?
            {
                Reply::Text(text) => {
                    stdout
                        .write_all(format!("ironclaw> {text}\n").as_bytes())
                        .await
                        .map_err(io_error)?;
                }
                Reply::Tool { ok, output } => {
                    stdout
                        .write_all(format!("tool(ok={ok})> {output}\n").as_bytes())
                        .await
                        .map_err(io_error)?;
                }
                Reply::Artifact(artifact) => {
                    let path = save_artifact(&artifact)?;
                    stdout
                        .write_all(format!("artifact> {}\n", path.display()).as_bytes())
                        .await
                        .map_err(io_error)?;
                }
            }
            continue;
        }

        match client.send(input).await? {
            Reply::Text(text) => {
                stdout
                    .write_all(format!("ironclaw> {text}\n").as_bytes())
                    .await
                    .map_err(io_error)?;
            }
            Reply::Tool { ok, output } => {
                stdout
                    .write_all(format!("tool(ok={ok})> {output}\n").as_bytes())
                    .await
                    .map_err(io_error)?;
            }
            Reply::Artifact(artifact) => {
                let path = save_artifact(&artifact)?;
                stdout
                    .write_all(
                        format!(
                            "artifact> {} ({}, {} bytes){}\n",
                            path.display(),
                            artifact.mime_type,
                            artifact.data.len(),
                            artifact_caption_suffix(&artifact.caption)
                        )
                        .as_bytes(),
                    )
                    .await
                    .map_err(io_error)?;
            }
        }
    }
    Ok(())
}

async fn run_doctor(options: ConnectionOptions) -> CliResult<()> {
    let mut client = IronclawClient::connect(&options).await?;
    doctor_roundtrip(&mut client).await?;
    println!("PASS: authenticated Firecracker guest file write/read round-trip");
    Ok(())
}

async fn doctor_roundtrip(client: &mut IronclawClient) -> CliResult<()> {
    let marker = format!("ironclaw-doctor-{}-{}", std::process::id(), now_ms());
    let path = format!(".doctor/{marker}.txt");
    let write = format!("!toolcall file_write\n{path}\n{marker}");
    match client.send(&write).await? {
        Reply::Tool { ok: true, .. } => {}
        reply => return Err(CliError::new(format!("sandbox write failed: {reply:?}"))),
    }

    let read = format!("!toolcall file_read\n{path}");
    match client.send(&read).await? {
        Reply::Tool { ok: true, output } if output == marker => Ok(()),
        reply => Err(CliError::new(format!(
            "sandbox read verification failed: {reply:?}"
        ))),
    }
}

async fn receive_envelope(
    socket: &mut Socket,
    timeout: Duration,
    waiting_for: &str,
) -> CliResult<MessageEnvelope> {
    loop {
        let next = tokio::time::timeout(timeout, socket.next())
            .await
            .map_err(|_| CliError::new(format!("timed out waiting for {waiting_for}")))?;
        match next {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(text.as_ref()).map_err(|error| {
                    CliError::new(format!(
                        "invalid daemon response while waiting for {waiting_for}: {error}"
                    ))
                });
            }
            Some(Ok(Message::Ping(payload))) => {
                socket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|error| CliError::new(format!("pong failed: {error}")))?;
            }
            Some(Ok(Message::Close(frame))) => {
                return Err(CliError::new(format!(
                    "daemon closed connection while waiting for {waiting_for}: {frame:?}"
                )));
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => {
                return Err(CliError::new(format!(
                    "receive failed while waiting for {waiting_for}: {error}"
                )));
            }
            None => {
                return Err(CliError::new(format!(
                    "daemon disconnected while waiting for {waiting_for}"
                )));
            }
        }
    }
}

async fn send_envelope(socket: &mut Socket, envelope: &MessageEnvelope) -> CliResult<()> {
    let json = serde_json::to_string(envelope)
        .map_err(|error| CliError::new(format!("serialize auth response failed: {error}")))?;
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|error| CliError::new(format!("send auth response failed: {error}")))
}

fn validate_execution_mode(mode: &str) -> CliResult<()> {
    match mode {
        "guest_tools" | "guest_autonomous" => Ok(()),
        "host_only" => Err(CliError::new(
            "daemon is in host_only mode; CLI refuses to bypass Firecracker",
        )),
        other => Err(CliError::new(format!(
            "daemon reported unknown execution mode: {other}"
        ))),
    }
}

fn websocket_url(options: &ConnectionOptions) -> String {
    let separator = if options.url.contains('?') { '&' } else { '?' };
    format!(
        "{}{separator}user_id={}&session_id={}",
        options.url,
        encode_query(&options.user_id),
        encode_query(&options.session_id)
    )
}

fn encode_query(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn parse_args(args: Vec<String>) -> CliResult<Command> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(Command::Chat(ConnectionOptions::default()));
    };
    if matches!(command, "-h" | "--help" | "help") {
        return Ok(Command::Help);
    }

    let mut options = ConnectionOptions::default();
    let mut message = Vec::new();
    let mut file = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--url" | "--user" | "--session" | "--timeout-secs" | "--file" => {
                let flag = args[index].clone();
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| CliError::new(format!("missing value for {flag}")))?;
                match flag.as_str() {
                    "--url" => options.url = value.clone(),
                    "--user" => options.user_id = value.clone(),
                    "--session" => options.session_id = value.clone(),
                    "--timeout-secs" => {
                        options.timeout_secs = value.parse::<u64>().map_err(|error| {
                            CliError::new(format!("invalid --timeout-secs value: {error}"))
                        })?;
                    }
                    "--file" => file = Some(PathBuf::from(value)),
                    _ => unreachable!(),
                }
            }
            value if value.starts_with('-') => {
                return Err(CliError::new(format!("unknown option: {value}")));
            }
            value => message.push(value.to_string()),
        }
        index += 1;
    }

    match command {
        "chat" => {
            if file.is_some() {
                Err(CliError::new(
                    "chat accepts files interactively with /file <PATH>",
                ))
            } else if message.is_empty() {
                Ok(Command::Chat(options))
            } else {
                Err(CliError::new("chat does not accept a message argument"))
            }
        }
        "ask" => {
            if message.is_empty() && file.is_none() {
                Err(CliError::new("ask requires a message"))
            } else {
                let prompt = if message.is_empty() {
                    "Analyze this file and report the important findings.".to_string()
                } else {
                    message.join(" ")
                };
                Ok(Command::Ask(options, prompt, file))
            }
        }
        "doctor" => {
            if file.is_some() {
                Err(CliError::new("doctor does not accept --file"))
            } else if message.is_empty() {
                Ok(Command::Doctor(options))
            } else {
                Err(CliError::new("doctor does not accept a message argument"))
            }
        }
        other => Err(CliError::new(format!("unknown command: {other}"))),
    }
}

fn print_reply(reply: Reply) -> CliResult<()> {
    match reply {
        Reply::Text(text) => println!("{text}"),
        Reply::Tool { ok, output } => println!("tool(ok={ok}): {output}"),
        Reply::Artifact(artifact) => {
            let path = save_artifact(&artifact)?;
            println!(
                "artifact: {} ({}, {} bytes){}",
                path.display(),
                artifact.mime_type,
                artifact.data.len(),
                artifact_caption_suffix(&artifact.caption)
            );
        }
    }
    Ok(())
}

fn save_artifact(artifact: &Artifact) -> CliResult<PathBuf> {
    const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
    if artifact.data.len() > MAX_ARTIFACT_BYTES {
        return Err(CliError::new("artifact exceeds CLI size limit"));
    }
    let filename = Path::new(&artifact.filename)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| CliError::new("artifact filename is invalid"))?;
    let directory = PathBuf::from("artifacts");
    std::fs::create_dir_all(&directory)
        .map_err(|error| CliError::new(format!("create artifact directory failed: {error}")))?;
    let path = directory.join(format!("{}-{filename}", now_ms()));
    std::fs::write(&path, &artifact.data)
        .map_err(|error| CliError::new(format!("save artifact failed: {error}")))?;
    Ok(path)
}

fn artifact_caption_suffix(caption: &str) -> String {
    if caption.trim().is_empty() {
        String::new()
    } else {
        format!(" — {}", caption.trim())
    }
}

fn print_usage() {
    println!(
        "Ironclaw Firecracker CLI\n\n\
Usage:\n  \
ironclaw chat [OPTIONS]\n  \
ironclaw ask [OPTIONS] [--file <PATH>] [MESSAGE]\n  \
ironclaw doctor [OPTIONS]\n\n\
Options:\n  \
--url <WS_URL>          Daemon WebSocket URL (default: {DEFAULT_URL})\n  \
--user <ID>             Isolated user/VM identifier (default: cli)\n  \
--session <ID>          Conversation session identifier\n  \
--file <PATH>           Upload a file into the Firecracker workspace\n  \
--timeout-secs <N>      VM boot and response timeout (default: {DEFAULT_TIMEOUT_SECS})"
    );
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn io_error(error: std::io::Error) -> CliError {
    CliError::new(format!("terminal I/O failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_interactive_chat() {
        assert_eq!(
            parse_args(Vec::new()).expect("parse"),
            Command::Chat(ConnectionOptions::default())
        );
    }

    #[test]
    fn parses_ask_options_and_message() {
        let parsed = parse_args(vec![
            "ask".into(),
            "--user".into(),
            "alice smith".into(),
            "hello".into(),
            "there".into(),
        ])
        .expect("parse");
        let mut expected_options = ConnectionOptions::default();
        expected_options.user_id = "alice smith".into();
        assert_eq!(
            parsed,
            Command::Ask(expected_options, "hello there".into(), None)
        );
    }

    #[test]
    fn parses_file_upload_with_default_analysis_prompt() {
        let parsed =
            parse_args(vec!["ask".into(), "--file".into(), "draft.tex".into()]).expect("parse");
        assert_eq!(
            parsed,
            Command::Ask(
                ConnectionOptions::default(),
                "Analyze this file and report the important findings.".into(),
                Some(PathBuf::from("draft.tex")),
            )
        );
    }

    #[test]
    fn rejects_host_only_daemon() {
        let error = validate_execution_mode("host_only").expect_err("must reject");
        assert!(error.to_string().contains("refuses to bypass Firecracker"));
    }

    #[test]
    fn encodes_websocket_query_values() {
        let options = ConnectionOptions {
            user_id: "alice smith".into(),
            session_id: "cli/a".into(),
            ..ConnectionOptions::default()
        };
        let url = websocket_url(&options);
        assert!(url.contains("user_id=alice%20smith"));
        assert!(url.contains("session_id=cli%2Fa"));
    }
}
