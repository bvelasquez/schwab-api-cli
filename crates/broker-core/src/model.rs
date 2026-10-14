use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc, Weekday};
use serde::{Deserialize, Serialize};

use crate::account::{AccountKind, AccountRef};
use crate::intent::{AssetKind, Side};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Quote {
    pub symbol: String,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub last: Option<f64>,
    pub bid_size: Option<f64>,
    pub ask_size: Option<f64>,
    pub volume: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Greeks {
    pub delta: Option<f64>,
    pub gamma: Option<f64>,
    pub theta: Option<f64>,
    pub vega: Option<f64>,
    pub iv: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OptionContract {
    pub symbol: String,
    pub expiration: String,
    pub strike: f64,
    pub put_call: String,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub open_interest: Option<f64>,
    #[serde(default)]
    pub greeks: Greeks,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OptionChain {
    pub underlying: String,
    pub expirations: Vec<String>,
    pub contracts: Vec<OptionContract>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bar {
    pub timestamp: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Position {
    pub symbol: String,
    pub asset: AssetKind,
    pub quantity: f64,
    pub average_price: Option<f64>,
    pub market_value: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OptionStrategyGroup {
    pub id: String,
    pub name: String,
    pub quantity: f64,
    pub legs: Vec<StrategyLeg>,
    pub market_value: Option<f64>,
    pub unit_cost: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StrategyLeg {
    pub symbol: String,
    pub quantity: f64,
    pub side: Side,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Balances {
    pub cash_buying_power: f64,
    pub options_buying_power: Option<f64>,
    pub equity: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Portfolio {
    pub account: AccountRef,
    pub kind: AccountKind,
    pub balances: Balances,
    pub positions: Vec<Position>,
    pub strategies: Vec<OptionStrategyGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Accepted,
    Working,
    Partial,
    Filled,
    Canceled,
    Rejected,
    Unknown,
}

impl OrderStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Filled | Self::Canceled | Self::Rejected
        )
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_uppercase().as_str() {
            "ACCEPTED" | "NEW" | "QUEUED" | "PENDING" | "PENDING_ACTIVATION" => Self::Working,
            "WORKING" | "OPEN" | "LIVE" | "AWAITING_PARENT_ORDER" => Self::Working,
            "PARTIALLY_FILLED" | "PARTIAL" => Self::Partial,
            "FILLED" => Self::Filled,
            "CANCELED" | "CANCELLED" | "EXPIRED" => Self::Canceled,
            "REJECTED" | "FAILED" => Self::Rejected,
            other if other.contains("FILL") && other.contains("PARTIAL") => Self::Partial,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Order {
    pub id: String,
    pub status: OrderStatus,
    pub filled_quantity: f64,
    pub average_price: Option<f64>,
    pub raw_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Preflight {
    pub accepted: bool,
    pub estimated_cost: Option<f64>,
    pub estimated_proceeds: Option<f64>,
    pub buying_power_requirement: Option<f64>,
    pub fees: Option<f64>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlaceResult {
    pub order_id: String,
    pub status: OrderStatus,
    pub message: String,
}

#[derive(Debug, Clone, Copy)]
pub struct WaitOptions {
    pub until: crate::WaitUntil,
    pub timeout_seconds: u64,
    pub poll_interval_seconds: u64,
    pub proceed_on_partial: bool,
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            until: crate::WaitUntil::Filled,
            timeout_seconds: 3600,
            poll_interval_seconds: 5,
            proceed_on_partial: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionStatus {
    pub open: bool,
    pub source: String,
}

pub fn session_open_local(now: DateTime<Utc>) -> SessionStatus {
    // Weekday 9:30–16:00 America/New_York, without a broker hours endpoint.
    // Fixed offset EST (UTC-5) is wrong in summer; use EDT/EST via chrono offset approximation:
    // US equity regular hours are 14:30–21:00 UTC during daylight time and 15:30–22:00 UTC otherwise.
    // March second Sunday through November first Sunday is daylight time. This calendar is the
    // Public gap filler; Schwab uses its market-hours endpoint instead.
    let date = now.date_naive();
    let weekday = date.format("%u").to_string();
    let weekend = weekday == "6" || weekday == "7";
    let daylight = us_daylight(date);
    let open_mins = if daylight { 14 * 60 + 30 } else { 15 * 60 + 30 };
    let close_mins = if daylight { 21 * 60 } else { 22 * 60 };
    let mins = now.time().hour() * 60 + now.time().minute();
    SessionStatus {
        open: !weekend && mins >= open_mins && mins < close_mins,
        source: "local-us-equity-calendar".into(),
    }
}

fn us_daylight(date: chrono::NaiveDate) -> bool {
    use chrono::Datelike;
    let year = date.year();
    let start = nth_sunday(year, 3, 2);
    let end = nth_sunday(year, 11, 1);
    date >= start && date < end
}

fn nth_sunday(year: i32, month: u32, n: u32) -> NaiveDate {
    let mut date = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
    let mut seen = 0;
    loop {
        if date.weekday() == Weekday::Sun {
            seen += 1;
            if seen == n {
                return date;
            }
        }
        date = date.succ_opt().expect("date");
    }
}
