use rusqlite::{params, Connection};

#[derive(Clone, Debug)]
pub struct RateLimitConfig {
    pub requests_per_minute: u32,
    pub requests_per_hour: u32,
    pub cost_per_day_cap: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_minute: 60,
            requests_per_hour: 1000,
            cost_per_day_cap: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RateLimitDecision {
    pub allowed: bool,
    pub reason: Option<String>,
    pub retry_after_seconds: u64,
}

pub struct RateLimiter {
    config: RateLimitConfig,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self { config }
    }

    pub fn check_and_record(
        &self,
        conn: &Connection,
        user_id: &str,
        channel: &str,
        now_epoch_seconds: i64,
        request_cost: u64,
    ) -> Result<RateLimitDecision, String> {
        let minute_start = now_epoch_seconds.saturating_sub(59);
        let hour_start = now_epoch_seconds.saturating_sub(3599);
        let day_start = now_epoch_seconds - (now_epoch_seconds % 86_400);

        let minute_count = sum_window(conn, user_id, channel, minute_start)?;
        if minute_count >= self.config.requests_per_minute as i64 {
            return Ok(RateLimitDecision {
                allowed: false,
                reason: Some("requests_per_minute_exceeded".to_string()),
                retry_after_seconds: 60,
            });
        }

        let hour_count = sum_window(conn, user_id, channel, hour_start)?;
        if hour_count >= self.config.requests_per_hour as i64 {
            return Ok(RateLimitDecision {
                allowed: false,
                reason: Some("requests_per_hour_exceeded".to_string()),
                retry_after_seconds: 3600,
            });
        }

        if self.config.cost_per_day_cap > 0 {
            let total_cost = current_day_cost(conn, user_id, channel, day_start)?;
            let projected = total_cost.saturating_add(request_cost as i64);
            if projected > self.config.cost_per_day_cap as i64 {
                return Ok(RateLimitDecision {
                    allowed: false,
                    reason: Some("cost_per_day_cap_exceeded".to_string()),
                    retry_after_seconds: 86_400,
                });
            }
        }

        conn.execute(
            "insert into rate_limits (user_id, channel, window_start, request_count)
             values (?1, ?2, ?3, 1)
             on conflict(user_id, channel, window_start)
             do update set request_count = request_count + 1",
            params![user_id, channel, now_epoch_seconds],
        )
        .map_err(|err| format!("rate limit write failed: {err}"))?;

        if self.config.cost_per_day_cap > 0 && request_cost > 0 {
            conn.execute(
                "insert into rate_limit_costs (user_id, channel, day_start, total_cost)
                 values (?1, ?2, ?3, ?4)
                 on conflict(user_id, channel, day_start)
                 do update set total_cost = total_cost + excluded.total_cost",
                params![user_id, channel, day_start, request_cost as i64],
            )
            .map_err(|err| format!("rate limit cost write failed: {err}"))?;
        }

        Ok(RateLimitDecision {
            allowed: true,
            reason: None,
            retry_after_seconds: 0,
        })
    }
}

fn sum_window(
    conn: &Connection,
    user_id: &str,
    channel: &str,
    from_epoch_second: i64,
) -> Result<i64, String> {
    conn.query_row(
        "select coalesce(sum(request_count), 0)
         from rate_limits
         where user_id = ?1 and channel = ?2 and window_start >= ?3",
        params![user_id, channel, from_epoch_second],
        |row| row.get(0),
    )
    .map_err(|err| format!("rate limit query failed: {err}"))
}

fn current_day_cost(
    conn: &Connection,
    user_id: &str,
    channel: &str,
    day_start: i64,
) -> Result<i64, String> {
    conn.query_row(
        "select coalesce(total_cost, 0)
         from rate_limit_costs
         where user_id = ?1 and channel = ?2 and day_start = ?3",
        params![user_id, channel, day_start],
        |row| row.get(0),
    )
    .or_else(|err| {
        if matches!(err, rusqlite::Error::QueryReturnedNoRows) {
            Ok(0)
        } else {
            Err(err)
        }
    })
    .map_err(|err| format!("rate limit cost query failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::{RateLimitConfig, RateLimiter};
    use crate::initialize_schema;

    #[test]
    fn rate_limit_sliding_windows() {
        let conn = match rusqlite::Connection::open_in_memory() {
            Ok(value) => value,
            Err(err) => panic!("db open failed: {err}"),
        };
        if let Err(err) = initialize_schema(&conn) {
            panic!("schema init failed: {err}");
        }

        let limiter = RateLimiter::new(RateLimitConfig {
            requests_per_minute: 2,
            requests_per_hour: 5,
            cost_per_day_cap: 0,
        });

        let t0 = 1_700_000_000i64;
        let first = limiter.check_and_record(&conn, "alice", "websocket", t0, 0);
        assert!(first.map(|v| v.allowed).unwrap_or(false));

        let second = limiter.check_and_record(&conn, "alice", "websocket", t0 + 1, 0);
        assert!(second.map(|v| v.allowed).unwrap_or(false));

        let denied = limiter
            .check_and_record(&conn, "alice", "websocket", t0 + 2, 0)
            .unwrap_or_else(|_| panic!("rate limit failed"));
        assert!(!denied.allowed);

        let allowed_after_window = limiter
            .check_and_record(&conn, "alice", "websocket", t0 + 65, 0)
            .unwrap_or_else(|_| panic!("rate limit failed"));
        assert!(allowed_after_window.allowed);
    }

    #[test]
    fn daily_cost_cap_enforced() {
        let conn = match rusqlite::Connection::open_in_memory() {
            Ok(value) => value,
            Err(err) => panic!("db open failed: {err}"),
        };
        if let Err(err) = initialize_schema(&conn) {
            panic!("schema init failed: {err}");
        }

        let limiter = RateLimiter::new(RateLimitConfig {
            requests_per_minute: 100,
            requests_per_hour: 1000,
            cost_per_day_cap: 10,
        });

        let t0 = 1_700_000_000i64;
        let first = limiter
            .check_and_record(&conn, "alice", "telegram", t0, 6)
            .unwrap_or_else(|_| panic!("rate limit failed"));
        assert!(first.allowed);

        let denied = limiter
            .check_and_record(&conn, "alice", "telegram", t0 + 1, 5)
            .unwrap_or_else(|_| panic!("rate limit failed"));
        assert!(!denied.allowed);
        assert_eq!(denied.reason.as_deref(), Some("cost_per_day_cap_exceeded"));
    }
}
