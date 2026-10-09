//! Schwab option-chain helpers shared by entry scan and exit marks.

use anyhow::{Context, Result};
use schwab_market_data::endpoints::chains::ChainQuery;
use schwab_market_data::MarketDataApi;
use serde_json::Value;

/// Which NBBO side is required for a close mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseQuoteSide {
    /// Sell-to-close a long: need bid (receive).
    Bid,
    /// Buy-to-close a short: need ask (pay).
    Ask,
}

impl CloseQuoteSide {
    fn primary_field(self) -> &'static str {
        match self {
            CloseQuoteSide::Bid => "bid",
            CloseQuoteSide::Ask => "ask",
        }
    }

    fn opposite_field(self) -> &'static str {
        match self {
            CloseQuoteSide::Bid => "ask",
            CloseQuoteSide::Ask => "bid",
        }
    }
}

/// One option-leg price used when marking a credit spread to close.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLegQuote {
    pub price: f64,
    /// `bid` / `ask` (native NBBO), `mark`, `last`, `opposite`, or `last_good`.
    pub source: &'static str,
    pub degraded: bool,
}

/// Combined debit-to-close for a credit vertical (or one condor wing).
#[derive(Debug, Clone, PartialEq)]
pub struct CreditCloseQuote {
    pub debit_to_close: f64,
    pub short_ask: Option<f64>,
    pub long_bid: Option<f64>,
    pub quote_degraded: bool,
    /// True when a substitute could understate debit (opposite NBBO or stale last-good).
    /// Mechanical stop still uses `debit_to_close`; profit_target is skipped so we do not
    /// invent a fill that looks like 50% profit.
    pub suppress_profit_target: bool,
    /// True when this debit is live enough to persist as last-good for the next tick.
    pub persist_as_last_good: bool,
    pub fallbacks: Vec<String>,
}

fn strike_key_candidates(strike: f64) -> Vec<String> {
    vec![
        format!("{strike:.1}"),
        format!("{strike:.0}"),
        strike.to_string(),
    ]
}

pub fn contract_at_strike(strike_map: &Value, strike: f64) -> Option<&Value> {
    let obj = strike_map.as_object()?;
    for key in strike_key_candidates(strike) {
        if let Some(contract) = obj
            .get(&key)
            .and_then(|contracts| contracts.as_array()?.first())
        {
            return Some(contract);
        }
    }
    None
}

fn positive_quote_field(contract: &Value, field: &str) -> Option<f64> {
    contract
        .get(field)
        .and_then(|v| v.as_f64())
        .filter(|v| *v > 0.0)
}

/// Resolve a close-mark quote for one strike.
///
/// Order (paper-safe / fail-soft; never uses a 0.00 NBBO as a real price):
/// 1. requested side (`bid` or `ask`) if > 0
/// 2. Schwab `mark` (exchange mid)
/// 3. `last` / `lastPrice`
/// 4. opposite NBBO side
///
/// A zero or null bid is treated as missing — using 0 would inflate debit-to-close and
/// can falsely fire the 2×-credit stop. Opposite-side is last resort and flagged degraded.
pub fn resolve_option_leg_quote(
    strike_map: &Value,
    strike: f64,
    side: CloseQuoteSide,
) -> Option<ResolvedLegQuote> {
    let contract = contract_at_strike(strike_map, strike)?;
    let primary = side.primary_field();
    if let Some(price) = positive_quote_field(contract, primary) {
        return Some(ResolvedLegQuote {
            price,
            source: primary,
            degraded: false,
        });
    }
    if let Some(price) = positive_quote_field(contract, "mark") {
        return Some(ResolvedLegQuote {
            price,
            source: "mark",
            degraded: true,
        });
    }
    if let Some(price) = positive_quote_field(contract, "last")
        .or_else(|| positive_quote_field(contract, "lastPrice"))
    {
        return Some(ResolvedLegQuote {
            price,
            source: "last",
            degraded: true,
        });
    }
    if let Some(price) = positive_quote_field(contract, side.opposite_field()) {
        return Some(ResolvedLegQuote {
            price,
            source: "opposite",
            degraded: true,
        });
    }
    None
}

fn push_fallback(
    fallbacks: &mut Vec<String>,
    strike: f64,
    side: CloseQuoteSide,
    q: &ResolvedLegQuote,
) {
    if q.degraded {
        fallbacks.push(format!(
            "{side:?} {strike}: {src}",
            side = match side {
                CloseQuoteSide::Bid => "bid",
                CloseQuoteSide::Ask => "ask",
            },
            src = q.source
        ));
    }
}

/// `debit_to_close = max(0, short_ask − long_bid)` with per-leg fallbacks.
pub fn credit_spread_debit_to_close(
    strike_map: &Value,
    short_strike: f64,
    long_strike: f64,
    last_good_debit: Option<f64>,
) -> Option<CreditCloseQuote> {
    credit_spread_debit_to_close_with_last_good_legs(
        strike_map,
        short_strike,
        long_strike,
        last_good_debit,
        None,
        None,
    )
}

/// Same as [`credit_spread_debit_to_close`], plus optional last-good NBBO for each wing.
pub fn credit_spread_debit_to_close_with_last_good_legs(
    strike_map: &Value,
    short_strike: f64,
    long_strike: f64,
    last_good_debit: Option<f64>,
    last_good_short_ask: Option<f64>,
    last_good_long_bid: Option<f64>,
) -> Option<CreditCloseQuote> {
    let mut fallbacks = Vec::new();
    let mut used_opposite = false;
    let mut used_last_good_leg = false;

    let short_ask = match resolve_option_leg_quote(strike_map, short_strike, CloseQuoteSide::Ask) {
        Some(q) => {
            used_opposite |= q.source == "opposite";
            push_fallback(&mut fallbacks, short_strike, CloseQuoteSide::Ask, &q);
            Some(q.price)
        }
        None => {
            if let Some(px) = last_good_short_ask.filter(|v| *v > 0.0) {
                used_last_good_leg = true;
                fallbacks.push(format!("ask {short_strike}: last_good"));
                Some(px)
            } else {
                None
            }
        }
    };

    let long_bid = match resolve_option_leg_quote(strike_map, long_strike, CloseQuoteSide::Bid) {
        Some(q) => {
            used_opposite |= q.source == "opposite";
            push_fallback(&mut fallbacks, long_strike, CloseQuoteSide::Bid, &q);
            Some(q.price)
        }
        None => {
            if let Some(px) = last_good_long_bid.filter(|v| *v > 0.0) {
                used_last_good_leg = true;
                fallbacks.push(format!("bid {long_strike}: last_good"));
                Some(px)
            } else {
                None
            }
        }
    };

    if let (Some(short_ask), Some(long_bid)) = (short_ask, long_bid) {
        let used_stale = used_last_good_leg;
        return Some(CreditCloseQuote {
            debit_to_close: (short_ask - long_bid).max(0.0),
            short_ask: Some(short_ask),
            long_bid: Some(long_bid),
            quote_degraded: !fallbacks.is_empty(),
            suppress_profit_target: used_opposite || used_stale,
            persist_as_last_good: !used_opposite && !used_stale,
            fallbacks,
        });
    }

    let last_good = last_good_debit.filter(|v| *v >= 0.0)?;
    fallbacks.push("debit: last_good".into());
    Some(CreditCloseQuote {
        debit_to_close: last_good,
        short_ask,
        long_bid,
        quote_degraded: true,
        suppress_profit_target: true,
        persist_as_last_good: false,
        fallbacks,
    })
}

/// Iron-condor close debit = put wing + call wing, sharing one last-good combined debit.
pub fn iron_condor_debit_to_close(
    put_map: &Value,
    call_map: &Value,
    put_short: f64,
    put_long: f64,
    call_short: f64,
    call_long: f64,
    last_good_debit: Option<f64>,
) -> Option<CreditCloseQuote> {
    let put = credit_spread_debit_to_close(put_map, put_short, put_long, None);
    let call = credit_spread_debit_to_close(call_map, call_short, call_long, None);
    match (put, call) {
        (Some(p), Some(c)) => {
            let mut fallbacks = p.fallbacks;
            fallbacks.extend(c.fallbacks);
            Some(CreditCloseQuote {
                debit_to_close: p.debit_to_close + c.debit_to_close,
                short_ask: None,
                long_bid: None,
                quote_degraded: p.quote_degraded || c.quote_degraded,
                suppress_profit_target: p.suppress_profit_target || c.suppress_profit_target,
                persist_as_last_good: p.persist_as_last_good && c.persist_as_last_good,
                fallbacks,
            })
        }
        _ => {
            let last_good = last_good_debit.filter(|v| *v >= 0.0)?;
            Some(CreditCloseQuote {
                debit_to_close: last_good,
                short_ask: None,
                long_bid: None,
                quote_degraded: true,
                suppress_profit_target: true,
                persist_as_last_good: false,
                fallbacks: vec!["debit: last_good".into()],
            })
        }
    }
}

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

/// Strike count for monitoring an open vertical.
///
/// Schwab's `/chains` response is centered on the underlying. The `strike` query
/// is not an anchor: `strike=<short>&strikeCount=50` still returns ~50 strikes
/// around spot, which drops a short that has rallied 8% OTM. Size the window
/// from spot out to the farther leg ($1 grids, both sides of spot, plus pad).
pub fn monitor_strike_count(spot: f64, far_strike: f64) -> u32 {
    if !(spot > 0.0) {
        return 160;
    }
    let distance = (spot - far_strike).abs();
    let needed = (2.0 * distance + 30.0).ceil() as u32;
    needed.clamp(80, 400)
}

pub fn both_strikes_in_map(strike_map: &Value, short_strike: f64, long_strike: f64) -> bool {
    contract_at_strike(strike_map, short_strike).is_some()
        && contract_at_strike(strike_map, long_strike).is_some()
}

/// ATM implied vol from per-contract `volatility` (percent). Never the chain-level
/// `volatility` field, which Schwab fills with a constant placeholder.
pub fn atm_implied_vol_pct(strike_map: &Value, spot: f64) -> Option<f64> {
    nearest_listed_iv(strike_map, spot).map(|(_, iv)| iv)
}

fn nearest_listed_iv(strike_map: &Value, spot: f64) -> Option<(f64, f64)> {
    if !(spot > 0.0) {
        return None;
    }
    let obj = strike_map.as_object()?;
    let mut best: Option<(f64, f64)> = None;
    for (key, contracts) in obj {
        let Ok(strike) = key.parse::<f64>() else {
            continue;
        };
        let Some(iv) = contracts
            .as_array()
            .and_then(|a| a.first())
            .and_then(|c| c.get("volatility"))
            .and_then(|v| v.as_f64())
            .filter(|v| *v > 0.0)
        else {
            continue;
        };
        let dist = (strike - spot).abs();
        match best {
            Some((best_dist, _)) if dist >= best_dist => {}
            _ => best = Some((dist, iv)),
        }
    }
    best
}

/// Closest listed IV across several expiry maps (put and call wings).
pub fn atm_implied_vol_across(maps: &[&Value], spot: f64) -> Option<f64> {
    maps.iter()
        .filter_map(|m| nearest_listed_iv(m, spot))
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, iv)| iv)
}

pub fn insert_chain_contract(strike_map: &mut Value, strike: f64, contract: Value) {
    let Some(obj) = strike_map.as_object_mut() else {
        return;
    };
    let key = format!("{strike:.1}");
    obj.insert(key, serde_json::json!([contract]));
}

fn quote_number(quote: &Value, fields: &[&str]) -> Option<f64> {
    for field in fields {
        if let Some(n) = quote.get(*field).and_then(|v| v.as_f64()).filter(|v| *v > 0.0) {
            return Some(n);
        }
    }
    None
}

fn quote_entry_for_symbol<'a>(raw: &'a Value, symbol: &str) -> Option<&'a Value> {
    let want = symbol.trim();
    if let Some(entry) = raw.get(want) {
        return Some(entry);
    }
    let obj = raw.as_object().or_else(|| raw.get("data")?.as_object())?;
    for (key, value) in obj {
        if key.trim().eq_ignore_ascii_case(want) {
            return Some(value);
        }
    }
    None
}

/// Map a Schwab option quote (`bidPrice` / `askPrice` / greeks) onto a chain
/// contract (`bid` / `ask` / `delta` / `volatility`) so a missing wing can be
/// spliced into the strike map.
pub fn chain_contract_from_option_quote(raw: &Value, symbol: &str) -> Option<Value> {
    let entry = quote_entry_for_symbol(raw, symbol)?;
    let quote = entry.get("quote").unwrap_or(entry);
    let bid = quote_number(quote, &["bidPrice", "bid"]);
    let ask = quote_number(quote, &["askPrice", "ask"]);
    let mark = quote_number(quote, &["mark"]);
    let last = quote_number(quote, &["lastPrice", "last"]);
    if bid.is_none() && ask.is_none() && mark.is_none() && last.is_none() {
        return None;
    }
    Some(serde_json::json!({
        "bid": bid,
        "ask": ask,
        "mark": mark,
        "last": last,
        "delta": quote.get("delta").and_then(|v| v.as_f64()),
        "theta": quote.get("theta").and_then(|v| v.as_f64()),
        "volatility": quote_number(quote, &["volatility"]),
        "inTheMoney": quote.get("inTheMoney").and_then(|v| v.as_bool()),
    }))
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

    /// QQQ 689/684 with spot 746: a 50- or 100-strike window around spot misses both legs.
    #[test]
    fn monitor_window_reaches_a_short_eight_percent_otm() {
        let count = monitor_strike_count(746.0, 684.0);
        assert!(count > 100, "count {count} still misses a 62-point wing");
        assert!(count <= 400);
        let around_spot = serde_json::json!({
            "722.0": [{ "bid": 1.0, "ask": 1.1 }],
            "771.0": [{ "bid": 20.0, "ask": 20.2 }]
        });
        assert!(!both_strikes_in_map(&around_spot, 689.0, 684.0));
    }

    #[test]
    fn atm_iv_uses_nearest_contract_not_chain_placeholder() {
        let map = serde_json::json!({
            "740.0": [{ "volatility": 18.2 }],
            "700.0": [{ "volatility": 22.0 }]
        });
        let iv = atm_implied_vol_pct(&map, 746.0).unwrap();
        assert!((iv - 18.2).abs() < 1e-9);
    }

    #[test]
    fn option_quote_splices_into_chain_contract() {
        let raw = serde_json::json!({
            "QQQ   261030P00689000": {
                "quote": {
                    "bidPrice": 1.99,
                    "askPrice": 2.02,
                    "mark": 2.005,
                    "lastPrice": 2.0,
                    "delta": -0.091,
                    "theta": -0.12,
                    "volatility": 25.1
                }
            }
        });
        let contract = chain_contract_from_option_quote(&raw, "QQQ   261030P00689000").unwrap();
        assert!((contract["bid"].as_f64().unwrap() - 1.99).abs() < 1e-9);
        assert!((contract["ask"].as_f64().unwrap() - 2.02).abs() < 1e-9);
        assert!((contract["volatility"].as_f64().unwrap() - 25.1).abs() < 1e-9);
        let mut map = serde_json::json!({});
        insert_chain_contract(&mut map, 689.0, contract);
        assert!(both_strikes_in_map(
            &{
                let mut with_long = map.clone();
                insert_chain_contract(
                    &mut with_long,
                    684.0,
                    serde_json::json!({"bid": 1.72, "ask": 1.74, "volatility": 25.8}),
                );
                with_long
            },
            689.0,
            684.0,
        ));
    }

    /// QQQ put-credit 670/665: long 665 bid blank (CPI thin quote) — still mark the spread.
    #[test]
    fn put_credit_missing_long_bid_uses_mark() {
        let map = serde_json::json!({
            "670.0": [{ "bid": 1.20, "ask": 1.24, "mark": 1.22, "last": 1.21 }],
            "665.0": [{ "bid": null, "ask": 0.42, "mark": 0.38, "last": 0.37, "delta": -0.08 }]
        });
        let q = credit_spread_debit_to_close(&map, 670.0, 665.0, None).expect("mark");
        assert!((q.debit_to_close - (1.24 - 0.38)).abs() < 1e-9);
        assert!(q.quote_degraded);
        assert!(!q.suppress_profit_target);
        assert!(q.persist_as_last_good);
        assert!(q
            .fallbacks
            .iter()
            .any(|s| s.contains("665") && s.contains("mark")));
    }

    #[test]
    fn put_credit_zero_long_bid_does_not_inflate_debit() {
        let map = serde_json::json!({
            "670.0": [{ "bid": 1.20, "ask": 1.24, "mark": 1.22 }],
            "665.0": [{ "bid": 0.0, "ask": 0.42, "mark": 0.38 }]
        });
        let q = credit_spread_debit_to_close(&map, 670.0, 665.0, None).unwrap();
        assert!(
            q.debit_to_close < 1.24,
            "zero bid must not be treated as 0 receive"
        );
        assert!((q.debit_to_close - (1.24 - 0.38)).abs() < 1e-9);
    }

    #[test]
    fn put_credit_native_nbbo_not_degraded() {
        let map = serde_json::json!({
            "670.0": [{ "bid": 1.20, "ask": 1.24 }],
            "665.0": [{ "bid": 0.35, "ask": 0.39 }]
        });
        let q = credit_spread_debit_to_close(&map, 670.0, 665.0, None).unwrap();
        assert!((q.debit_to_close - 0.89).abs() < 1e-9);
        assert!(!q.quote_degraded);
        assert!(!q.suppress_profit_target);
    }

    #[test]
    fn opposite_side_long_bid_suppresses_profit_target() {
        let map = serde_json::json!({
            "670.0": [{ "bid": 1.20, "ask": 1.24 }],
            "665.0": [{ "ask": 0.42 }]
        });
        let q = credit_spread_debit_to_close(&map, 670.0, 665.0, None).unwrap();
        assert!((q.debit_to_close - (1.24 - 0.42)).abs() < 1e-9);
        assert!(q.quote_degraded);
        assert!(q.suppress_profit_target);
        assert!(!q.persist_as_last_good);
    }

    #[test]
    fn last_good_debit_when_long_strike_absent() {
        let map = serde_json::json!({
            "670.0": [{ "bid": 1.20, "ask": 1.24 }]
        });
        let q = credit_spread_debit_to_close(&map, 670.0, 665.0, Some(0.90)).unwrap();
        assert!((q.debit_to_close - 0.90).abs() < 1e-9);
        assert!(q.quote_degraded);
        assert!(q.suppress_profit_target);
        assert!(!q.persist_as_last_good);
    }

    #[test]
    fn missing_quotes_without_last_good_returns_none() {
        let map = serde_json::json!({
            "670.0": [{ "delta": -0.12 }]
        });
        assert!(credit_spread_debit_to_close(&map, 670.0, 665.0, None).is_none());
    }
}
