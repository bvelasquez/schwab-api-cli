use schwab_api::client::SchwabClient;
use schwab_api::Result;
use serde_json::Value;

pub mod chains;
pub mod instruments;
pub mod markets;
pub mod price_history;
pub mod quotes;

pub use chains::ChainsApi;
pub use instruments::InstrumentsApi;
pub use markets::MarketsApi;
pub use price_history::PriceHistoryApi;
pub use quotes::QuotesApi;

pub(crate) fn opt_query(key: &str, value: Option<&str>) -> Vec<(String, String)> {
    value
        .map(|v| vec![(key.to_string(), v.to_string())])
        .unwrap_or_default()
}

pub(crate) fn opt_query_i64(key: &str, value: Option<i64>) -> Vec<(String, String)> {
    value
        .map(|v| vec![(key.to_string(), v.to_string())])
        .unwrap_or_default()
}

pub(crate) fn opt_query_u32(key: &str, value: Option<u32>) -> Vec<(String, String)> {
    value
        .map(|v| vec![(key.to_string(), v.to_string())])
        .unwrap_or_default()
}

pub(crate) fn opt_query_bool(key: &str, value: Option<bool>) -> Vec<(String, String)> {
    value
        .map(|v| vec![(key.to_string(), v.to_string())])
        .unwrap_or_default()
}

pub(crate) fn merge_queries_str(parts: Vec<Vec<(String, String)>>) -> Vec<(String, String)> {
    parts.into_iter().flatten().collect()
}

async fn get(client: &SchwabClient, path: &str, query: &[(String, String)]) -> Result<Value> {
    let q: Vec<(&str, &str)> = query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    client.get_market_data_json(path, &q).await
}

/// Aggregates Market Data API endpoint groups.
#[derive(Debug, Clone)]
pub struct MarketDataApi {
    client: SchwabClient,
}

impl MarketDataApi {
    pub fn new(client: SchwabClient) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &SchwabClient {
        &self.client
    }

    pub fn quotes(&self) -> QuotesApi<'_> {
        QuotesApi::new(&self.client)
    }

    pub fn price_history(&self) -> PriceHistoryApi<'_> {
        PriceHistoryApi::new(&self.client)
    }

    pub fn instruments(&self) -> InstrumentsApi<'_> {
        InstrumentsApi::new(&self.client)
    }

    pub fn markets(&self) -> MarketsApi<'_> {
        MarketsApi::new(&self.client)
    }

    pub fn chains(&self) -> ChainsApi<'_> {
        ChainsApi::new(&self.client)
    }
}
