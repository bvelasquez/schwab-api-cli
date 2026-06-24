use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::options::days_to_expiry;

/// Live chain fields attached to entry signals and LLM context.
pub fn vertical_entry_market_context(
    chain: &Value,
    underlying: &str,
    expiry: NaiveDate,
    today: NaiveDate,
    put_map: &Value,
    short_strike: f64,
    long_strike: f64,
    width: f64,
    credit: f64,
    contracts: f64,
) -> Value {
    let underlying_quote = chain.get("underlying").cloned().unwrap_or(json!({}));
    let underlying_price = chain
        .pointer("/underlying/last")
        .or_else(|| chain.pointer("/underlying/mark"))
        .or_else(|| chain.pointer("/underlyingPrice"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let dte = days_to_expiry(expiry, today);
    let short_delta = strike_field(put_map, short_strike, "delta");
    let long_delta = strike_field(put_map, long_strike, "delta");
    let short_iv = strike_field(put_map, short_strike, "volatility");
    let chain_iv = chain.get("volatility").and_then(|v| v.as_f64());

    let max_loss_per_spread = ((width - credit).max(0.0)) * 100.0;
    let credit_to_width_pct = if width > f64::EPSILON {
        (credit / width) * 100.0
    } else {
        0.0
    };
    let short_otm_pct = if underlying_price > f64::EPSILON {
        ((underlying_price - short_strike) / underlying_price) * 100.0
    } else {
        0.0
    };

    json!({
        "data_source": "schwab_option_chain",
        "underlying": underlying,
        "underlying_price": underlying_price,
        "underlying_bid": underlying_quote.pointer("/bid").and_then(|v| v.as_f64()),
        "underlying_ask": underlying_quote.pointer("/ask").and_then(|v| v.as_f64()),
        "underlying_change_pct": underlying_quote.pointer("/percentChange").and_then(|v| v.as_f64()),
        "chain_iv": chain_iv,
        "ivr_available": false,
        "ivr_note": "Schwab chain provides current IV (chain_iv), not IV Rank",
        "expiry": expiry.to_string(),
        "dte": dte,
        "short_strike": short_strike,
        "long_strike": long_strike,
        "width": width,
        "estimated_credit": credit,
        "credit_to_width_pct": credit_to_width_pct,
        "short_delta": short_delta,
        "long_delta": long_delta,
        "short_strike_iv": short_iv,
        "short_otm_pct": short_otm_pct,
        "short_in_the_money": strike_bool(put_map, short_strike, "inTheMoney"),
        "max_loss_per_spread_usd": max_loss_per_spread,
        "max_loss_total_usd": max_loss_per_spread * contracts,
        "contracts": contracts,
    })
}

pub fn market_context_summary_for_llm() -> Value {
    json!({
        "data_source": "schwab_option_chain",
        "ivr_available": false,
        "note": "Each candidate_entries[] item includes market_context with live price, IV, delta, and DTE from Schwab. Do not defer for missing market data when market_context is present."
    })
}

fn strike_field(strike_map: &Value, strike: f64, field: &str) -> Option<f64> {
    let obj = strike_map.as_object()?;
    for key in strike_key_candidates(strike) {
        if let Some(contracts) = obj.get(&key) {
            if let Some(v) = contracts
                .as_array()?
                .first()?
                .get(field)?
                .as_f64()
            {
                return Some(v);
            }
        }
    }
    None
}

fn strike_bool(strike_map: &Value, strike: f64, field: &str) -> Option<bool> {
    let obj = strike_map.as_object()?;
    for key in strike_key_candidates(strike) {
        if let Some(contracts) = obj.get(&key) {
            if let Some(v) = contracts.as_array()?.first()?.get(field)?.as_bool() {
                return Some(v);
            }
        }
    }
    None
}

fn strike_key_candidates(strike: f64) -> Vec<String> {
    vec![
        format!("{strike:.1}"),
        format!("{strike:.0}"),
        strike.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_vertical_market_context() {
        let chain = json!({
            "underlying": { "last": 298.0, "bid": 297.9, "ask": 298.1, "percentChange": 0.5 },
            "underlyingPrice": 298.0,
            "volatility": 29.0,
            "putExpDateMap": {}
        });
        let put_map = json!({
            "283.0": [{ "delta": -0.22, "volatility": 30.1, "inTheMoney": false }],
            "281.0": [{ "delta": -0.15, "volatility": 29.5, "inTheMoney": false }]
        });
        let ctx = vertical_entry_market_context(
            &chain,
            "IWM",
            NaiveDate::from_ymd_opt(2026, 7, 24).unwrap(),
            NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
            &put_map,
            283.0,
            281.0,
            2.0,
            0.30,
            1.0,
        );
        assert_eq!(ctx["underlying_price"], 298.0);
        assert_eq!(ctx["chain_iv"], 29.0);
        assert_eq!(ctx["short_delta"], -0.22);
        assert!((ctx["credit_to_width_pct"].as_f64().unwrap() - 15.0).abs() < 0.01);
        assert_eq!(ctx["ivr_available"], false);
    }
}
