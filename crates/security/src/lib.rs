pub mod auth;
pub mod control_plane;
pub mod pairing;
pub mod rate_limiter;

use rusqlite::Connection;

pub use auth::{channel_allowed, validate_webhook_secret};
pub use control_plane::{
    append_audit_event, AuditEvent, ControlPlaneAuthorizer, ControlPlanePrincipal, ControlPlaneRole,
};
pub use pairing::{PairingManager, PairingStatus};
pub use rate_limiter::{RateLimitConfig, RateLimitDecision, RateLimiter};

pub fn initialize_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "create table if not exists pairing (
            id integer primary key autoincrement,
            node_id text not null,
            otp text,
            created_at integer not null,
            expires_at integer not null,
            status text not null,
            bearer_token_hash text,
            paired_at integer
        );
        create index if not exists idx_pairing_node_id_created
            on pairing(node_id, created_at desc);

        create table if not exists rate_limits (
            user_id text not null,
            channel text not null,
            window_start integer not null,
            request_count integer not null,
            primary key (user_id, channel, window_start)
        );

        create table if not exists rate_limit_costs (
            user_id text not null,
            channel text not null,
            day_start integer not null,
            total_cost integer not null,
            primary key (user_id, channel, day_start)
        );

        create table if not exists control_plane_audit (
            id integer primary key autoincrement,
            request_id text not null,
            occurred_at_ms integer not null,
            principal_id text,
            organization_id text,
            role text,
            method text not null,
            path text not null,
            action text not null,
            resource text not null,
            decision text not null,
            status_code integer not null
        );
        create index if not exists idx_control_plane_audit_time
            on control_plane_audit(occurred_at_ms desc);
        ",
    )
    .map_err(|err| format!("security schema failed: {err}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::initialize_schema;

    #[test]
    fn schema_init_creates_tables() {
        let conn = match rusqlite::Connection::open_in_memory() {
            Ok(value) => value,
            Err(err) => panic!("db open failed: {err}"),
        };
        if let Err(err) = initialize_schema(&conn) {
            panic!("schema init failed: {err}");
        }

        let count: i64 = match conn.query_row(
            "select count(1) from sqlite_master where type='table' and name in (
                'pairing', 'rate_limits', 'rate_limit_costs', 'control_plane_audit'
            )",
            [],
            |row| row.get(0),
        ) {
            Ok(value) => value,
            Err(err) => panic!("schema query failed: {err}"),
        };
        assert_eq!(count, 4);
    }
}
