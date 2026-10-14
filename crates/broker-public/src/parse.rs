use broker_core::{
    AccountKind, AccountRef, AssetKind, Balances, Bar, BrokerKind, Greeks, OptionChain,
    OptionContract, OptionStrategyGroup, Order, OrderStatus, Portfolio, Position, Preflight,
    Quote, Side, StrategyLeg,
};
use serde_json::Value;

pub fn num(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

pub fn accounts_from(raw: &Value) -> Vec<AccountRef> {
    raw.get("accounts")
        .and_then(|value| value.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let id = row.get("accountId")?.as_str()?.to_string();
                    let mut account = AccountRef::new(BrokerKind::Public, id);
                    account.kind = match row
                        .get("brokerageAccountType")
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                    {
                        "CASH" => AccountKind::Cash,
                        "IRA" => AccountKind::Ira,
                        _ => AccountKind::Margin,
                    };
                    account.label = row
                        .get("accountType")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    Some(account)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn portfolio_from(raw: &Value, account: &AccountRef) -> Portfolio {
    let buying = raw.get("buyingPower");
    let balances = Balances {
        cash_buying_power: num(buying.and_then(|value| value.get("cashOnlyBuyingPower"))).unwrap_or(0.0),
        options_buying_power: num(buying.and_then(|value| value.get("optionsBuyingPower"))),
        equity: num(raw.get("totalAccountValue")).unwrap_or(0.0),
    };
    let kind = match raw.get("accountType").and_then(|value| value.as_str()) {
        Some("TRADITIONAL_IRA") | Some("ROTH_IRA") => AccountKind::Ira,
        Some(_) if balances.options_buying_power.is_none() => AccountKind::Cash,
        _ => account.kind,
    };
    let positions = raw
        .get("positions")
        .and_then(|value| value.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let instrument = row.get("instrument")?;
                    let symbol = instrument.get("symbol")?.as_str()?.to_string();
                    let asset = match instrument.get("type").and_then(|value| value.as_str()) {
                        Some("OPTION") => AssetKind::Option,
                        _ => AssetKind::Equity,
                    };
                    Some(Position {
                        symbol,
                        asset,
                        quantity: num(row.get("quantity")).unwrap_or(0.0),
                        average_price: num(row
                            .get("costBasis")
                            .and_then(|value| value.get("unitCost"))),
                        market_value: num(row.get("currentValue")),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let strategies = raw
        .get("strategies")
        .and_then(|value| value.as_array())
        .map(|rows| rows.iter().filter_map(strategy_from).collect())
        .unwrap_or_default();
    Portfolio {
        account: account.clone(),
        kind,
        balances,
        positions,
        strategies,
    }
}

fn strategy_from(row: &Value) -> Option<OptionStrategyGroup> {
    let legs = row
        .get("optionLegs")
        .and_then(|value| value.as_array())
        .map(|legs| {
            legs.iter()
                .filter_map(|leg| {
                    let symbol = leg.get("symbol")?.as_str()?.to_string();
                    let ratio = num(leg.get("ratioQuantity")).unwrap_or(1.0);
                    let side = match leg
                        .get("positionType")
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                        .to_ascii_uppercase()
                        .as_str()
                    {
                        "SHORT" | "SELL" => Side::Sell,
                        _ => Side::Buy,
                    };
                    Some(StrategyLeg {
                        symbol,
                        quantity: ratio,
                        side,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(OptionStrategyGroup {
        id: row.get("strategyId")?.as_str()?.to_string(),
        name: row
            .get("displayName")
            .and_then(|value| value.as_str())
            .unwrap_or("strategy")
            .to_string(),
        quantity: num(row.get("quantity")).unwrap_or(1.0),
        legs,
        market_value: num(row.get("currentValue")),
        unit_cost: num(row.get("costBasis").and_then(|value| value.get("unitCost"))),
    })
}

pub fn orders_from(raw: &Value) -> Vec<Order> {
    let rows = raw
        .get("orders")
        .and_then(|value| value.as_array())
        .cloned()
        .or_else(|| raw.as_array().cloned())
        .unwrap_or_default();
    rows.iter().filter_map(order_from).collect()
}

pub fn order_from(raw: &Value) -> Option<Order> {
    let id = raw.get("orderId")?.as_str()?.to_string();
    let raw_status = raw
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("NEW")
        .to_string();
    Some(Order {
        id,
        status: OrderStatus::parse(&raw_status),
        filled_quantity: num(raw.get("filledQuantity")).unwrap_or(0.0),
        average_price: num(raw.get("averagePrice")),
        raw_status,
    })
}

pub fn preflight_from(raw: &Value) -> Preflight {
    let message = raw
        .get("message")
        .or_else(|| raw.get("error"))
        .and_then(|value| value.as_str())
        .unwrap_or("Public preflight")
        .to_string();
    let rejected = raw.get("error").is_some()
        || message.to_ascii_lowercase().contains("insufficient")
        || message.to_ascii_lowercase().contains("reject");
    Preflight {
        accepted: !rejected,
        estimated_cost: num(raw.get("estimatedCost")).or_else(|| num(raw.get("orderValue"))),
        estimated_proceeds: num(raw.get("estimatedProceeds")),
        buying_power_requirement: num(raw.get("buyingPowerRequirement")),
        fees: num(raw.get("estimatedCommission")),
        message,
    }
}

pub fn quotes_from(raw: &Value) -> Vec<Quote> {
    let rows = raw
        .get("quotes")
        .or_else(|| raw.get("instruments"))
        .or_else(|| raw.get("data"))
        .and_then(|value| value.as_array())
        .cloned()
        .or_else(|| raw.as_array().cloned())
        .unwrap_or_else(|| {
            raw.as_object()
                .map(|map| map.values().cloned().collect())
                .unwrap_or_default()
        });
    rows.iter().filter_map(quote_from).collect()
}

fn quote_from(row: &Value) -> Option<Quote> {
    let symbol = row
        .get("instrument")
        .and_then(|value| value.get("symbol"))
        .and_then(|value| value.as_str())
        .or_else(|| row.get("symbol").and_then(|value| value.as_str()))?
        .to_string();
    let outcome = row.get("outcome").and_then(|value| value.as_str()).unwrap_or("SUCCESS");
    if outcome != "SUCCESS" && row.get("bid").is_none() && row.get("last").is_none() {
        return None;
    }
    Some(Quote {
        symbol,
        bid: num(row.get("bid")),
        ask: num(row.get("ask")),
        last: num(row.get("last")).or_else(|| num(row.get("lastPrice"))),
        bid_size: num(row.get("bidSize")),
        ask_size: num(row.get("askSize")),
        volume: num(row.get("volume")),
    })
}

pub fn expirations_from(raw: &Value) -> Vec<String> {
    raw.get("expirations")
        .and_then(|value| value.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub fn chain_from(underlying: &str, expiration: &str, raw: &Value) -> OptionChain {
    let mut contracts = Vec::new();
    for (key, put_call) in [("calls", "C"), ("puts", "P")] {
        if let Some(rows) = raw.get(key).and_then(|value| value.as_array()) {
            for row in rows {
                if let Some(contract) = contract_from(row, expiration, put_call) {
                    contracts.push(contract);
                }
            }
        }
    }
    OptionChain {
        underlying: underlying.to_uppercase(),
        expirations: vec![expiration.to_string()],
        contracts,
    }
}

fn contract_from(row: &Value, expiration: &str, put_call: &str) -> Option<OptionContract> {
    let symbol = row
        .get("instrument")
        .and_then(|value| value.get("symbol"))
        .and_then(|value| value.as_str())
        .or_else(|| row.get("symbol").and_then(|value| value.as_str()))?
        .to_string();
    let details = row.get("optionDetails");
    let greeks = details.and_then(|value| value.get("greeks"));
    Some(OptionContract {
        symbol,
        expiration: expiration.to_string(),
        strike: num(details.and_then(|value| value.get("strikePrice")))
            .or_else(|| num(row.get("strikePrice")))
            .unwrap_or(0.0),
        put_call: put_call.into(),
        bid: num(row.get("bid")),
        ask: num(row.get("ask")),
        open_interest: num(row.get("openInterest")),
        greeks: Greeks {
            delta: num(greeks.and_then(|value| value.get("delta"))),
            gamma: num(greeks.and_then(|value| value.get("gamma"))),
            theta: num(greeks.and_then(|value| value.get("theta"))),
            vega: num(greeks.and_then(|value| value.get("vega"))),
            iv: num(greeks.and_then(|value| value.get("impliedVolatility"))),
        },
    })
}

pub fn greeks_from(raw: &Value) -> Greeks {
    let details = raw
        .pointer("/quotes/0/optionDetails/greeks")
        .or_else(|| raw.pointer("/0/optionDetails/greeks"))
        .or_else(|| raw.get("greeks"));
    Greeks {
        delta: num(details.and_then(|value| value.get("delta"))),
        gamma: num(details.and_then(|value| value.get("gamma"))),
        theta: num(details.and_then(|value| value.get("theta"))),
        vega: num(details.and_then(|value| value.get("vega"))),
        iv: num(details.and_then(|value| value.get("impliedVolatility"))),
    }
}

pub fn bars_from(raw: &Value) -> Vec<Bar> {
    let mut rows = Vec::new();
    if let Some(map) = raw.as_object() {
        for key in ["regularMarket", "bars", "preMarket", "afterMarket"] {
            if let Some(list) = map.get(key).and_then(|value| value.get("bars")).and_then(|v| v.as_array())
            {
                rows.extend(list.iter().cloned());
            } else if key == "bars" {
                if let Some(list) = map.get(key).and_then(|value| value.as_array()) {
                    rows.extend(list.iter().cloned());
                }
            }
        }
    }
    rows.iter()
        .filter_map(|row| {
            Some(Bar {
                timestamp: row
                    .get("timestamp")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string(),
                open: num(row.get("open"))?,
                high: num(row.get("high"))?,
                low: num(row.get("low"))?,
                close: num(row.get("close")).or_else(|| num(row.get("value")))?,
                volume: num(row.get("volume")).unwrap_or(0.0),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn spread_preflight_without_message_is_accepted() {
        let raw = json!({
            "strategyName": "SPY Bull Put Spread",
            "estimatedCommission": "0.12",
            "orderValue": "-3.00",
            "estimatedCost": "-3.03",
            "buyingPowerRequirement": "-2.91",
            "estimatedProceeds": "3.03"
        });
        let preflight = preflight_from(&raw);
        assert!(preflight.accepted);
        assert_eq!(preflight.estimated_cost, Some(-3.03));
        assert_eq!(preflight.estimated_proceeds, Some(3.03));
        assert_eq!(preflight.buying_power_requirement, Some(-2.91));
        assert_eq!(preflight.fees, Some(0.12));
    }
}
