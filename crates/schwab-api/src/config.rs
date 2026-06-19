use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{OAUTH_AUTHORIZE_URL, OAUTH_TOKEN_URL, TRADER_BASE_URL};

/// Runtime configuration for the Schwab API client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    pub app_key: String,
    pub app_secret: String,
    pub redirect_uri: String,
    pub trader_base_url: String,
    pub oauth_authorize_url: String,
    pub oauth_token_url: String,
    pub token_dir: PathBuf,
}

impl ClientConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let app_key = env_first(&["SCHWAB_APP_KEY", "SCHWAB_CLIENT_ID"])
            .ok_or_else(|| anyhow::anyhow!("SCHWAB_APP_KEY (or SCHWAB_CLIENT_ID) is required"))?;
        let app_secret = env_first(&["SCHWAB_APP_SECRET", "SCHWAB_CLIENT_SECRET"])
            .ok_or_else(|| anyhow::anyhow!("SCHWAB_APP_SECRET (or SCHWAB_CLIENT_SECRET) is required"))?;
        let redirect_uri = std::env::var("SCHWAB_REDIRECT_URI")
            .unwrap_or_else(|_| "https://127.0.0.1:8182".to_string());
        let token_dir = std::env::var("SCHWAB_TOKEN_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_token_dir());

        Ok(Self {
            app_key,
            app_secret,
            redirect_uri,
            trader_base_url: TRADER_BASE_URL.to_string(),
            oauth_authorize_url: OAUTH_AUTHORIZE_URL.to_string(),
            oauth_token_url: OAUTH_TOKEN_URL.to_string(),
            token_dir,
        })
    }

    pub fn for_tests() -> Self {
        Self {
            app_key: "test-key".into(),
            app_secret: "test-secret".into(),
            redirect_uri: "https://127.0.0.1:8182".into(),
            trader_base_url: TRADER_BASE_URL.to_string(),
            oauth_authorize_url: OAUTH_AUTHORIZE_URL.to_string(),
            oauth_token_url: OAUTH_TOKEN_URL.to_string(),
            token_dir: std::env::temp_dir().join("schwabinvestbot-test"),
        }
    }
}

fn env_first(keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| std::env::var(key).ok())
        .filter(|v| !v.is_empty())
}

fn default_token_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "schwabinvestbot")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".schwabinvestbot"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_tests_has_defaults() {
        let cfg = ClientConfig::for_tests();
        assert_eq!(cfg.trader_base_url, TRADER_BASE_URL);
    }
}
