use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API error {status}: {message}")]
    Api { status: u16, message: String },

    #[error("OAuth error: {0}")]
    OAuth(String),

    #[error("Not authenticated: {0}")]
    NotAuthenticated(String),

    #[error("Token store error: {0}")]
    TokenStore(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ApiError>;
