use crate::client::SchwabClient;
use crate::error::Result;
use crate::models::user::UserPreference;

pub struct UserApi<'a> {
    client: &'a SchwabClient,
}

impl<'a> UserApi<'a> {
    pub fn new(client: &'a SchwabClient) -> Self {
        Self { client }
    }

    /// GET /userPreference
    pub async fn preference(&self) -> Result<UserPreference> {
        self.client.get_json("/userPreference", &[]).await
    }
}
