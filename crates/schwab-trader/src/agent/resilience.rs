//! Classify agent tick failures so the loop can backoff instead of exiting.

/// How the agent should react to a tick (or startup) error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentErrorClass {
    /// Network blip, 5xx, timeouts — short backoff and retry.
    Recoverable,
    /// Refresh token revoked / missing — stay alive, long backoff, wait for re-login.
    AuthFatal,
    /// Unexpected errors — backoff and keep trying (unattended bot must not die).
    Unexpected,
}

const MIN_BACKOFF_SECS: u64 = 5;
const MAX_BACKOFF_SECS: u64 = 300;
const AUTH_FATAL_BACKOFF_SECS: u64 = 60;

/// Classify an error from its Display / Debug chain text.
pub fn classify_agent_error(err: &anyhow::Error) -> AgentErrorClass {
    classify_error_message(&format!("{err:#}"))
}

pub fn classify_error_message(msg: &str) -> AgentErrorClass {
    let lower = msg.to_ascii_lowercase();

    if lower.contains("invalid_grant")
        || lower.contains("refresh token is invalid")
        || lower.contains("refresh token") && (lower.contains("expired") || lower.contains("revoked"))
        || lower.contains("not authenticated")
        || lower.contains("no refresh token")
        || lower.contains("run: schwab auth login")
        || lower.contains("schwab auth login")
    {
        return AgentErrorClass::AuthFatal;
    }

    // Transient HTTP / transport
    if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("temporarily unavailable")
        || lower.contains("dns")
        || lower.contains("error sending request")
        || lower.contains("hyper::error")
        || lower.contains("api error 429")
        || lower.contains("api error 500")
        || lower.contains("api error 502")
        || lower.contains("api error 503")
        || lower.contains("api error 504")
        || lower.contains("status: 429")
        || lower.contains("status: 500")
        || lower.contains("status: 502")
        || lower.contains("status: 503")
        || lower.contains("status: 504")
        || lower.contains("unexpected empty response body")
        || lower.contains("no space left")
        || lower.contains("os error 28")
    {
        return AgentErrorClass::Recoverable;
    }

    // Access-token 401 often clears after refresh; treat as recoverable unless
    // paired with invalid_grant (already handled above).
    if lower.contains("api error 401") || lower.contains("status: 401") || lower.contains("unauthorized")
    {
        return AgentErrorClass::Recoverable;
    }

    AgentErrorClass::Unexpected
}

/// Exponential backoff from consecutive failure count (1-based).
pub fn backoff_seconds(class: AgentErrorClass, consecutive_failures: u32) -> u64 {
    match class {
        AgentErrorClass::AuthFatal => AUTH_FATAL_BACKOFF_SECS,
        AgentErrorClass::Recoverable | AgentErrorClass::Unexpected => {
            let n = consecutive_failures.max(1).saturating_sub(1);
            let exp = MIN_BACKOFF_SECS.saturating_mul(2u64.saturating_pow(n.min(6)));
            exp.min(MAX_BACKOFF_SECS)
        }
    }
}

pub fn class_label(class: AgentErrorClass) -> &'static str {
    match class {
        AgentErrorClass::Recoverable => "recoverable",
        AgentErrorClass::AuthFatal => "auth_fatal",
        AgentErrorClass::Unexpected => "unexpected",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_invalid_grant_as_auth_fatal() {
        let msg = r#"OAuth error: HTTP 400: 400 Bad Request: "{"error_description":"Refresh token is invalid, expired or revoked","error":"invalid_grant"}""#;
        assert_eq!(classify_error_message(msg), AgentErrorClass::AuthFatal);
    }

    #[test]
    fn classifies_not_authenticated_as_auth_fatal() {
        assert_eq!(
            classify_error_message("Not authenticated: No refresh token on disk"),
            AgentErrorClass::AuthFatal
        );
        assert_eq!(
            classify_error_message(
                "Not authenticated: token file is empty (/home/jarvis/.config/schwabinvestbot/tokens.json). Run `schwab auth login`"
            ),
            AgentErrorClass::AuthFatal
        );
    }

    #[test]
    fn classifies_401_as_recoverable() {
        assert_eq!(
            classify_error_message("API error 401: "),
            AgentErrorClass::Recoverable
        );
    }

    #[test]
    fn classifies_5xx_and_timeout_as_recoverable() {
        assert_eq!(
            classify_error_message("API error 503: service unavailable"),
            AgentErrorClass::Recoverable
        );
        assert_eq!(
            classify_error_message("HTTP request failed: timeout"),
            AgentErrorClass::Recoverable
        );
        assert_eq!(
            classify_error_message("Unexpected empty response body"),
            AgentErrorClass::Recoverable
        );
        assert_eq!(
            classify_error_message("No space left on device (os error 28)"),
            AgentErrorClass::Recoverable
        );
    }

    #[test]
    fn classifies_unknown_as_unexpected() {
        assert_eq!(
            classify_error_message("something weird broke"),
            AgentErrorClass::Unexpected
        );
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_seconds(AgentErrorClass::Recoverable, 1), 5);
        assert_eq!(backoff_seconds(AgentErrorClass::Recoverable, 2), 10);
        assert_eq!(backoff_seconds(AgentErrorClass::Recoverable, 3), 20);
        assert_eq!(backoff_seconds(AgentErrorClass::Recoverable, 10), 300);
        assert_eq!(backoff_seconds(AgentErrorClass::AuthFatal, 1), 60);
        assert_eq!(backoff_seconds(AgentErrorClass::AuthFatal, 99), 60);
    }
}
