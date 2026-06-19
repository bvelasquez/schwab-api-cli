use crate::client::SchwabClient;

pub mod accounts;
pub mod orders;
pub mod transactions;
pub mod user;

pub use accounts::AccountsApi;
pub use orders::OrdersApi;
pub use transactions::TransactionsApi;
pub use user::UserApi;

/// Aggregates all Trader API endpoint groups.
#[derive(Debug, Clone)]
pub struct TraderApi {
    client: SchwabClient,
}

impl TraderApi {
    pub fn new(client: SchwabClient) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &SchwabClient {
        &self.client
    }

    pub fn accounts(&self) -> AccountsApi<'_> {
        AccountsApi::new(&self.client)
    }

    pub fn orders(&self) -> OrdersApi<'_> {
        OrdersApi::new(&self.client)
    }

    pub fn transactions(&self) -> TransactionsApi<'_> {
        TransactionsApi::new(&self.client)
    }

    pub fn user(&self) -> UserApi<'_> {
        UserApi::new(&self.client)
    }
}

/// Helper for optional query parameters.
pub(crate) fn opt_query<'a>(key: &'a str, value: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    value.map(|v| vec![(key, v)]).unwrap_or_default()
}

pub(crate) fn merge_queries<'a>(parts: Vec<Vec<(&'a str, &'a str)>>) -> Vec<(&'a str, &'a str)> {
    parts.into_iter().flatten().collect()
}
