use anyhow::{Context, Result};
use serde_json::json;

use crate::rules::TelegramNotifyConfig;

const TELEGRAM_API: &str = "https://api.telegram.org";

pub struct TelegramNotifier {
    http: reqwest::Client,
    token: String,
    chat_id: String,
    config: TelegramNotifyConfig,
}

impl TelegramNotifier {
    pub fn from_env(config: &TelegramNotifyConfig) -> Result<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }
        let token = std::env::var("TELEGRAM_BOT_TOKEN")
            .context("TELEGRAM_BOT_TOKEN required when notify.telegram.enabled is true")?;
        let chat_id = std::env::var("TELEGRAM_CHAT_ID")
            .context("TELEGRAM_CHAT_ID required when notify.telegram.enabled is true")?;
        Ok(Some(Self {
            http: reqwest::Client::new(),
            token,
            chat_id,
            config: config.clone(),
        }))
    }

    pub async fn send(&self, text: &str) -> Result<()> {
        let url = format!("{TELEGRAM_API}/bot{}/sendMessage", self.token);
        let resp = self
            .http
            .post(&url)
            .json(&json!({
                "chat_id": self.chat_id,
                "text": truncate_message(text, 4000),
                "disable_web_page_preview": true,
            }))
            .send()
            .await
            .context("Telegram send failed")?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Telegram API error: {body}");
        }
        Ok(())
    }

    pub fn wants_tick_summary(&self) -> bool {
        self.config.notify_every_tick
    }

    pub fn wants_actions(&self) -> bool {
        self.config.notify_on_actions
    }
}

fn truncate_message(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars().take(max.saturating_sub(1)) {
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_long_messages() {
        let msg = "a".repeat(5000);
        assert_eq!(truncate_message(&msg, 4000).chars().count(), 4000);
    }
}
