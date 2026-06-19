use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Order = Value;
pub type OrderRequest = Value;
pub type PreviewOrder = Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderInstruction {
    Buy,
    Sell,
    BuyToOpen,
    SellToClose,
    SellShort,
    BuyToCover,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderType {
    Market,
    Limit,
    Stop,
    StopLimit,
    TrailingStop,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderDuration {
    Day,
    GoodTillCancel,
    FillOrKill,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OrderSession {
    Normal,
    Am,
    Pm,
    Seamless,
}
