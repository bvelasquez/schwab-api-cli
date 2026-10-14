use serde::{Deserialize, Serialize};

/// What an adapter can do. Engines branch on these flags, not on the broker name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub broker: String,
    pub native_bracket: bool,
    pub strategy_quote: bool,
    pub paper: bool,
    pub market_hours_api: bool,
    pub fractional: bool,
    pub max_legs: u32,
    pub rate_limit_rps: u32,
    pub order_history_days: u32,
}

impl Capabilities {
    pub fn schwab() -> Self {
        Self {
            broker: "schwab".into(),
            native_bracket: false,
            strategy_quote: false,
            paper: false,
            market_hours_api: true,
            fractional: false,
            max_legs: 4,
            rate_limit_rps: 2,
            order_history_days: 60,
        }
    }

    pub fn public() -> Self {
        Self {
            broker: "public".into(),
            native_bracket: true,
            strategy_quote: true,
            paper: false,
            market_hours_api: false,
            fractional: true,
            max_legs: 6,
            rate_limit_rps: 10,
            order_history_days: 30,
        }
    }
}
