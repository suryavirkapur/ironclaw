use common::codec::ProtoCodec;
use common::firecracker::{StubVmManager, VmConfig, VmManager};
use common::proto::ironclaw::{message_envelope, MessageEnvelope, UserMessage};
use common::stream_transport::StreamTransport;
use common::transport::{LocalTransport, Transport};

fn sample_message(text: &str, msg_id: u64) -> MessageEnvelope {
    MessageEnvelope {
        user_id: "owner".to_string(),
        session_id: "session".to_string(),
        msg_id,
        timestamp_ms: 123,
        cap_token: "cap".to_string(),
        payload: Some(message_envelope::Payload::UserMessage(UserMessage {
            text: text.to_string(),
        })),
    }
}

#[test]
fn protobuf_codec_roundtrip_preserves_fields() {
    let original = sample_message("hello", 7);
    let encoded = match ProtoCodec::encode(&original) {
        Ok(value) => value,
        Err(err) => panic!("encode failed: {err}"),
    };

    let mut codec = ProtoCodec::new();
    codec.push_bytes(&encoded);
    let decoded = match codec.next_frame() {
        Ok(Some(value)) => value,
        Ok(None) => panic!("missing decoded frame"),
        Err(err) => panic!("decode failed: {err}"),
    };

    assert_eq!(decoded.user_id, original.user_id);
    assert_eq!(decoded.session_id, original.session_id);
    assert_eq!(decoded.msg_id, original.msg_id);
    assert_eq!(decoded.cap_token, original.cap_token);
    match decoded.payload {
        Some(message_envelope::Payload::UserMessage(message)) => {
            assert_eq!(message.text, "hello");
        }
        _ => panic!("unexpected payload"),
    }
}

#[test]
fn protobuf_codec_rejects_invalid_payload() {
    let mut codec = ProtoCodec::new();
    let mut frame = vec![0_u8, 0, 0, 4];
    frame.extend_from_slice(&[1_u8, 2, 3, 4]);
    codec.push_bytes(&frame);

    let result = codec.next_frame();
    assert!(result.is_err());
}

#[tokio::test]
async fn local_transport_sends_and_receives_frames() {
    let (mut a, mut b) = LocalTransport::pair(8);
    let outbound = sample_message("ping", 1);

    if let Err(err) = a.send(outbound).await {
        panic!("send failed: {err}");
    }

    let inbound = match b.recv().await {
        Ok(Some(value)) => value,
        Ok(None) => panic!("transport closed"),
        Err(err) => panic!("recv failed: {err}"),
    };

    match inbound.payload {
        Some(message_envelope::Payload::UserMessage(message)) => {
            assert_eq!(message.text, "ping");
        }
        _ => panic!("unexpected payload"),
    }
}

#[tokio::test]
async fn stream_transport_roundtrip_over_duplex() {
    let (left, right) = tokio::io::duplex(4096);
    let mut sender = StreamTransport::new(left);
    let mut receiver = StreamTransport::new(right);

    let outbound = sample_message("stream-test", 22);
    if let Err(err) = sender.send(outbound).await {
        panic!("stream send failed: {err}");
    }

    let inbound = match receiver.recv().await {
        Ok(Some(value)) => value,
        Ok(None) => panic!("stream closed"),
        Err(err) => panic!("stream recv failed: {err}"),
    };

    assert_eq!(inbound.msg_id, 22);
    match inbound.payload {
        Some(message_envelope::Payload::UserMessage(message)) => {
            assert_eq!(message.text, "stream-test");
        }
        _ => panic!("unexpected payload"),
    }
}

#[tokio::test]
async fn stub_vm_lifecycle_tracks_running_state() {
    let manager = StubVmManager::new(8);
    let config = VmConfig {
        user_id: "owner".to_string(),
        brain_path: std::path::PathBuf::from("/tmp/brain"),
    };

    let before = match manager.is_vm_running("owner").await {
        Ok(value) => value,
        Err(err) => panic!("is_vm_running failed: {err}"),
    };
    assert!(!before);

    let _instance = match manager.start_vm(config).await {
        Ok(value) => value,
        Err(err) => panic!("start_vm failed: {err}"),
    };

    let after_start = match manager.is_vm_running("owner").await {
        Ok(value) => value,
        Err(err) => panic!("is_vm_running failed: {err}"),
    };
    assert!(after_start);

    if let Err(err) = manager.stop_vm("owner").await {
        panic!("stop_vm failed: {err}");
    }

    let after_stop = match manager.is_vm_running("owner").await {
        Ok(value) => value,
        Err(err) => panic!("is_vm_running failed: {err}"),
    };
    assert!(!after_stop);
}

#[tokio::test]
async fn stub_vm_with_guest_transport_roundtrip() {
    let manager = StubVmManager::new(8);
    let config = VmConfig {
        user_id: "owner".to_string(),
        brain_path: std::path::PathBuf::from("/tmp/brain"),
    };

    let (instance, mut guest_transport) = match manager.start_vm_with_guest(config) {
        Ok(value) => value,
        Err(err) => panic!("start_vm_with_guest failed: {err}"),
    };
    let mut host_transport = instance.transport;

    if let Err(err) = host_transport
        .send(sample_message("vm-roundtrip", 33))
        .await
    {
        panic!("host send failed: {err}");
    }

    let inbound = match guest_transport.recv().await {
        Ok(Some(value)) => value,
        Ok(None) => panic!("guest closed"),
        Err(err) => panic!("guest recv failed: {err}"),
    };

    assert_eq!(inbound.msg_id, 33);
}
