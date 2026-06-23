use schwab_api::client::SchwabClient;
use schwab_api::Result;
use serde_json::Value;

use super::{get, merge_queries_str, opt_query, opt_query_u32};

pub struct ChainsApi<'a> {
    client: &'a SchwabClient,
}

/// Query parameters for GET /chains (option chain).
#[derive(Debug, Clone, Default)]
pub struct ChainQuery<'a> {
    pub symbol: &'a str,
    pub contract_type: Option<&'a str>,
    pub strike_count: Option<u32>,
    pub include_underlying_quote: Option<bool>,
    pub strategy: Option<&'a str>,
    pub interval: Option<&'a str>,
    pub strike: Option<&'a str>,
    pub range: Option<&'a str>,
    pub from_date: Option<&'a str>,
    pub to_date: Option<&'a str>,
    pub exp_month: Option<&'a str>,
    pub option_type: Option<&'a str>,
}

impl<'a> ChainsApi<'a> {
    pub fn new(client: &'a SchwabClient) -> Self {
        Self { client }
    }

    /// GET /chains — option chain for an underlying symbol.
    pub async fn get(&self, query: &ChainQuery<'_>) -> Result<Value> {
        let q = merge_queries_str(vec![
            vec![("symbol".into(), query.symbol.to_uppercase())],
            opt_query("contractType", query.contract_type),
            opt_query_u32("strikeCount", query.strike_count),
            super::opt_query_bool(
                "includeUnderlyingQuote",
                query.include_underlying_quote,
            ),
            opt_query("strategy", query.strategy),
            opt_query("interval", query.interval),
            opt_query("strike", query.strike),
            opt_query("range", query.range),
            opt_query("fromDate", query.from_date),
            opt_query("toDate", query.to_date),
            opt_query("expMonth", query.exp_month),
            opt_query("optionType", query.option_type),
        ]);
        get(self.client, "/chains", &q).await
    }
}
