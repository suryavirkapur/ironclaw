use crate::model::now_ms;
use common::proto::ironclaw::{message_envelope, AuthAck, MessageEnvelope};
use serde_json::Value;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tungstenite::{connect, Message};

pub enum Incoming {
    Ready,
    AgentText(String),
    System(String),
    Closed(String),
}

pub enum Outgoing {
    Prompt(String),
    Shutdown,
}

pub struct ChatSession {
    pub incoming: Receiver<Incoming>,
    outgoing: Sender<Outgoing>,
    worker: Option<JoinHandle<()>>,
}

impl ChatSession {
    pub fn connect(url: String, agent_id: String) -> Result<Self, String> {
        let (incoming_tx, incoming_rx) = mpsc::channel();
        let (outgoing_tx, outgoing_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("ironclaw-chat".into())
            .spawn(move || run_session(url, agent_id, incoming_tx, outgoing_rx))
            .map_err(|err| err.to_string())?;
        Ok(Self {
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            worker: Some(worker),
        })
    }

    pub fn send_prompt(&self, prompt: String) -> Result<(), String> {
        self.outgoing
            .send(Outgoing::Prompt(prompt))
            .map_err(|_| "chat session closed".to_string())
    }

    pub fn drain(&self) -> Vec<Incoming> {
        let mut events = Vec::new();
        loop {
            match self.incoming.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    events.push(Incoming::Closed("chat connection dropped".into()));
                    break;
                }
            }
        }
        events
    }
}

impl Drop for ChatSession {
    fn drop(&mut self) {
        let _ = self.outgoing.send(Outgoing::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_session(
    url: String,
    agent_id: String,
    incoming: Sender<Incoming>,
    outgoing: Receiver<Outgoing>,
) {
    let (mut socket, _) = match connect(&url) {
        Ok(value) => value,
        Err(err) => {
            let _ = incoming.send(Incoming::Closed(format!("could not connect: {err}")));
            return;
        }
    };
    if let Err(err) = apply_read_timeout(&mut socket) {
        let _ = incoming.send(Incoming::Closed(format!("socket setup failed: {err}")));
        return;
    }

    let session_id = format!("workspace-{agent_id}");
    let mut pending = String::new();
    let mut ready = false;

    loop {
        match outgoing.try_recv() {
            Ok(Outgoing::Shutdown) | Err(TryRecvError::Disconnected) => break,
            Ok(Outgoing::Prompt(prompt)) if ready => {
                if let Err(err) = socket.send(Message::Text(prompt.into())) {
                    let _ = incoming.send(Incoming::Closed(format!("send failed: {err}")));
                    break;
                }
            }
            Ok(Outgoing::Prompt(_)) => {
                let _ = incoming.send(Incoming::System(
                    "Still connecting to this agent's MicroVM.".into(),
                ));
            }
            Err(TryRecvError::Empty) => {}
        }

        match socket.read() {
            Ok(Message::Text(text)) => {
                if !ready {
                    match serde_json::from_str::<MessageEnvelope>(&text) {
                        Ok(envelope) => match envelope.payload {
                            Some(message_envelope::Payload::AuthChallenge(auth)) => {
                                let ack = MessageEnvelope {
                                    user_id: agent_id.clone(),
                                    session_id: session_id.clone(),
                                    msg_id: envelope.msg_id,
                                    timestamp_ms: now_ms(),
                                    cap_token: auth.cap_token.clone(),
                                    payload: Some(message_envelope::Payload::AuthAck(AuthAck {
                                        cap_token: auth.cap_token,
                                    })),
                                };
                                match serde_json::to_string(&ack) {
                                    Ok(json) => {
                                        if socket.send(Message::Text(json.into())).is_err() {
                                            let _ = incoming
                                                .send(Incoming::Closed("auth ack failed".into()));
                                            break;
                                        }
                                        ready = true;
                                        let _ = incoming.send(Incoming::Ready);
                                    }
                                    Err(err) => {
                                        let _ = incoming
                                            .send(Incoming::Closed(format!("auth encode: {err}")));
                                        break;
                                    }
                                }
                            }
                            _ => {
                                let _ = incoming.send(Incoming::System(text));
                            }
                        },
                        Err(_) => {
                            let _ = incoming.send(Incoming::System(text));
                        }
                    }
                    continue;
                }

                if let Ok(envelope) = serde_json::from_str::<MessageEnvelope>(&text) {
                    match envelope.payload {
                        Some(message_envelope::Payload::StreamDelta(delta)) => {
                            pending.push_str(&delta.delta);
                            if delta.done {
                                let _ = incoming
                                    .send(Incoming::AgentText(std::mem::take(&mut pending)));
                            }
                        }
                        Some(message_envelope::Payload::ToolCallResponse(result)) => {
                            let text = if result.output.is_empty() {
                                if result.ok {
                                    "Tool completed.".into()
                                } else {
                                    "Tool failed.".into()
                                }
                            } else {
                                result.output
                            };
                            let _ = incoming.send(Incoming::AgentText(text));
                        }
                        Some(message_envelope::Payload::Artifact(artifact)) => {
                            let caption = if artifact.caption.is_empty() {
                                format!("Created {}", artifact.filename)
                            } else {
                                artifact.caption
                            };
                            let _ = incoming.send(Incoming::AgentText(caption));
                        }
                        _ => {
                            if let Some(fallback) = json_text_fallback(&text) {
                                let _ = incoming.send(Incoming::System(fallback));
                            }
                        }
                    }
                } else {
                    let _ = incoming.send(Incoming::System(text));
                }
            }
            Ok(Message::Close(_)) => {
                let _ = incoming.send(Incoming::Closed("socket closed".into()));
                break;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(err))
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(err) => {
                let _ = incoming.send(Incoming::Closed(format!("socket error: {err}")));
                break;
            }
        }
    }
}

fn apply_read_timeout(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
) -> Result<(), String> {
    match socket.get_mut() {
        tungstenite::stream::MaybeTlsStream::Plain(stream) => {
            stream.set_read_timeout(Some(Duration::from_millis(80)))
        }
        tungstenite::stream::MaybeTlsStream::Rustls(stream) => stream
            .get_mut()
            .set_read_timeout(Some(Duration::from_millis(80))),
        other => {
            return Err(format!("unsupported websocket stream: {other:?}"));
        }
    }
    .map_err(|err| err.to_string())
}

fn json_text_fallback(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    value
        .get("error")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}
