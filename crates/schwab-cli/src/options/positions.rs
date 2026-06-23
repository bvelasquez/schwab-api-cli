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

pub fn group_option_legs(legs: &[OptionPositionLeg]) -> Vec<OptionPositionGroup> {
    use std::collections::HashMap;

    let mut by_key: HashMap<String, Vec<&OptionPositionLeg>> = HashMap::new();
    for leg in legs {
        let expiry = leg
            .parsed
            .as_ref()
            .map(|p| p.expiry.to_string())
            .unwrap_or_else(|| "unknown".into());
        let key = format!("{}|{}", leg.underlying, expiry);
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
    symbol.len() >= 15
        && symbol
            .chars()
            .nth(12)
            .is_some_and(|c| c == 'C' || c == 'P')
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
    use schwab_api::models::order::{
        ComplexOrderStrategyType, OrderDuration, OrderInstruction, OrderSession, OrderTypeRequest,
    };

    use crate::order_builder::{build_complex_option_order, build_single_option_order, OrderLegSpec};

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
        None,
        OrderDuration::Day,
        OrderSession::Normal,
        None,
    )
}
