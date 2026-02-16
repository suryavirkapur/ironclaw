use rand::Rng;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairingStatus {
    Unpaired,
    Pairing,
    Paired,
}

impl PairingStatus {
    fn as_db(self) -> &'static str {
        match self {
            Self::Unpaired => "unpaired",
            Self::Pairing => "pairing",
            Self::Paired => "paired",
        }
    }

    fn from_db(raw: &str) -> Option<Self> {
        match raw {
            "unpaired" => Some(Self::Unpaired),
            "pairing" => Some(Self::Pairing),
            "paired" => Some(Self::Paired),
            _ => None,
        }
    }
}

pub struct PairingManager {
    otp_expiry_seconds: u64,
}

impl PairingManager {
    pub fn new(otp_expiry_seconds: u64) -> Self {
        Self { otp_expiry_seconds }
    }

    pub fn current_status(
        &self,
        conn: &Connection,
        node_id: &str,
        now_epoch_seconds: i64,
    ) -> Result<PairingStatus, String> {
        let row = conn
            .query_row(
                "select status, expires_at
                 from pairing
                 where node_id = ?1
                 order by created_at desc
                 limit 1",
                params![node_id],
                |row| {
                    let status: String = row.get(0)?;
                    let expires_at: i64 = row.get(1)?;
                    Ok((status, expires_at))
                },
            )
            .optional()
            .map_err(|err| format!("pairing status query failed: {err}"))?;

        let Some((status_raw, expires_at)) = row else {
            return Ok(PairingStatus::Unpaired);
        };
        let status = PairingStatus::from_db(&status_raw).unwrap_or(PairingStatus::Unpaired);
        if status == PairingStatus::Pairing && expires_at < now_epoch_seconds {
            return Ok(PairingStatus::Unpaired);
        }
        Ok(status)
    }

    pub fn begin_pairing(
        &self,
        conn: &Connection,
        node_id: &str,
        now_epoch_seconds: i64,
    ) -> Result<String, String> {
        let otp = generate_otp();
        let expires_at = now_epoch_seconds.saturating_add(self.otp_expiry_seconds as i64);
        conn.execute(
            "insert into pairing (
                node_id, otp, created_at, expires_at, status, bearer_token_hash, paired_at
            ) values (?1, ?2, ?3, ?4, ?5, null, null)",
            params![
                node_id,
                otp,
                now_epoch_seconds,
                expires_at,
                PairingStatus::Pairing.as_db()
            ],
        )
        .map_err(|err| format!("pairing insert failed: {err}"))?;
        Ok(otp)
    }

    pub fn verify_otp_and_issue_bearer(
        &self,
        conn: &Connection,
        node_id: &str,
        otp_attempt: &str,
        now_epoch_seconds: i64,
    ) -> Result<Option<String>, String> {
        let row = conn
            .query_row(
                "select id, otp, expires_at
                 from pairing
                 where node_id = ?1 and status = ?2
                 order by created_at desc
                 limit 1",
                params![node_id, PairingStatus::Pairing.as_db()],
                |row| {
                    let id: i64 = row.get(0)?;
                    let otp: Option<String> = row.get(1)?;
                    let expires_at: i64 = row.get(2)?;
                    Ok((id, otp, expires_at))
                },
            )
            .optional()
            .map_err(|err| format!("pairing verify lookup failed: {err}"))?;

        let Some((id, otp, expires_at)) = row else {
            return Ok(None);
        };

        if expires_at < now_epoch_seconds {
            conn.execute(
                "update pairing
                 set status = ?1
                 where id = ?2",
                params![PairingStatus::Unpaired.as_db(), id],
            )
            .map_err(|err| format!("pairing expiration update failed: {err}"))?;
            return Ok(None);
        }

        let expected = otp.unwrap_or_default();
        if !constant_time_eq(expected.as_bytes(), otp_attempt.as_bytes()) {
            return Ok(None);
        }

        let token = generate_bearer_token();
        let token_hash = sha256_hex(token.as_bytes());
        conn.execute(
            "update pairing
             set status = ?1, bearer_token_hash = ?2, paired_at = ?3, otp = null
             where id = ?4",
            params![
                PairingStatus::Paired.as_db(),
                token_hash,
                now_epoch_seconds,
                id
            ],
        )
        .map_err(|err| format!("pairing update failed: {err}"))?;

        Ok(Some(token))
    }

    pub fn validate_bearer(
        &self,
        conn: &Connection,
        node_id: &str,
        bearer_token: &str,
    ) -> Result<bool, String> {
        let row = conn
            .query_row(
                "select bearer_token_hash
                 from pairing
                 where node_id = ?1 and status = ?2
                 order by created_at desc
                 limit 1",
                params![node_id, PairingStatus::Paired.as_db()],
                |row| {
                    let value: Option<String> = row.get(0)?;
                    Ok(value)
                },
            )
            .optional()
            .map_err(|err| format!("pairing token lookup failed: {err}"))?;

        let Some(Some(expected_hash)) = row else {
            return Ok(false);
        };
        let got_hash = sha256_hex(bearer_token.as_bytes());
        Ok(constant_time_eq(
            expected_hash.as_bytes(),
            got_hash.as_bytes(),
        ))
    }
}

pub fn generate_otp() -> String {
    let value: u32 = rand::rng().random_range(0..1_000_000);
    format!("{value:06}")
}

fn generate_bearer_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut diff: u8 = 0;
    for (left, right) in a.iter().zip(b.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::{constant_time_eq, generate_otp, PairingManager, PairingStatus};
    use crate::initialize_schema;

    #[test]
    fn otp_generation_is_six_digits() {
        for _ in 0..500 {
            let otp = generate_otp();
            assert_eq!(otp.len(), 6);
            assert!(otp.chars().all(|ch| ch.is_ascii_digit()));
        }
    }

    #[test]
    fn otp_pairing_happy_path_and_expiration() {
        let conn = match rusqlite::Connection::open_in_memory() {
            Ok(value) => value,
            Err(err) => panic!("db open failed: {err}"),
        };
        if let Err(err) = initialize_schema(&conn) {
            panic!("schema init failed: {err}");
        }
        let manager = PairingManager::new(300);
        let now = 1_700_000_000i64;

        let otp = match manager.begin_pairing(&conn, "node-a", now) {
            Ok(value) => value,
            Err(err) => panic!("begin pairing failed: {err}"),
        };
        assert_eq!(
            manager
                .current_status(&conn, "node-a", now)
                .unwrap_or(PairingStatus::Unpaired),
            PairingStatus::Pairing
        );

        let token = match manager.verify_otp_and_issue_bearer(&conn, "node-a", &otp, now + 1) {
            Ok(Some(value)) => value,
            Ok(None) => panic!("expected token, got none"),
            Err(err) => panic!("verify failed: {err}"),
        };
        assert!(!token.is_empty());
        assert!(manager
            .validate_bearer(&conn, "node-a", &token)
            .unwrap_or(false));
        assert_eq!(
            manager
                .current_status(&conn, "node-a", now + 1)
                .unwrap_or(PairingStatus::Unpaired),
            PairingStatus::Paired
        );

        let otp2 = match manager.begin_pairing(&conn, "node-b", now) {
            Ok(value) => value,
            Err(err) => panic!("begin pairing failed: {err}"),
        };
        let expired = manager.verify_otp_and_issue_bearer(&conn, "node-b", &otp2, now + 400);
        match expired {
            Ok(None) => {}
            Ok(Some(_)) => panic!("expected no token for expired otp"),
            Err(err) => panic!("verify failed: {err}"),
        }
        assert_eq!(
            manager
                .current_status(&conn, "node-b", now + 400)
                .unwrap_or(PairingStatus::Pairing),
            PairingStatus::Unpaired
        );
    }

    #[test]
    fn constant_time_compare_handles_equal_and_not_equal() {
        assert!(constant_time_eq(b"123456", b"123456"));
        assert!(!constant_time_eq(b"123456", b"123457"));
        assert!(!constant_time_eq(b"123456", b"12345"));
    }
}
