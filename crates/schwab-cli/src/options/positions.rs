use anyhow::Result;
use schwab_api::TraderApi;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::options::symbology::{parse_option_symbol, ParsedOptionSymbol};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionPositionLeg {
    pub symbol: String,
    pub underlying: String,
    pub quantity: f64,
    pub market_value: f64,
    pub average_price: Option<f64>,
    pub parsed: Option<ParsedOptionSymbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionPositionGroup {
    pub id: String,
    pub underlying: String,
    pub expiry: String,
    pub strategy_hint: String,
    pub legs: Vec<OptionPositionLeg>,
    pub net_market_value: f64,
}

pub fn legacy_position_id(underlying: &str, expiry: &str) -> String {
    format!("{underlying}|{expiry}")
}

pub fn position_group_id(account_hash: &str, group: &OptionPositionGroup) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        account_hash,
        group.underlying,
        group.expiry,
        group.strategy_hint,
        group_leg_signature(group)
    )
}

pub fn candidate_position_id(
    account_hash: &str,
    underlying: &str,
    expiry: &str,
    strategy: &str,
    legs: Vec<(char, f64, &str)>,
) -> String {
    let mut parts: Vec<String> = legs
        .into_iter()
        .map(|(put_call, strike, side)| {
            format!(
                "{}{}{}",
                put_call.to_ascii_uppercase(),
                format_strike(strike),
                side.to_ascii_uppercase()
            )
        })
        .collect();
    parts.sort();
    format!(
        "{}|{}|{}|{}|{}",
        account_hash,
        underlying.to_uppercase(),
        expiry,
        strategy,
        parts.join("_")
    )
}

pub async fn list_option_positions(
    api: &TraderApi,
    account_hash: Option<&str>,
) -> Result<Vec<OptionPositionLeg>> {
    let accounts = if let Some(hash) = account_hash {
        vec![api.accounts().get(hash, Some("positions")).await?]
    } else {
        api.accounts().list(Some("positions")).await?
    };

    let mut legs = Vec::new();
    for account in accounts {
        let positions = account
            .securities_account
            .as_ref()
            .and_then(|sa| sa.positions.as_ref());

        let Some(positions) = positions else {
            continue;
        };

        for pos in positions {
            let instrument = match &pos.instrument {
                Some(i) => i,
                None => continue,
            };
            let asset_type = instrument
                .r#type
                .as_deref()
                .unwrap_or("")
                .to_ascii_uppercase();
            let symbol = instrument.symbol.as_deref().unwrap_or("").to_string();
            if asset_type != "OPTION" && !looks_like_option_symbol(&symbol) {
                continue;
            }

            let long_qty = pos.long_quantity.unwrap_or(0.0);
            let short_qty = pos.short_quantity.unwrap_or(0.0);
            let net_qty = long_qty - short_qty;
            if net_qty.abs() < f64::EPSILON {
                continue;
            }

            let parsed = parse_option_symbol(&symbol).ok();
            let underlying = parsed
                .as_ref()
                .map(|p| p.underlying.clone())
                .unwrap_or_else(|| symbol.split_whitespace().next().unwrap_or("").to_string());

            legs.push(OptionPositionLeg {
                symbol,
                underlying,
                quantity: net_qty,
                market_value: pos.market_value.unwrap_or(0.0),
                average_price: pos.average_price,
                parsed,
            });
        }
    }

    Ok(legs)
}

/// Number of spreads in a grouped position (max abs leg quantity).
pub fn spread_contract_count(group: &OptionPositionGroup) -> u32 {
    group
        .legs
        .iter()
        .map(|l| l.quantity.abs())
        .fold(0.0_f64, f64::max)
        .round()
        .max(1.0) as u32
}

pub fn group_option_legs(legs: &[OptionPositionLeg]) -> Vec<OptionPositionGroup> {
    use std::collections::HashMap;

    let mut by_key: HashMap<String, Vec<&OptionPositionLeg>> = HashMap::new();
    for leg in legs {
        let expiry = leg
            .parsed
            .as_ref()
            .map(|p| p.expiry.to_string())
            .unwrap_or_else(|| "unknown".into());
        let key = legacy_position_id(&leg.underlying, &expiry);
        by_key.entry(key).or_default().push(leg);
    }

    by_key
        .into_iter()
        .map(|(key, group_legs)| {
            let net_mv: f64 = group_legs.iter().map(|l| l.market_value).sum();
            let parts: Vec<&str> = key.split('|').collect();
            let underlying = parts.first().copied().unwrap_or("").to_string();
            let expiry = parts.get(1).copied().unwrap_or("").to_string();
            let strategy_hint = infer_strategy_hint(&group_legs);
            OptionPositionGroup {
                id: key.clone(),
                underlying,
                expiry,
                strategy_hint,
                legs: group_legs.into_iter().cloned().collect(),
                net_market_value: net_mv,
            }
        })
        .collect()
}

fn looks_like_option_symbol(symbol: &str) -> bool {
    symbol.len() >= 15 && symbol.chars().nth(12).is_some_and(|c| c == 'C' || c == 'P')
}

fn infer_strategy_hint(legs: &[&OptionPositionLeg]) -> String {
    match legs.len() {
        2 => "vertical".into(),
        4 => "iron_condor".into(),
        1 => "single_leg".into(),
        n => format!("{n}_legs"),
    }
}

pub fn find_position_group<'a>(
    groups: &'a [OptionPositionGroup],
    position_id: &str,
) -> Option<&'a OptionPositionGroup> {
    groups.iter().find(|g| g.id == position_id)
}

pub fn build_close_order_for_group(group: &OptionPositionGroup) -> Result<Value> {
    let price = close_limit_from_market_value(group);
    build_close_order_for_group_with_limit(group, price)
}

pub fn build_close_order_for_group_with_limit(
    group: &OptionPositionGroup,
    limit_price: Option<f64>,
) -> Result<Value> {
    use schwab_api::models::order::{
        ComplexOrderStrategyType, OrderDuration, OrderInstruction, OrderSession, OrderTypeRequest,
    };

    use crate::order_builder::{
        build_complex_option_order, build_single_option_order, OrderLegSpec,
    };

    if group.legs.is_empty() {
        anyhow::bail!("position group has no legs");
    }

    let leg_specs: Vec<OrderLegSpec> = group
        .legs
        .iter()
        .map(|leg| {
            let instruction = if leg.quantity > 0.0 {
                OrderInstruction::SellToClose
            } else {
                OrderInstruction::BuyToClose
            };
            Ok(OrderLegSpec {
                instruction,
                symbol: leg.symbol.clone(),
                asset_type: "OPTION",
                quantity: leg.quantity.abs(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let complex = match group.legs.len() {
        2 => ComplexOrderStrategyType::Vertical,
        4 => ComplexOrderStrategyType::IronCondor,
        _ => ComplexOrderStrategyType::Custom,
    };

    if group.legs.len() == 1 {
        let leg = &group.legs[0];
        let instruction = if leg.quantity > 0.0 {
            OrderInstruction::SellToClose
        } else {
            OrderInstruction::BuyToClose
        };
        return build_single_option_order(
            instruction,
            &leg.symbol,
            leg.quantity.abs(),
            OrderTypeRequest::Market,
            None,
            OrderDuration::Day,
            OrderSession::Normal,
            None,
        );
    }

    let order_type = if group.net_market_value >= 0.0 {
        OrderTypeRequest::NetCredit
    } else {
        OrderTypeRequest::NetDebit
    };

    build_complex_option_order(
        complex,
        order_type,
        leg_specs,
        limit_price,
        OrderDuration::Day,
        OrderSession::Normal,
        None,
    )
}

fn close_limit_from_market_value(group: &OptionPositionGroup) -> Option<f64> {
    if group.legs.len() < 2 {
        return None;
    }
    let contracts = spread_contract_count(group) as f64;
    if contracts <= 0.0 {
        return None;
    }
    let per_share = (group.net_market_value.abs() / contracts / 100.0).max(0.01);
    Some(per_share)
}

pub fn group_leg_signature(group: &OptionPositionGroup) -> String {
    let mut parts: Vec<String> = group
        .legs
        .iter()
        .map(|leg| {
            let side = if leg.quantity < 0.0 { "S" } else { "L" };
            if let Some(parsed) = leg.parsed.as_ref() {
                format!(
                    "{}{}{}",
                    parsed.put_call.to_ascii_uppercase(),
                    format_strike(parsed.strike),
                    side
                )
            } else {
                format!(
                    "{}{}",
                    leg.symbol.trim().to_uppercase().replace(' ', ""),
                    side
                )
            }
        })
        .collect();
    parts.sort();
    parts.join("_")
}

fn format_strike(strike: f64) -> String {
    if (strike.fract()).abs() < f64::EPSILON {
        format!("{strike:.0}")
    } else {
        format!("{strike:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_contract_count_uses_max_leg_quantity() {
        let group = OptionPositionGroup {
            id: "IWM|2026-07-31".into(),
            underlying: "IWM".into(),
            expiry: "2026-07-31".into(),
            strategy_hint: "vertical".into(),
            legs: vec![
                OptionPositionLeg {
                    symbol: "IWM   260731P00282000".into(),
                    underlying: "IWM".into(),
                    quantity: -2.0,
                    market_value: -632.0,
                    average_price: Some(3.16),
                    parsed: None,
                },
                OptionPositionLeg {
                    symbol: "IWM   260731P00280000".into(),
                    underlying: "IWM".into(),
                    quantity: 2.0,
                    market_value: 565.0,
                    average_price: Some(2.825),
                    parsed: None,
                },
            ],
            net_market_value: -67.0,
        };
        assert_eq!(spread_contract_count(&group), 2);
    }

    #[test]
    fn candidate_and_live_position_ids_match_vertical_signature() {
        let group = OptionPositionGroup {
            id: "IWM|2026-07-31".into(),
            underlying: "IWM".into(),
            expiry: "2026-07-31".into(),
            strategy_hint: "vertical".into(),
            legs: vec![
                OptionPositionLeg {
                    symbol: "IWM   260731P00282000".into(),
                    underlying: "IWM".into(),
                    quantity: -1.0,
                    market_value: -32.0,
                    average_price: Some(0.25),
                    parsed: parse_option_symbol("IWM   260731P00282000").ok(),
                },
                OptionPositionLeg {
                    symbol: "IWM   260731P00280000".into(),
                    underlying: "IWM".into(),
                    quantity: 1.0,
                    market_value: 10.0,
                    average_price: Some(0.05),
                    parsed: parse_option_symbol("IWM   260731P00280000").ok(),
                },
            ],
            net_market_value: -22.0,
        };
        let candidate = candidate_position_id(
            "acct",
            "IWM",
            "2026-07-31",
            "vertical",
            vec![('P', 282.0, "S"), ('P', 280.0, "L")],
        );
        assert_eq!(position_group_id("acct", &group), candidate);
    }
}
