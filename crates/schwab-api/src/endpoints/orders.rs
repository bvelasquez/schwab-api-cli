use crate::client::SchwabClient;
use crate::error::{ApiError, Result};
use crate::models::order::{Order, OrderRequest, PreviewOrder};
use crate::query::{default_order_window, default_orders_all_window, resolve_time_range};

pub struct OrdersApi<'a> {
    client: &'a SchwabClient,
}

impl<'a> OrdersApi<'a> {
    pub fn new(client: &'a SchwabClient) -> Self {
        Self { client }
    }

    /// GET /accounts/{accountNumber}/orders
    pub async fn list_for_account(
        &self,
        account_number: &str,
        from_entered_time: Option<&str>,
        to_entered_time: Option<&str>,
        status: Option<&str>,
        max_results: Option<&str>,
    ) -> Result<Vec<Order>> {
        let (from, to) =
            resolve_time_range(from_entered_time, to_entered_time, default_order_window)
                .map_err(ApiError::Other)?;
        let path = format!("/accounts/{account_number}/orders");
        let query = super::merge_queries(vec![
            vec![
                ("fromEnteredTime", from.as_str()),
                ("toEnteredTime", to.as_str()),
            ],
            super::opt_query("status", status),
            super::opt_query("maxResults", max_results),
        ]);
        self.client.get_json(&path, &query).await
    }

    /// GET /orders — all accounts (from date must be within 60 days).
    pub async fn list_all(
        &self,
        from_entered_time: Option<&str>,
        to_entered_time: Option<&str>,
        status: Option<&str>,
        max_results: Option<&str>,
    ) -> Result<Vec<Order>> {
        let (from, to) = resolve_time_range(
            from_entered_time,
            to_entered_time,
            default_orders_all_window,
        )
        .map_err(ApiError::Other)?;
        let query = super::merge_queries(vec![
            vec![
                ("fromEnteredTime", from.as_str()),
                ("toEnteredTime", to.as_str()),
            ],
            super::opt_query("status", status),
            super::opt_query("maxResults", max_results),
        ]);
        self.client.get_json("/orders", &query).await
    }

    /// GET /accounts/{accountNumber}/orders/{orderId}
    pub async fn get(&self, account_number: &str, order_id: &str) -> Result<Order> {
        let path = format!("/accounts/{account_number}/orders/{order_id}");
        self.client.get_json(&path, &[]).await
    }

    /// POST /accounts/{accountNumber}/orders — 201 empty body + Location header.
    pub async fn place(
        &self,
        account_number: &str,
        order: &OrderRequest,
    ) -> Result<crate::client::MutationResponse> {
        let path = format!("/accounts/{account_number}/orders");
        self.client.post_mutate(&path, order).await
    }

    /// POST /accounts/{accountNumber}/previewOrder
    pub async fn preview(
        &self,
        account_number: &str,
        order: &OrderRequest,
    ) -> Result<PreviewOrder> {
        let path = format!("/accounts/{account_number}/previewOrder");
        self.client.post_json(&path, order).await
    }

    /// DELETE /accounts/{accountNumber}/orders/{orderId}
    pub async fn cancel(
        &self,
        account_number: &str,
        order_id: &str,
    ) -> Result<crate::client::MutationResponse> {
        let path = format!("/accounts/{account_number}/orders/{order_id}");
        self.client.delete_mutate(&path).await
    }

    /// PUT /accounts/{accountNumber}/orders/{orderId}
    pub async fn replace(
        &self,
        account_number: &str,
        order_id: &str,
        order: &OrderRequest,
    ) -> Result<crate::client::MutationResponse> {
        let path = format!("/accounts/{account_number}/orders/{order_id}");
        self.client.put_mutate(&path, order).await
    }
}
