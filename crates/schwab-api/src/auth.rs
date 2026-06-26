use chrono::{DateTime, Utc};
use serde::Deserialize;

/// Schwab refresh tokens are valid for approximately seven days.
pub const REFRESH_TOKEN_LIFETIME_SECS: i64 = 7 * 24 * 3600;

/// OAuth token bundle persisted on disk.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_at: DateTime<Utc>,
    pub scope: Option<String>,
    /// When this token bundle was issued (login or last refresh).
    #[serde(default = "default_obtained_at")]
    pub obtained_at: DateTime<Utc>,
}

fn default_obtained_at() -> DateTime<Utc> {
    Utc::now()
}

impl Tokens {
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at
    }

    pub fn expires_in_seconds(&self) -> i64 {
        (self.expires_at - Utc::now()).num_seconds().max(0)
    }

    pub fn refresh_age_seconds(&self) -> i64 {
        (Utc::now() - self.obtained_at).num_seconds().max(0)
    }

    pub fn refresh_expires_in_seconds(&self) -> i64 {
        (REFRESH_TOKEN_LIFETIME_SECS - self.refresh_age_seconds()).max(0)
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    token_type: String,
    expires_in: i64,
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    error: Option<String>,
    error_description: Option<String>,
    message: Option<String>,
}

use std::path::PathBuf;

use reqwest::Client;
use tokio::fs;
use tracing::{debug, info};

use crate::config::ClientConfig;
use crate::error::{ApiError, Result};

/// File-backed OAuth token storage.
#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn new(token_dir: PathBuf) -> Self {
        Self {
            path: token_dir.join("tokens.json"),
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub async fn load(&self) -> Result<Option<Tokens>> {
        match fs::read_to_string(&self.path).await {
            Ok(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(ApiError::TokenStore(err.to_string())),
        }
    }

    pub async fn save(&self, tokens: &Tokens) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| ApiError::TokenStore(e.to_string()))?;
        }
        let raw = serde_json::to_string_pretty(tokens)?;
        fs::write(&self.path, raw)
            .await
            .map_err(|e| ApiError::TokenStore(e.to_string()))?;
        Ok(())
    }

    pub async fn clear(&self) -> Result<()> {
        match fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(ApiError::TokenStore(err.to_string())),
        }
    }
}

/// Schwab OAuth 2.0 authorization-code client.
#[derive(Debug, Clone)]
pub struct OAuthClient {
    http: Client,
    config: ClientConfig,
    store: TokenStore,
}

impl OAuthClient {
    pub fn new(config: ClientConfig) -> Self {
        let store = TokenStore::new(config.token_dir.clone());
        let http = Client::builder()
            .gzip(true)
            .build()
            .expect("reqwest client");
        Self {
            http,
            config,
            store,
        }
    }

    pub fn store(&self) -> &TokenStore {
        &self.store
    }

    pub fn authorize_url(&self) -> String {
        let mut url =
            url::Url::parse(&self.config.oauth_authorize_url).expect("valid oauth authorize url");
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("client_id", &self.config.app_key);
            pairs.append_pair("redirect_uri", &self.config.redirect_uri);
            pairs.append_pair("response_type", "code");
        }
        url.to_string()
    }

    pub async fn exchange_code(&self, code: &str) -> Result<Tokens> {
        let tokens = self
            .token_request(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &self.config.redirect_uri),
            ])
            .await?;
        self.store.save(&tokens).await?;
        info!("OAuth tokens saved");
        Ok(tokens)
    }

    pub async fn refresh(&self) -> Result<Tokens> {
        let existing = self
            .store
            .load()
            .await?
            .ok_or_else(|| ApiError::NotAuthenticated("No refresh token on disk".into()))?;

        let tokens = self
            .token_request(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &existing.refresh_token),
            ])
            .await?;
        self.store.save(&tokens).await?;
        info!("OAuth tokens refreshed");
        Ok(tokens)
    }

    pub async fn ensure_access_token(&self) -> Result<String> {
        let tokens = match self.store.load().await? {
            Some(tokens) if !tokens.is_expired() => tokens,
            Some(_) => self.refresh().await?,
            None => {
                return Err(ApiError::NotAuthenticated(
                    "Run `schwab auth login` to authenticate".into(),
                ))
            }
        };
        Ok(tokens.access_token)
    }

    pub async fn status(&self) -> Result<Option<Tokens>> {
        self.store.load().await
    }

    pub async fn logout(&self) -> Result<()> {
        self.store.clear().await
    }

    async fn token_request(&self, params: &[(&str, &str)]) -> Result<Tokens> {
        debug!("Requesting OAuth token");
        let response = self
            .http
            .post(&self.config.oauth_token_url)
            .basic_auth(&self.config.app_key, Some(&self.config.app_secret))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .form(params)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(ApiError::OAuth(format_oauth_error(status.as_u16(), &body)));
        }

        let parsed: TokenResponse = serde_json::from_str(&body).map_err(|e| {
            ApiError::OAuth(format!("Token response parse error: {e}; body={body}"))
        })?;
        Ok(Tokens {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token,
            token_type: parsed.token_type,
            expires_at: Utc::now() + chrono::Duration::seconds(parsed.expires_in),
            scope: parsed.scope,
            obtained_at: Utc::now(),
        })
    }
}

fn format_oauth_error(status: u16, body: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<OAuthErrorResponse>(body) {
        let msg = parsed
            .error_description
            .or(parsed.message)
            .or(parsed.error)
            .unwrap_or_else(|| body.to_string());
        return format!("HTTP {status}: {msg}");
    }
    if body.chars().all(|c| c.is_ascii() || c.is_whitespace()) {
        format!("HTTP {status}: {body}")
    } else {
        format!(
            "HTTP {status}: non-text error body ({} bytes). \
             Common causes: expired authorization code (retry login immediately), \
             redirect URI mismatch, or invalid app secret.",
            body.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_json_oauth_error() {
        let body = r#"{"error":"invalid_grant","error_description":"code expired"}"#;
        let msg = format_oauth_error(400, body);
        assert!(msg.contains("code expired"));
    }

    #[test]
    fn refresh_expiry_counts_down_from_obtained_at() {
        let tokens = Tokens {
            access_token: "a".into(),
            refresh_token: "r".into(),
            token_type: "Bearer".into(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            scope: None,
            obtained_at: Utc::now() - chrono::Duration::days(6),
        };
        assert!(tokens.refresh_expires_in_seconds() < 2 * 86400);
    }
}
