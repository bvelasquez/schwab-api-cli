use reqwest::{Client, Method, Response};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::debug;

use crate::auth::OAuthClient;
use crate::config::ClientConfig;
use crate::error::{ApiError, Result};

/// Response for POST/PUT/DELETE calls that may return an empty body.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MutationResponse {
    pub status: u16,
    pub location: Option<String>,
}

/// Authenticated HTTP client for Trader API v1.
#[derive(Debug, Clone)]
pub struct SchwabClient {
    http: Client,
    config: ClientConfig,
    oauth: OAuthClient,
}

impl SchwabClient {
    pub fn new(config: ClientConfig) -> Self {
        let oauth = OAuthClient::new(config.clone());
        Self {
            http: Client::builder()
                .gzip(true)
                .build()
                .expect("reqwest client"),
            config,
            oauth,
        }
    }

    pub fn oauth(&self) -> &OAuthClient {
        &self.oauth
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        self.request(
            &self.config.trader_base_url,
            Method::GET,
            path,
            query,
            None::<&Value>,
        )
        .await
    }

    /// GET against Market Data Production (`/marketdata/v1`).
    pub async fn get_market_data_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        self.request(
            crate::MARKET_DATA_BASE_URL,
            Method::GET,
            path,
            query,
            None::<&Value>,
        )
        .await
    }

    pub async fn post_json<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        self.request(
            &self.config.trader_base_url,
            Method::POST,
            path,
            &[],
            Some(body),
        )
        .await
    }

    pub async fn put_json<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        self.request(
            &self.config.trader_base_url,
            Method::PUT,
            path,
            &[],
            Some(body),
        )
        .await
    }

    pub async fn post_mutate(&self, path: &str, body: &Value) -> Result<MutationResponse> {
        self.mutate(Method::POST, path, Some(body)).await
    }

    pub async fn put_mutate(&self, path: &str, body: &Value) -> Result<MutationResponse> {
        self.mutate(Method::PUT, path, Some(body)).await
    }

    pub async fn delete_mutate(&self, path: &str) -> Result<MutationResponse> {
        self.mutate(Method::DELETE, path, None).await
    }

    async fn mutate(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<MutationResponse> {
        let token = self.oauth.ensure_access_token().await?;
        let url = format!("{}{}", self.config.trader_base_url, path);
        debug!(%url, ?method, "API mutation");

        let mut req = self
            .http
            .request(method, &url)
            .bearer_auth(token)
            .header("Accept", "application/json");

        if let Some(body) = body {
            req = req.json(body);
        }

        let response = req.send().await?;
        let response = Self::ensure_success(response).await?;
        Ok(Self::mutation_response(response))
    }

    async fn request<T: DeserializeOwned>(
        &self,
        base_url: &str,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<T> {
        let token = self.oauth.ensure_access_token().await?;
        let url = format!("{base_url}{path}");
        debug!(%url, ?method, "API request");

        let mut req = self
            .http
            .request(method, &url)
            .bearer_auth(token)
            .header("Accept", "application/json");

        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(body) = body {
            req = req.json(body);
        }

        let response = req.send().await?;
        let response = Self::ensure_success(response).await?;

        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Err(ApiError::Other("Unexpected empty response body".into()));
        }

        let bytes = response.bytes().await?;
        if bytes.is_empty() {
            return Err(ApiError::Other("Unexpected empty response body".into()));
        }

        Ok(serde_json::from_slice(&bytes)?)
    }

    fn mutation_response(response: Response) -> MutationResponse {
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get("Location")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        MutationResponse { status, location }
    }

    async fn ensure_success(response: Response) -> Result<Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let message = response.text().await.unwrap_or_default();
        Err(ApiError::Api {
            status: status.as_u16(),
            message,
        })
    }
}
