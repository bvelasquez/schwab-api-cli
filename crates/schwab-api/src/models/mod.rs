use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod account;
pub mod order;
pub mod transaction;
pub mod user;

/// Generic JSON value wrapper for endpoints whose schema evolves on Schwab's side.
pub type JsonValue = Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountNumberHash {
    pub account_number: String,
    pub hash_value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceError {
    pub message: String,
    pub errors: Option<Vec<Value>>,
}
