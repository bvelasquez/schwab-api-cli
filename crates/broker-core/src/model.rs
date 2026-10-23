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
    // EDT is UTC−4, so the cash session is 13:30–20:00 UTC. EST is UTC−5, so it is
    // 14:30–21:00 UTC. March second Sunday through November first Sunday is daylight
    // time. This calendar is the Public gap filler; Schwab uses its market-hours endpoint.
    let mins = now.time().hour() as i32 * 60 + now.time().minute() as i32;
    let open = cash_session_bounds(now.date_naive()).is_some_and(|(open_mins, close_mins)| {
        mins >= open_mins && mins < close_mins
    });
    SessionStatus {
        open,
        source: "local-us-equity-calendar".into(),
    }
}

/// Minutes since 9:30 ET. `None` on a weekend or before the cash open.
pub fn minutes_since_cash_open(now: DateTime<Utc>) -> Option<u32> {
    let (open_mins, _) = cash_session_bounds(now.date_naive())?;
    let mins = now.time().hour() as i32 * 60 + now.time().minute() as i32;
    if mins < open_mins {
        return None;
    }
    Some((mins - open_mins) as u32)
}

fn cash_session_bounds(date: NaiveDate) -> Option<(i32, i32)> {
    if matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
        return None;
    }
    let daylight = us_daylight(date);
    let open_mins = if daylight { 13 * 60 + 30 } else { 14 * 60 + 30 };
    let close_mins = if daylight { 20 * 60 } else { 21 * 60 };
    Some((open_mins, close_mins))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn daylight_session_opens_at_1330_utc() {
        let before = at("2026-10-22T13:29:00Z");
        let open = at("2026-10-22T13:30:00Z");
        assert!(!session_open_local(before).open);
        assert_eq!(minutes_since_cash_open(before), None);
        assert!(session_open_local(open).open);
        assert_eq!(minutes_since_cash_open(open), Some(0));
        assert_eq!(minutes_since_cash_open(at("2026-10-22T13:45:00Z")), Some(15));
        assert!(!session_open_local(at("2026-10-22T20:00:00Z")).open);
    }

    #[test]
    fn standard_session_opens_at_1430_utc() {
        let before = at("2026-01-15T14:29:00Z");
        let open = at("2026-01-15T14:30:00Z");
        assert!(!session_open_local(before).open);
        assert!(session_open_local(open).open);
        assert_eq!(minutes_since_cash_open(open), Some(0));
        assert!(!session_open_local(at("2026-01-15T21:00:00Z")).open);
    }

    #[test]
    fn weekend_is_closed() {
        let saturday = at("2026-10-24T15:00:00Z");
        assert!(!session_open_local(saturday).open);
        assert_eq!(minutes_since_cash_open(saturday), None);
    }
}
