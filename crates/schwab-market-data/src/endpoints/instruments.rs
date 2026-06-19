use schwab_api::client::SchwabClient;
use schwab_api::Result;
use serde_json::Value;

use super::{get, merge_queries_str};

pub struct InstrumentsApi<'a> {
    client: &'a SchwabClient,
}

impl<'a> InstrumentsApi<'a> {
    pub fn new(client: &'a SchwabClient) -> Self {
        Self { client }
    }

    /// GET /instruments — search by symbol with projection.
    ///
    /// Projections: `symbol-search`, `symbol-regex`, `desc-search`, `desc-regex`, `search`, `fundamental`
    pub async fn search(&self, symbol: &str, projection: &str) -> Result<Value> {
        let query = merge_queries_str(vec![
            vec![("symbol".into(), symbol.into())],
            vec![("projection".into(), projection.into())],
        ]);
        get(self.client, "/instruments", &query).await
    }

    /// GET /instruments/{cusip_id}
    pub async fn get_by_cusip(&self, cusip: &str) -> Result<Value> {
        let path = format!("/instruments/{cusip}");
        get(self.client, &path, &[]).await
    }
}
