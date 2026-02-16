use super::VsockTransport;

#[tokio::test]
async fn vsock_accept_either_errors_or_waits_for_connection() {
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        VsockTransport::accept(5001),
    )
    .await;

    assert!(matches!(result, Err(_) | Ok(Err(_))));
}
