//! Schwab OAuth refresh-token expiry warnings (~7 day lifetime).

use chrono::{DateTime, Utc};
use schwab_api::{ClientConfig, Tokens};

use crate::agent::state::AgentState;
use crate::notify::TelegramNotifier;

/// Warn when ≤2 days remain; urgent when ≤1 day.
const SOON_THRESHOLD_SECS: i64 = 2 * 86400;
const URGENT_THRESHOLD_SECS: i64 = 86400;
const REMINDER_COOLDOWN_SECS: i64 = 24 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthReminderLevel {
    None,
    Soon,
    Urgent,
    Expired,
}

impl AuthReminderLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Soon => "soon",
            Self::Urgent => "urgent",
            Self::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuthReminder {
    pub level: AuthReminderLevel,
    pub obtained_at: DateTime<Utc>,
    pub refresh_expires_in_seconds: i64,
    pub access_expires_in_seconds: i64,
    pub message: String,
}

pub fn assess_refresh_token(tokens: &Tokens) -> AuthReminder {
    let refresh_expires_in_seconds = tokens.refresh_expires_in_seconds();
    let access_expires_in_seconds = tokens.expires_in_seconds();

    let mut level = if refresh_expires_in_seconds <= 0 {
        AuthReminderLevel::Expired
    } else if refresh_expires_in_seconds <= URGENT_THRESHOLD_SECS {
        AuthReminderLevel::Urgent
    } else if refresh_expires_in_seconds <= SOON_THRESHOLD_SECS {
        AuthReminderLevel::Soon
    } else {
        AuthReminderLevel::None
    };

    if tokens.is_expired() && level == AuthReminderLevel::None {
        level = AuthReminderLevel::Soon;
    }

    let message = match level {
        AuthReminderLevel::None => String::new(),
        AuthReminderLevel::Soon => format!(
            "Schwab login due in ~{} — run: schwab auth login",
            format_days(refresh_expires_in_seconds)
        ),
        AuthReminderLevel::Urgent => format!(
            "Schwab login needed within ~{} — run: schwab auth login now",
            format_days(refresh_expires_in_seconds)
        ),
        AuthReminderLevel::Expired => {
            "Schwab refresh token expired — run: schwab auth login".to_string()
        }
    };

    AuthReminder {
        level,
        obtained_at: tokens.obtained_at,
        refresh_expires_in_seconds,
        access_expires_in_seconds,
        message,
    }
}

impl AuthReminder {
    /// Extra context for status UIs (token issued, access + refresh horizons).
    pub fn detail_line(&self) -> String {
        let issued = format_ago((Utc::now() - self.obtained_at).num_seconds());
        format!(
            "issued {issued} ago · access {} · refresh {}",
            format_duration(self.access_expires_in_seconds),
            format_days(self.refresh_expires_in_seconds)
        )
    }
}

pub fn load_auth_reminder() -> Option<AuthReminder> {
    let config = ClientConfig::from_env().ok()?;
    let path = config.token_dir.join("tokens.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let tokens: Tokens = serde_json::from_str(&raw).ok()?;
    Some(assess_refresh_token(&tokens))
}

pub fn should_send_reminder(state: &AgentState, level: AuthReminderLevel) -> bool {
    if level == AuthReminderLevel::None {
        return false;
    }
    let level_s = level.as_str();
    match (
        state.last_auth_reminder_level.as_deref(),
        state.last_auth_reminder_at,
    ) {
        (Some(prev), Some(at)) if prev == level_s => {
            (Utc::now() - at).num_seconds() >= REMINDER_COOLDOWN_SECS
        }
        (Some(prev), _) if prev != level_s => true,
        (None, _) => true,
        _ => true,
    }
}

pub fn record_reminder_sent(state: &mut AgentState, level: AuthReminderLevel) {
    state.last_auth_reminder_level = Some(level.as_str().to_string());
    state.last_auth_reminder_at = Some(Utc::now());
}

pub async fn maybe_notify_auth_reminder(
    telegram: Option<&TelegramNotifier>,
    state: &mut AgentState,
    reminder: &AuthReminder,
) {
    if reminder.level == AuthReminderLevel::None {
        return;
    }
    if !should_send_reminder(state, reminder.level) {
        return;
    }
    let Some(tg) = telegram else {
        return;
    };
    let text = format!("SCHWAB AUTH REMINDER\n{}", reminder.message);
    if tg.send(&text).await.is_ok() {
        record_reminder_sent(state, reminder.level);
    }
}

pub async fn notify_auth_required(telegram: Option<&TelegramNotifier>, detail: &str) {
    let Some(tg) = telegram else {
        return;
    };
    let _ = tg
        .send(&format!(
            "SCHWAB AUTH REQUIRED\n{detail}\nRun: schwab auth login"
        ))
        .await;
}

fn format_days(secs: i64) -> String {
    if secs < 3600 {
        format!("{}m", (secs / 60).max(1))
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

fn format_duration(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

fn format_ago(secs: i64) -> String {
    if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tokens(obtained_days_ago: i64) -> Tokens {
        Tokens {
            access_token: "a".into(),
            refresh_token: "r".into(),
            token_type: "Bearer".into(),
            expires_at: Utc::now() + chrono::Duration::minutes(20),
            scope: None,
            obtained_at: Utc::now() - chrono::Duration::days(obtained_days_ago),
        }
    }

    #[test]
    fn urgent_when_one_day_left() {
        let tokens = sample_tokens(6);
        let r = assess_refresh_token(&tokens);
        assert_eq!(r.level, AuthReminderLevel::Urgent);
    }

    #[test]
    fn none_when_fresh() {
        let tokens = sample_tokens(1);
        let r = assess_refresh_token(&tokens);
        assert_eq!(r.level, AuthReminderLevel::None);
    }

    #[test]
    fn dedupe_same_level_within_cooldown() {
        let state = AgentState {
            last_auth_reminder_level: Some("urgent".into()),
            last_auth_reminder_at: Some(Utc::now()),
            ..Default::default()
        };
        assert!(!should_send_reminder(&state, AuthReminderLevel::Urgent));
    }
}
