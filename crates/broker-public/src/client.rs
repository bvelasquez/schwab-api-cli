use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde_json::Value;
use tokio::sync::Mutex;

const BASE: &str = "https://api.public.com";

struct CachedToken {
    access: String,
    refresh_at: Instant,
}

pub struct PublicClient {
    http: Client,
    secret: String,
    token: Mutex<Option<CachedToken>>,
    last_request: Mutex<Option<Instant>>,
    min_interval: Duration,
}

impl PublicClient {
    pub fn new(secret: String) -> Self {
        Self {
            http: Client::builder()
                .gzip(true)
                .build()
                .expect("reqwest client"),
            secret,
            token: Mutex::new(None),
            last_request: Mutex::new(None),
            min_interval: Duration::from_millis(100),
        }
    }

    pub fn from_env() -> Result<Self> {
        let secret = std::env::var("PUBLIC_API_KEY")
            .or_else(|_| std::env::var("PUBLIC_PERSONAL_SECRET"))
            .context("Set PUBLIC_API_KEY (or PUBLIC_PERSONAL_SECRET)")?;
        if secret.trim().is_empty() {
            anyhow::bail!("PUBLIC_API_KEY is empty");
        }
        Ok(Self::new(secret))
    }

    async fn access_token(&self) -> Result<String> {
        let mut slot = self.token.lock().await;
        if let Some(token) = slot.as_ref() {
            if Instant::now() < token.refresh_at {
                return Ok(token.access.clone());
            }
        }
        let response = self
            .http
            .post(format!("{BASE}/userapiauthservice/personal/access-tokens"))
            .json(&serde_json::json!({
                "secret": self.secret,
                "validityInMinutes": 15
            }))
            .send()
            .await
            .context("Public token request failed")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!(
                "Public token exchange failed (HTTP {status}): {}",
                redact(&body, &self.secret)
            );
        }
        let parsed: Value = serde_json::from_str(&body).context("Public token response was not JSON")?;
        let access = parsed
            .get("accessToken")
            .and_then(|value| value.as_str())
            .context("Public token response missing accessToken")?
            .to_string();
        *slot = Some(CachedToken {
            access: access.clone(),
            refresh_at: Instant::now() + Duration::from_secs(14 * 60),
        });
        Ok(access)
    }

    async fn pace(&self) {
        let mut last = self.last_request.lock().await;
        if let Some(previous) = *last {
            let elapsed = previous.elapsed();
            if elapsed < self.min_interval {
                tokio::time::sleep(self.min_interval - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }

    pub async fn get_json(&self, path: &str) -> Result<Value> {
        self.send(reqwest::Method::GET, path, None).await
    }

    pub async fn delete(&self, path: &str) -> Result<Value> {
        self.send(reqwest::Method::DELETE, path, None).await
    }

    pub async fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        self.send(reqwest::Method::POST, path, Some(body)).await
    }

    pub async fn put_json(&self, path: &str, body: &Value) -> Result<Value> {
        self.send(reqwest::Method::PUT, path, Some(body)).await
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Option<&Value>) -> Result<Value> {
        self.pace().await;
        let token = self.access_token().await?;
        let url = format!("{BASE}{path}");
        let mut request = self
            .http
            .request(method, &url)
            .bearer_auth(token)
            .header("accept", "application/json");
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.with_context(|| format!("Public request {path} failed"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let sent = body
                .map(|value| value.to_string())
                .unwrap_or_default()
                .chars()
                .take(400)
                .collect::<String>();
            return Err(anyhow!(
                "Public {} failed (HTTP {status}): {} request: {}",
                redact_path(path),
                redact(&text, &self.secret),
                redact(&sent, &self.secret)
            ));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).with_context(|| format!("Public {path} returned non-JSON"))
    }
}

fn redact_path(path: &str) -> String {
    match std::env::var("PUBLIC_ACCOUNT_ID") {
        Ok(id) if !id.is_empty() => path.replace(&id, "<account>"),
        _ => path.to_string(),
    }
}

fn redact(body: &str, secret: &str) -> String {
    let clipped: String = body.chars().take(500).collect();
    if secret.is_empty() {
        return clipped;
    }
    clipped.replace(secret, "[redacted]")
}
