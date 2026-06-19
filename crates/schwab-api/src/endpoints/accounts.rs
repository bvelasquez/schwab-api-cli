use crate::client::SchwabClient;
use crate::error::Result;
use crate::models::account::Account;
use crate::models::AccountNumberHash;

pub struct AccountsApi<'a> {
    client: &'a SchwabClient,
}

impl<'a> AccountsApi<'a> {
    pub fn new(client: &'a SchwabClient) -> Self {
        Self { client }
    }

    /// GET /accounts/accountNumbers
    pub async fn account_numbers(&self) -> Result<Vec<AccountNumberHash>> {
        self.client.get_json("/accounts/accountNumbers", &[]).await
    }

    /// GET /accounts
    pub async fn list(&self, fields: Option<&str>) -> Result<Vec<Account>> {
        let query = super::opt_query("fields", fields);
        self.client.get_json("/accounts", &query).await
    }

    /// GET /accounts/{accountNumber}
    pub async fn get(&self, account_number: &str, fields: Option<&str>) -> Result<Account> {
        let path = format!("/accounts/{account_number}");
        let query = super::opt_query("fields", fields);
        self.client.get_json(&path, &query).await
    }
}
