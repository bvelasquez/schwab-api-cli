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
    /// When the interactive OAuth login happened. Refreshing an access token does NOT
    /// extend the underlying refresh token's 7-day hard expiry, so this must be carried
    /// forward across refreshes rather than reset. `None` means the token file predates
    /// this field (login age is unknown, not "fresh").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_at: Option<DateTime<Utc>>,
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

    /// Seconds since the interactive login, or `None` if `login_at` is unknown
    /// (token file predates login tracking).
    pub fn refresh_age_seconds(&self) -> Option<i64> {
        self.login_at
            .map(|at| (Utc::now() - at).num_seconds().max(0))
    }

    /// Seconds until the refresh token hard-expires, or `None` if unknown. Callers must
    /// not treat `None` as "fresh" — it means re-login is required to start tracking.
    pub fn refresh_expires_in_seconds(&self) -> Option<i64> {
        self.refresh_age_seconds()
            .map(|age| (REFRESH_TOKEN_LIFETIME_SECS - age).max(0))
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
        if self.path.is_file() {
            if let Err(err) = crate::atomic::restrict_owner_file(&self.path) {
                tracing::warn!(%err, path = %self.path.display(), "could not restrict token file mode");
            }
            if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
                if let Err(err) = crate::atomic::restrict_owner_dir(parent) {
                    tracing::warn!(%err, path = %parent.display(), "could not restrict token directory mode");
                }
            }
        }
        match fs::read_to_string(&self.path).await {
            Ok(raw) if raw.trim().is_empty() => Err(ApiError::NotAuthenticated(format!(
                "token file is empty ({}). Run `schwab auth login`",
                self.path.display()
            ))),
            Ok(raw) => serde_json::from_str(&raw).map(Some).map_err(|e| {
                ApiError::NotAuthenticated(format!(
                    "token file is invalid ({}): {e}. Run `schwab auth login`",
                    self.path.display()
                ))
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(ApiError::TokenStore(err.to_string())),
        }
    }

    pub async fn save(&self, tokens: &Tokens) -> Result<()> {
        let raw = serde_json::to_string_pretty(tokens)?;
        crate::atomic::write_atomic_private(&self.path, raw)
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
        // Interactive login: this is the moment the refresh token's 7-day clock starts.
        let tokens = self
            .token_request(
                &[
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", &self.config.redirect_uri),
                ],
                Some(Utc::now()),
            )
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

        // Refreshing the access token does NOT reset the refresh token's hard expiry, so
        // carry forward the original login_at (possibly None for pre-existing token files).
        let tokens = self
            .token_request(
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &existing.refresh_token),
                ],
                existing.login_at,
            )
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

    async fn token_request(
        &self,
        params: &[(&str, &str)],
        login_at: Option<DateTime<Utc>>,
    ) -> Result<Tokens> {
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
            ApiError::OAuth(format!(
                "Token response parse error: {e} (response body omitted)"
            ))
        })?;
        Ok(Tokens {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token,
            token_type: parsed.token_type,
            expires_at: Utc::now() + chrono::Duration::seconds(parsed.expires_in),
            scope: parsed.scope,
            obtained_at: Utc::now(),
            login_at,
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

    fn sample_tokens(login_at: Option<DateTime<Utc>>) -> Tokens {
        Tokens {
            access_token: "a".into(),
            refresh_token: "r".into(),
            token_type: "Bearer".into(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            scope: None,
            obtained_at: Utc::now(),
            login_at,
        }
    }

    #[test]
    fn refresh_expiry_counts_down_from_login_at() {
        let tokens = sample_tokens(Some(Utc::now() - chrono::Duration::days(6)));
        assert!(tokens.refresh_expires_in_seconds().unwrap() < 2 * 86400);
    }

    #[test]
    fn refresh_expiry_ignores_obtained_at_bump_from_a_refresh() {
        // A keeper refreshes every ~30 min, bumping obtained_at, but login_at (and thus
        // refresh expiry) must not move — this is the exact bug being fixed.
        let mut tokens = sample_tokens(Some(Utc::now() - chrono::Duration::days(6)));
        let before = tokens.refresh_expires_in_seconds().unwrap();
        tokens.obtained_at = Utc::now();
        assert_eq!(tokens.refresh_expires_in_seconds().unwrap(), before);
    }

    #[test]
    fn refresh_expiry_is_unknown_without_login_at() {
        let tokens = sample_tokens(None);
        assert_eq!(tokens.refresh_age_seconds(), None);
        assert_eq!(tokens.refresh_expires_in_seconds(), None);
    }

    #[test]
    fn legacy_token_file_without_login_at_loads_as_unknown() {
        let raw = r#"{
            "access_token": "a",
            "refresh_token": "r",
            "token_type": "Bearer",
            "expires_at": "2020-01-01T00:00:00Z",
            "scope": null,
            "obtained_at": "2020-01-01T00:00:00Z"
        }"#;
        let tokens: Tokens = serde_json::from_str(raw).unwrap();
        assert_eq!(tokens.login_at, None);
        assert_eq!(tokens.refresh_expires_in_seconds(), None);
    }

    #[test]
    fn mirror_file_without_refresh_token_or_login_at_loads() {
        let raw = r#"{
            "access_token": "a",
            "refresh_token": "",
            "token_type": "Bearer",
            "expires_at": "2020-01-01T00:00:00Z",
            "scope": null,
            "obtained_at": "2020-01-01T00:00:00Z"
        }"#;
        let tokens: Tokens = serde_json::from_str(raw).unwrap();
        assert_eq!(tokens.refresh_token, "");
        assert_eq!(tokens.login_at, None);
    }

    #[test]
    fn mirror_file_with_login_at_loads() {
        let raw = r#"{
            "access_token": "a",
            "refresh_token": "",
            "token_type": "Bearer",
            "expires_at": "2020-01-01T00:00:00Z",
            "scope": null,
            "obtained_at": "2020-01-01T00:00:00Z",
            "login_at": "2020-01-01T00:00:00Z"
        }"#;
        let tokens: Tokens = serde_json::from_str(raw).unwrap();
        assert!(tokens.login_at.is_some());
        assert!(tokens.refresh_expires_in_seconds().is_some());
    }

    /// Binds a localhost listener that replies once with `body` for any request, then
    /// returns the base URL to point `ClientConfig::oauth_token_url` at.
    async fn spawn_token_endpoint(body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = stream.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    fn test_config(token_url: String, token_dir: PathBuf) -> ClientConfig {
        let mut config = ClientConfig::for_tests();
        config.oauth_token_url = token_url;
        config.token_dir = token_dir;
        config
    }

    fn temp_token_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "schwab-auth-{label}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ))
    }

    const SAMPLE_TOKEN_RESPONSE: &str = r#"{"access_token":"new-access","refresh_token":"new-refresh","token_type":"Bearer","expires_in":1800,"scope":"api"}"#;

    #[tokio::test]
    async fn login_sets_login_at() {
        let url = spawn_token_endpoint(SAMPLE_TOKEN_RESPONSE).await;
        let dir = temp_token_dir("login");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let oauth = OAuthClient::new(test_config(url, dir.clone()));

        let before = Utc::now();
        let tokens = oauth.exchange_code("some-code").await.unwrap();

        let login_at = tokens.login_at.expect("login must set login_at");
        assert!(login_at >= before - chrono::Duration::seconds(2));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn refresh_preserves_login_at() {
        let url = spawn_token_endpoint(SAMPLE_TOKEN_RESPONSE).await;
        let dir = temp_token_dir("refresh-preserve");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let original_login_at = Utc::now() - chrono::Duration::days(5);
        let store = TokenStore::new(dir.clone());
        store
            .save(&sample_tokens(Some(original_login_at)))
            .await
            .unwrap();

        let oauth = OAuthClient::new(test_config(url, dir.clone()));
        let refreshed = oauth.refresh().await.unwrap();

        // obtained_at moves forward (new access token issued)...
        assert!(refreshed.obtained_at >= original_login_at);
        // ...but login_at (and thus refresh expiry) does not reset.
        assert_eq!(refreshed.login_at, Some(original_login_at));
        assert_eq!(refreshed.access_token, "new-access");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn refresh_of_legacy_token_stays_unknown() {
        let url = spawn_token_endpoint(SAMPLE_TOKEN_RESPONSE).await;
        let dir = temp_token_dir("refresh-legacy");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let store = TokenStore::new(dir.clone());
        // Simulate a pre-fix token file: no login_at at all.
        store.save(&sample_tokens(None)).await.unwrap();

        let oauth = OAuthClient::new(test_config(url, dir.clone()));
        let refreshed = oauth.refresh().await.unwrap();

        assert_eq!(refreshed.login_at, None);
        assert_eq!(refreshed.refresh_expires_in_seconds(), None);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn empty_token_file_is_not_authenticated() {
        let dir = std::env::temp_dir().join(format!(
            "schwab-empty-tokens-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("tokens.json");
        tokio::fs::write(&path, b"").await.unwrap();
        let store = TokenStore::new(dir.clone());
        let err = store.load().await.expect_err("empty tokens must fail");
        let msg = err.to_string();
        assert!(msg.contains("token file is empty"), "{msg}");
        assert!(msg.contains("schwab auth login"), "{msg}");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn invalid_token_json_is_not_authenticated() {
        let dir = std::env::temp_dir().join(format!(
            "schwab-bad-tokens-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(1)
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("tokens.json"), b"{not-json")
            .await
            .unwrap();
        let store = TokenStore::new(dir.clone());
        let err = store.load().await.expect_err("invalid tokens must fail");
        let msg = err.to_string();
        assert!(msg.contains("token file is invalid"), "{msg}");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
