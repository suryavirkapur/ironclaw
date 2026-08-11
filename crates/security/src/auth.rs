use crate::pairing::constant_time_eq;

pub fn channel_allowed(allowed_channels: &[String], channel: &str) -> bool {
    if allowed_channels.is_empty() {
        return true;
    }
    allowed_channels.iter().any(|value| value == channel)
}

pub fn validate_webhook_secret(expected: &str, provided: Option<&str>) -> bool {
    if expected.is_empty() {
        return true;
    }
    let Some(got) = provided else {
        return false;
    };
    constant_time_eq(expected.as_bytes(), got.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{channel_allowed, validate_webhook_secret};

    #[test]
    fn channel_allowlist_behavior() {
        let empty = Vec::<String>::new();
        assert!(channel_allowed(&empty, "websocket"));

        let allow = vec!["telegram".to_string(), "whatsapp".to_string()];
        assert!(channel_allowed(&allow, "telegram"));
        assert!(!channel_allowed(&allow, "websocket"));
    }

    #[test]
    fn webhook_secret_validation() {
        assert!(validate_webhook_secret("", None));
        assert!(validate_webhook_secret("abc", Some("abc")));
        assert!(!validate_webhook_secret("abc", Some("abd")));
        assert!(!validate_webhook_secret("abc", None));
    }
}
