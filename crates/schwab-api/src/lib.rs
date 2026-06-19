//! Charles Schwab Trader API client (`https://api.schwabapi.com/trader/v1`).

pub mod auth;
pub mod client;
pub mod config;
pub mod endpoints;
pub mod error;
pub mod models;
pub mod query;

pub use auth::{OAuthClient, TokenStore, Tokens};
pub use client::SchwabClient;
pub use config::ClientConfig;
pub use endpoints::TraderApi;
pub use error::{ApiError, Result};
pub use query::{default_order_window, default_orders_all_window, default_transaction_window, iso8601_ms, resolve_time_range};

pub const TRADER_BASE_URL: &str = "https://api.schwabapi.com/trader/v1";
pub const OAUTH_AUTHORIZE_URL: &str = "https://api.schwabapi.com/v1/oauth/authorize";
pub const OAUTH_TOKEN_URL: &str = "https://api.schwabapi.com/v1/oauth/token";
