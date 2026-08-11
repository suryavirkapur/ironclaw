# Channel Abstraction — Minimal-Code Integrations

## Problem

Every messaging integration (Telegram, WhatsApp, future: Discord, Signal, Matrix…) currently
duplicates the same core logic:

| Duplicated concern              | Telegram location       | WhatsApp location       |
|---------------------------------|-------------------------|-------------------------|
| Session struct                  | `TelegramSession`       | `WhatsAppSession`       |
| Transcript type + persistence   | `TelegramTranscript*`   | `WhatsAppTranscript*`   |
| Message loop                    | `run_telegram_loop`     | `run_whatsapp_loop`     |
| Text handler                    | `handle_telegram_text`  | `handle_whatsapp_text`  |
| Memory summarization            | `summarize_telegram_…`  | `summarize_whatsapp_…`  |
| Chunk splitting                 | `split_telegram_chunks` | `split_whatsapp_chunks` |

Adding a new channel today means copy-pasting ~300 lines and renaming "telegram" → "discord".

## Goal

A new integration should only need to implement **message I/O** (receive + send). Everything
else — sessions, transcripts, memory, LLM turns, tool execution — should be shared.

---

## Proposed Architecture

```
crates/ironclawd/src/
├── channel/
│   ├── mod.rs          # Channel trait + run_channel_loop + handle_text
│   ├── session.rs      # Unified Session (replaces Telegram/WhatsAppSession)
│   ├── transcript.rs   # Unified Transcript + persistence
│   ├── telegram.rs     # impl Channel for Telegram (~80 lines)
│   └── whatsapp.rs     # impl Channel for WhatsApp  (~80 lines)
└── main.rs             # spawns channels generically
```

### 1. The `Channel` trait

Each integration only implements this:

```rust
/// A messaging channel (Telegram, WhatsApp, Discord, etc.)
#[async_trait]
pub trait Channel: Send + Sync + 'static {
    /// Unique prefix for session IDs, e.g. "telegram", "whatsapp"
    fn name(&self) -> &'static str;

    /// Maximum characters per outgoing message.
    fn max_message_len(&self) -> usize { 4096 }

    /// Receive the next incoming message. Blocks until one arrives or shutdown.
    /// Returns None on shutdown / channel close.
    async fn recv(&mut self) -> Option<IncomingMessage>;

    /// Send a text message to a conversation.
    async fn send(&self, conversation_id: &str, text: &str) -> Result<(), IronclawError>;

    /// Authorization check — is this sender allowed?
    fn is_allowed(&self, sender_id: &str) -> bool;
}
```

```rust
pub struct IncomingMessage {
    pub sender_id: String,       // opaque ID (chat_id, JID, etc.)
    pub text: String,
    pub timestamp_ms: u64,
}
```

### 2. Unified `Session`

Replaces both `TelegramSession` and `WhatsAppSession`:

```rust
pub struct Session {
    pub conversation_id: String,   // the sender's opaque ID  
    pub user_id: String,           // always "owner" for now
    pub session_id: String,        // "{channel_name}-{conversation_id}"
    pub transcript: Transcript,
    pub transcript_path: PathBuf,
    pub runtime_banner_sent: bool,
    // Telegram-specific (guest transport) — behind Option
    pub transport: Option<Box<dyn Transport>>,
    pub transport_failures: u32,
    pub msg_id: u64,
}
```

Transport-related fields are `Option`/zero-default for channels that don't use
guest VMs (WhatsApp, Discord). This keeps one struct without penalty.

### 3. Unified `Transcript`

Identical to what both channels already use — just remove the prefix:

```rust
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Transcript {
    pub messages: Vec<TranscriptMessage>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TranscriptMessage {
    pub role: String,       // "user" | "assistant"
    pub text: String,
    pub timestamp_ms: u64,
}
```

### 4. Generic `run_channel_loop`

One loop to rule them all:

```rust
async fn run_channel_loop(
    state: AppState,
    mut channel: impl Channel,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), IronclawError> {
    let mut sessions = HashMap::<String, Session>::new();

    loop {
        tokio::select! {
            maybe_msg = channel.recv() => {
                let Some(msg) = maybe_msg else { break };
                if !channel.is_allowed(&msg.sender_id) {
                    tracing::warn!("{}: denied {}", channel.name(), msg.sender_id);
                    continue;
                }
                let session = sessions
                    .entry(msg.sender_id.clone())
                    .or_insert_with(|| Session::load(
                        channel.name(), &msg.sender_id, &state.host_config.storage.users_root
                    ).unwrap());

                if let Err(err) = handle_channel_text(&state, &channel, session, &msg.text).await {
                    tracing::error!("{}: {err}", channel.name());
                    let _ = session.append_assistant("request failed", now_ms().unwrap_or(0));
                    let _ = channel.send(&session.conversation_id, "request failed").await;
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() { break; }
            }
        }
    }
    Ok(())
}
```

### 5. Generic `handle_channel_text`

Replaces both `handle_telegram_text` and `handle_whatsapp_text`:

```rust
async fn handle_channel_text(
    state: &AppState,
    channel: &impl Channel,
    session: &mut Session,
    text: &str,
) -> Result<(), IronclawError> {
    session.append_user(text, now_ms()?)?;
    let _ = summarize_session_memory(state, session);    // shared

    if let Some(cmd) = parse_memory_command(text) {
        let output = execute_memory_command(state, &session.user_id, &session.session_id, cmd)?;
        send_chunked(channel, &session.conversation_id, &output).await?;
        session.append_assistant(&output, now_ms()?)?;
        return Ok(());
    }

    let history = session.prompt_history();
    let tools = allowed_tools_for_runtime(state.local_guest, state.guest_allow_bash);
    let output = run_host_turn(state, &session.user_id, &tools, text, Some(&history)).await?;

    send_chunked(channel, &session.conversation_id, &output).await?;
    session.append_assistant(&output, now_ms()?)?;
    Ok(())
}
```

Where `send_chunked` splits by `channel.max_message_len()`:

```rust
async fn send_chunked(
    channel: &impl Channel,
    conversation_id: &str,
    text: &str,
) -> Result<(), IronclawError> {
    for chunk in split_chunks(text, channel.max_message_len()) {
        channel.send(conversation_id, &chunk).await?;
    }
    Ok(())
}
```

---

## What a new integration looks like

Adding **Discord** becomes ~80 lines:

```rust
// channel/discord.rs
pub struct DiscordChannel { /* bot, rx, config */ }

#[async_trait]
impl Channel for DiscordChannel {
    fn name(&self) -> &'static str { "discord" }

    async fn recv(&mut self) -> Option<IncomingMessage> {
        self.rx.recv().await  // from serenity event handler
    }

    async fn send(&self, channel_id: &str, text: &str) -> Result<(), IronclawError> {
        self.http.send_message(channel_id.parse()?, text).await?;
        Ok(())
    }

    fn is_allowed(&self, sender_id: &str) -> bool {
        self.config.allowlist.contains(sender_id)
    }
}
```

Plus a config struct and `start_discord_bot()` function. **Zero duplication of session,
transcript, memory, LLM, or loop logic.**

---

## Handling Telegram's extra complexity

Telegram currently has features WhatsApp doesn't:

| Feature                   | How to handle                                      |
|---------------------------|-----------------------------------------------------|
| Guest VM transport        | `Session.transport` is `Option` — only Telegram sets it |
| Retry with backoff        | Wrap `handle_channel_text` in a retry layer in Telegram's loop override, or add an optional `retry_policy()` method to the trait |
| Runtime banner            | Add `fn runtime_banner(&self) -> Option<&str>` to trait (default `None`) |
| HTTP polling (getUpdates) | Implemented inside `TelegramChannel::recv()` |
| Offset persistence        | Internal to `TelegramChannel` — not the loop's concern |

The key insight: **transport/retry is orthogonal to the channel abstraction**. It can
live as a thin wrapper around `handle_channel_text` that only Telegram opts into, or as
a `Channel` trait method:

```rust
trait Channel {
    // ...existing methods...

    /// Optional: number of retries on transport failure (default 0 = no retry).
    fn retry_attempts(&self) -> usize { 0 }

    /// Optional: called before the first message to send a one-time banner.
    fn runtime_banner(&self) -> Option<&str> { None }
}
```

---

## Migration plan

### Phase 1 — Extract shared types (non-breaking)
1. Create `channel/mod.rs` with the `Channel` trait, `IncomingMessage`, `Session`, `Transcript`
2. Keep existing Telegram/WhatsApp code working alongside

### Phase 2 — Implement `Channel` for WhatsApp  
- Wrap existing `start_whatsapp_bot` + mpsc into `WhatsAppChannel::recv()`
- Delete `WhatsAppSession`, `WhatsAppTranscript`, `run_whatsapp_loop`, `handle_whatsapp_text`

### Phase 3 — Implement `Channel` for Telegram  
- Wrap `TelegramClient.get_updates` into `TelegramChannel::recv()`  
- Move transport/retry logic into `TelegramChannel` or a wrapper
- Delete `TelegramSession`, `TelegramTranscript`, `run_telegram_loop`, `handle_telegram_text`

### Phase 4 — Clean up `main.rs`
- Replace per-channel `if let Some(config)` blocks with a `Vec<Box<dyn Channel>>`
- Single spawn per channel using `run_channel_loop`

---

## Line count estimate

| Component | Before (TG + WA) | After (shared + 2 impls) |
|-----------|------------------:|--------------------------:|
| Session   |        ~60 + ~60  |                      ~60  |
| Transcript|        ~50 + ~50  |                      ~50  |
| Loop      |        ~70 + ~50  |                      ~40  |
| Handler   |       ~130 + ~40  |                      ~50  |
| TG impl   |              —    |                      ~80  |
| WA impl   |              —    |                      ~80  |
| **Total** |         **~510**  |                   **~360**|

Net reduction is modest (~30%), but the real win is that **each new channel is ~80 lines
instead of ~250+**, and bug fixes to session/transcript/memory logic apply everywhere.
