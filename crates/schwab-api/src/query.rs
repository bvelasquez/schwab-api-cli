use chrono::{Duration, Utc};

/// ISO-8601 format required by Schwab order/transaction endpoints.
pub fn iso8601_ms(dt: chrono::DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Per-account orders: default last 30 days (API allows up to 1 year).
pub fn default_order_window() -> (String, String) {
    window_days(30)
}

/// All-accounts orders: `fromEnteredTime` must be within 60 days of today.
pub fn default_orders_all_window() -> (String, String) {
    window_days(60)
}

/// Transactions: default last 30 days (API allows up to 1 year).
pub fn default_transaction_window() -> (String, String) {
    window_days(30)
}

fn window_days(days: i64) -> (String, String) {
    let to = Utc::now();
    let from = to - Duration::days(days);
    (iso8601_ms(from), iso8601_ms(to))
}

/// Schwab requires both ends of a range when either is supplied.
pub fn resolve_time_range(
    from: Option<&str>,
    to: Option<&str>,
    default: impl FnOnce() -> (String, String),
) -> Result<(String, String), String> {
    match (from, to) {
        (Some(f), Some(t)) => Ok((f.to_string(), t.to_string())),
        (None, None) => Ok(default()),
        _ => Err("Provide both range parameters or omit both to use CLI defaults".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_window_is_valid_iso8601() {
        let (from, to) = default_order_window();
        assert!(from.ends_with('Z'));
        assert!(to.ends_with('Z'));
        assert!(from < to);
    }

    #[test]
    fn resolve_requires_both_or_neither() {
        assert!(resolve_time_range(Some("a"), None, default_order_window).is_err());
        assert!(resolve_time_range(None, None, default_order_window).is_ok());
    }
}
