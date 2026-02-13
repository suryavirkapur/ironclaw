use common::firecracker::{
    default_vsock_port, FirecrackerManager, FirecrackerManagerConfig, VmConfig, VmManager,
};
use common::proto::ironclaw::{message_envelope, AuthChallenge, MessageEnvelope, UserMessage};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[tokio::main]
async fn main() -> Result<(), String> {
    let firecracker_bin = env_or_default("FIRECRACKER_BIN", "firecracker");
    let kernel_path = path_env_or_default("KERNEL_PATH", "kernels/firecracker/vmlinux-6.1.155.bin");
    let rootfs_path = path_env_or_default("ROOTFS_PATH", "rootfs/build/guest-rootfs.ext4");
    let api_socket_dir = path_env_or_default("API_SOCKET_DIR", "/tmp/ironclaw-fc");
    let vsock_uds_dir = path_env_or_default("VSOCK_UDS_DIR", "/tmp/ironclaw/vsock");
    let vsock_port = std::env::var("VSOCK_PORT")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or_else(default_vsock_port);

    let manager = FirecrackerManager::new(FirecrackerManagerConfig {
        firecracker_bin: PathBuf::from(firecracker_bin),
        kernel_path,
        rootfs_path,
        api_socket_dir,
        vsock_uds_dir,
        vsock_port,
    });

    let user_id = format!("smoke-{}", std::process::id());
    let brain_path = PathBuf::from(format!("/tmp/ironclaw-smoke-{}", std::process::id()));

    let mut vm = manager
        .start_vm(VmConfig {
            user_id: user_id.clone(),
            brain_path,
        })
        .await
        .map_err(|e| e.to_string())?;

    let cap_token = format!("smoke-token-{}", now_ms()?);

    let challenge = MessageEnvelope {
        user_id: user_id.clone(),
        session_id: "roundtrip".to_string(),
        msg_id: 1,
        timestamp_ms: now_ms()?,
        cap_token: cap_token.clone(),
        payload: Some(message_envelope::Payload::AuthChallenge(AuthChallenge {
            cap_token: cap_token.clone(),
            allowed_tools: vec![],
            execution_mode: "host_only".to_string(),
        })),
    };

    vm.transport
        .send(challenge)
        .await
        .map_err(|e| format!("auth challenge send failed: {e}"))?;

    let auth_msg = tokio::time::timeout(std::time::Duration::from_secs(5), vm.transport.recv())
        .await
        .map_err(|_| "auth ack timeout".to_string())
        .and_then(|res| res.map_err(|e| format!("auth ack recv failed: {e}")))?
        .ok_or_else(|| "auth ack stream closed".to_string())?;

    match auth_msg.payload {
        Some(message_envelope::Payload::AuthAck(ack)) if ack.cap_token == cap_token => {}
        other => {
            let _ = manager.stop_vm(&user_id).await;
            return Err(format!("unexpected auth ack payload: {other:?}"));
        }
    }

    let user_msg = MessageEnvelope {
        user_id: user_id.clone(),
        session_id: "roundtrip".to_string(),
        msg_id: 2,
        timestamp_ms: now_ms()?,
        cap_token,
        payload: Some(message_envelope::Payload::UserMessage(UserMessage {
            text: "hello from smoke".to_string(),
        })),
    };

    vm.transport
        .send(user_msg)
        .await
        .map_err(|e| format!("user message send failed: {e}"))?;

    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), vm.transport.recv())
        .await
        .map_err(|_| "guest reply timeout".to_string())
        .and_then(|res| res.map_err(|e| format!("guest reply recv failed: {e}")))?
        .ok_or_else(|| "guest reply stream closed".to_string())?;

    let reply_text = match reply.payload {
        Some(message_envelope::Payload::StreamDelta(delta)) => delta.delta,
        other => {
            let _ = manager.stop_vm(&user_id).await;
            return Err(format!("unexpected guest reply payload: {other:?}"));
        }
    };

    manager
        .stop_vm(&user_id)
        .await
        .map_err(|e| format!("vm stop failed: {e}"))?;

    if reply_text != "stub: hello from smoke" {
        return Err(format!("unexpected guest reply text: {reply_text}"));
    }

    println!("ok firecracker vsock roundtrip: {reply_text}");
    Ok(())
}

fn env_or_default(key: &str, default_value: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default_value.to_string())
}

fn path_env_or_default(key: &str, default_value: &str) -> PathBuf {
    PathBuf::from(env_or_default(key, default_value))
}

fn now_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .map_err(|err| format!("time error: {err}"))
}
