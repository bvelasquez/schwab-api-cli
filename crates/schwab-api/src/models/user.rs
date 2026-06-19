use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type UserPreference = Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamerInfo {
    pub streamer_socket_url: Option<String>,
    pub schwab_client_customer_id: Option<String>,
    pub schwab_client_correl_id: Option<String>,
    pub schwab_client_channel: Option<String>,
    pub schwab_client_function_id: Option<String>,
}
