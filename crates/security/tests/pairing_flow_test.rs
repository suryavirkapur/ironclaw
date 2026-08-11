use security::{initialize_schema, PairingManager, PairingStatus};

#[test]
fn full_pairing_flow_works() {
    let conn = match rusqlite::Connection::open_in_memory() {
        Ok(value) => value,
        Err(err) => panic!("db open failed: {err}"),
    };
    if let Err(err) = initialize_schema(&conn) {
        panic!("schema init failed: {err}");
    }

    let manager = PairingManager::new(300);
    let now = 1_700_000_000i64;

    let status_before = manager
        .current_status(&conn, "gateway-1", now)
        .unwrap_or(PairingStatus::Pairing);
    assert_eq!(status_before, PairingStatus::Unpaired);

    let otp = match manager.begin_pairing(&conn, "gateway-1", now) {
        Ok(value) => value,
        Err(err) => panic!("begin pairing failed: {err}"),
    };
    assert_eq!(
        manager
            .current_status(&conn, "gateway-1", now + 1)
            .unwrap_or(PairingStatus::Unpaired),
        PairingStatus::Pairing
    );

    let wrong = manager
        .verify_otp_and_issue_bearer(&conn, "gateway-1", "000000", now + 2)
        .unwrap_or_else(|_| panic!("verify failed"));
    assert!(wrong.is_none());

    let token = manager
        .verify_otp_and_issue_bearer(&conn, "gateway-1", &otp, now + 3)
        .unwrap_or_else(|_| panic!("verify failed"));
    let token = match token {
        Some(value) => value,
        None => panic!("missing token"),
    };

    assert_eq!(
        manager
            .current_status(&conn, "gateway-1", now + 3)
            .unwrap_or(PairingStatus::Unpaired),
        PairingStatus::Paired
    );

    let valid = manager
        .validate_bearer(&conn, "gateway-1", &token)
        .unwrap_or(false);
    assert!(valid);

    let invalid = manager
        .validate_bearer(&conn, "gateway-1", "not-the-token")
        .unwrap_or(true);
    assert!(!invalid);
}
