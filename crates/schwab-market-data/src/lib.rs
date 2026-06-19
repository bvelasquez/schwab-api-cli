//! Charles Schwab Market Data API client (`https://api.schwabapi.com/marketdata/v1`).

pub mod endpoints;

pub use endpoints::MarketDataApi;
pub use schwab_api::MARKET_DATA_BASE_URL;
