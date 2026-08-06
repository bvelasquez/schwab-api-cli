//! Schwab option-chain helpers shared by entry scan and exit marks.

use anyhow::{Context, Result};
use schwab_market_data::endpoints::chains::ChainQuery;
use schwab_market_data::MarketDataApi;
use serde_json::Value;

/// Format strike for Schwab `strike` query param (whole strikes use one decimal).
pub fn format_chain_strike(strike: f64) -> String {
    if (strike.fract() * 10.0).round() as i64 % 10 == 0 {
        format!("{strike:.1}")
    } else {
        format!("{strike:.2}")
    }
}

pub fn find_expiry_strikes(chain: &Value, map_key: &str, expiry: &str) -> Result<Value> {
    let map = chain
        .get(map_key)
        .context("chain missing exp date map")?
        .as_object()
        .context("exp date map not an object")?;

    for (key, strikes) in map {
        let date_part = key.split(':').next().unwrap_or(key);
        if date_part == expiry || key.starts_with(expiry) {
            return Ok(strikes.clone());
        }
    }
    anyhow::bail!("expiry {expiry} not in chain")
}

/// Chain for vertical entry: OTM wing around spot so shorts are not pinned to the map edge.
/// Schwab centers chains on the underlying; size the window so ~12Δ shorts and wings are included.
pub fn vertical_entry_strike_count(underlying_price: f64, max_width: f64) -> u32 {
    let depth_usd = underlying_price * 0.10 + max_width + 15.0;
    (depth_usd.ceil() as u32).clamp(130, 150)
}

pub async fn fetch_vertical_entry_chain(
    market: &MarketDataApi,
    underlying: &str,
    contract_type: &str,
    underlying_price: f64,
    max_width: f64,
) -> Result<Value> {
    let sized = vertical_entry_strike_count(underlying_price, max_width);
    let anchor = format_chain_strike(underlying_price);
    let mut last_err: Option<anyhow::Error> = None;

    for (range, strike_count, use_anchor) in [
        (None, sized, false),
        (None, sized.saturating_add(10).min(150), false),
        (None, 100u32, true),
        (Some("NTM"), 80u32, true),
    ] {
        let strike = if use_anchor {
            Some(anchor.as_str())
        } else {
            None
        };
        match market
            .chains()
            .get(&ChainQuery {
                symbol: underlying,
                contract_type: Some(contract_type),
                range,
                strike,
                strike_count: Some(strike_count),
                include_underlying_quote: Some(true),
                ..Default::default()
            })
            .await
        {
            Ok(chain) => return Ok(chain),
            Err(e) => last_err = Some(e.into()),
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("vertical entry chain fetch failed")))
}

/// Strike-centered chain for one expiry (smaller payload; used for long-wing quotes).
pub async fn fetch_chain_for_expiry(
    market: &MarketDataApi,
    underlying: &str,
    contract_type: &str,
    anchor_price: f64,
    expiry: &str,
    strike_count: u32,
) -> Result<Value> {
    let anchor = format_chain_strike(anchor_price);
    market
        .chains()
        .get(&ChainQuery {
            symbol: underlying,
            contract_type: Some(contract_type),
            strike: Some(&anchor),
            strike_count: Some(strike_count),
            from_date: Some(expiry),
            to_date: Some(expiry),
            include_underlying_quote: Some(true),
            ..Default::default()
        })
        .await
        .map_err(Into::into)
}

/// Merge missing strike keys from `fallback` into `primary` (for short-leg quotes after wing refetch).
pub fn merge_strike_maps(primary: &mut Value, fallback: &Value) {
    let Some(into) = primary.as_object_mut() else {
        return;
    };
    let Some(from) = fallback.as_object() else {
        return;
    };
    for (k, v) in from {
        into.entry(k.clone()).or_insert_with(|| v.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_chain_strike_whole_and_half() {
        assert_eq!(format_chain_strike(282.0), "282.0");
        assert_eq!(format_chain_strike(282.5), "282.50");
    }

    #[test]
    fn entry_strike_count_covers_deep_otm_on_high_priced_etfs() {
        assert!(vertical_entry_strike_count(715.0, 5.0) >= 130);
        assert!(vertical_entry_strike_count(400.0, 5.0) >= 130);
    }
}
