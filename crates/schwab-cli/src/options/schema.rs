use serde_json::{json, Value};

use crate::order_schema::{order_examples, order_schema_meta};
use crate::options::types::StrategyKind;

pub fn options_schema() -> Value {
    json!({
        "meta": order_schema_meta(),
        "strategies": {
            "vertical": {
                "description": "Two-leg vertical spread (put/call, credit/debit)",
                "params": {
                    "underlying": "string",
                    "expiry": "YYYY-MM-DD",
                    "type": "put_credit | put_debit | call_credit | call_debit",
                    "short_strike": "number",
                    "long_strike": "number",
                    "contracts": "number",
                    "limit_credit": "number (credit spreads)",
                    "limit_debit": "number (debit spreads)"
                },
                "example": {
                    "underlying": "SPY",
                    "expiry": "2026-07-18",
                    "type": "put_credit",
                    "short_strike": 540,
                    "long_strike": 535,
                    "contracts": 2,
                    "limit_credit": 0.85
                }
            },
            "iron_condor": {
                "description": "Four-leg iron condor (defined risk)",
                "params": {
                    "underlying": "string",
                    "expiry": "YYYY-MM-DD",
                    "put_short": "number",
                    "put_long": "number",
                    "call_short": "number",
                    "call_long": "number",
                    "contracts": "number",
                    "limit_credit": "number"
                },
                "example": {
                    "underlying": "SPY",
                    "expiry": "2026-08-15",
                    "put_short": 520,
                    "put_long": 515,
                    "call_short": 560,
                    "call_long": 565,
                    "contracts": 1,
                    "limit_credit": 1.20
                }
            }
        },
        "symbology": order_examples().get("optionSymbology").cloned().unwrap_or(json!(null)),
        "workflow": [
            "schwab options chain --symbol SPY --json",
            "schwab options validate --strategy vertical --params '<json>' --json",
            "schwab options preview --account-number <hash> --strategy vertical --params '<json>' --json",
            "schwab options open --account-number <hash> --strategy vertical --params '<json>' --trust --yes --json"
        ],
        "allowed_strategies_v1": [
            StrategyKind::Vertical.as_str(),
            StrategyKind::IronCondor.as_str()
        ]
    })
}
