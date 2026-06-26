use schwab_api::client::SchwabClient;
use schwab_api::Result;
use serde_json::Value;

use super::{get, merge_queries_str, opt_query, opt_query_bool};

pub struct QuotesApi<'a> {
    client: &'a SchwabClient,
}

impl<'a> QuotesApi<'a> {
    pub fn new(client: &'a SchwabClient) -> Self {
        Self { client }
    }

    /// GET /quotes — comma-separated symbols.
    pub async fn get_quotes(
        &self,
        symbols: &str,
        fields: Option<&str>,
        indicative: Option<bool>,
    ) -> Result<Value> {
        let query = merge_queries_str(vec![
            vec![("symbols".into(), symbols.into())],
            opt_query("fields", fields),
            opt_query_bool("indicative", indicative),
        ]);
        get(self.client, "/quotes", &query).await
    }

    /// GET /{symbol_id}/quotes — single symbol.
    pub async fn get_quote(
        &self,
        symbol: &str,
        fields: Option<&str>,
        indicative: Option<bool>,
    ) -> Result<Value> {
        let path = format!("/{}/quotes", urlencoding::encode(symbol));
        let query = merge_queries_str(vec![
            opt_query("fields", fields),
            opt_query_bool("indicative", indicative),
        ]);
        get(self.client, &path, &query).await
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn quote_path_encodes_symbol() {
        let path = format!("/{}/quotes", urlencoding::encode("BRK/B"));
        assert!(path.contains("BRK"));
    }
}
