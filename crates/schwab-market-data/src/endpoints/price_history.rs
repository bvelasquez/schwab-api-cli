use schwab_api::client::SchwabClient;
use schwab_api::Result;
use serde_json::Value;

use super::{get, merge_queries_str, opt_query, opt_query_bool, opt_query_i64, opt_query_u32};

pub struct PriceHistoryApi<'a> {
    client: &'a SchwabClient,
}

impl<'a> PriceHistoryApi<'a> {
    pub fn new(client: &'a SchwabClient) -> Self {
        Self { client }
    }

    /// GET /pricehistory
    #[allow(clippy::too_many_arguments)]
    pub async fn get(
        &self,
        symbol: &str,
        period_type: Option<&str>,
        period: Option<u32>,
        frequency_type: Option<&str>,
        frequency: Option<u32>,
        start_date: Option<i64>,
        end_date: Option<i64>,
        need_extended_hours_data: Option<bool>,
        need_previous_close: Option<bool>,
    ) -> Result<Value> {
        let query = merge_queries_str(vec![
            vec![("symbol".into(), symbol.into())],
            opt_query("periodType", period_type),
            opt_query_u32("period", period),
            opt_query("frequencyType", frequency_type),
            opt_query_u32("frequency", frequency),
            opt_query_i64("startDate", start_date),
            opt_query_i64("endDate", end_date),
            opt_query_bool("needExtendedHoursData", need_extended_hours_data),
            opt_query_bool("needPreviousClose", need_previous_close),
        ]);
        get(self.client, "/pricehistory", &query).await
    }
}
