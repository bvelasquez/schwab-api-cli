use schwab_api::client::SchwabClient;
use schwab_api::Result;
use serde_json::Value;

use super::{get, merge_queries_str, opt_query};

pub struct MarketsApi<'a> {
    client: &'a SchwabClient,
}

impl<'a> MarketsApi<'a> {
    pub fn new(client: &'a SchwabClient) -> Self {
        Self { client }
    }

    /// GET /markets — markets is comma-separated: equity, option, bond, future, forex
    pub async fn hours(&self, markets: &str, date: Option<&str>) -> Result<Value> {
        let query = merge_queries_str(vec![
            vec![("markets".into(), markets.into())],
            opt_query("date", date),
        ]);
        get(self.client, "/markets", &query).await
    }

    /// GET /markets/{market_id}
    pub async fn hours_for_market(&self, market_id: &str, date: Option<&str>) -> Result<Value> {
        let path = format!("/markets/{market_id}");
        let query = merge_queries_str(vec![opt_query("date", date)]);
        get(self.client, &path, &query).await
    }
}
