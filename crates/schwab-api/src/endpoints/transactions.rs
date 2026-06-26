use crate::client::SchwabClient;
use crate::error::{ApiError, Result};
use crate::models::transaction::Transaction;
use crate::query::{default_transaction_window, resolve_time_range};

pub struct TransactionsApi<'a> {
    client: &'a SchwabClient,
}

impl<'a> TransactionsApi<'a> {
    pub fn new(client: &'a SchwabClient) -> Self {
        Self { client }
    }

    /// GET /accounts/{accountNumber}/transactions
    pub async fn list(
        &self,
        account_number: &str,
        start_date: Option<&str>,
        end_date: Option<&str>,
        types: Option<&str>,
        symbol: Option<&str>,
    ) -> Result<Vec<Transaction>> {
        let (start, end) = resolve_time_range(start_date, end_date, default_transaction_window)
            .map_err(ApiError::Other)?;
        let types = types.unwrap_or("TRADE");
        let path = format!("/accounts/{account_number}/transactions");
        let query = super::merge_queries(vec![
            vec![("startDate", start.as_str()), ("endDate", end.as_str())],
            vec![("types", types)],
            super::opt_query("symbol", symbol),
        ]);
        self.client.get_json(&path, &query).await
    }

    /// GET /accounts/{accountNumber}/transactions/{transactionId}
    pub async fn get(&self, account_number: &str, transaction_id: &str) -> Result<Transaction> {
        let path = format!("/accounts/{account_number}/transactions/{transaction_id}");
        self.client.get_json(&path, &[]).await
    }
}
