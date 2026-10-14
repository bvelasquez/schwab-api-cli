//! Schwab JSON built to match `schwab_cli::order_builder` and the swing OCO builder.
//! Those crates are not edited. Parity tests compare the two outputs.

use anyhow::{bail, Result};
use broker_core::{
    AssetKind, EquitySize, ExitBracket, LegInstruction, NetPrice, OrderIntent, Session, Side,
    SpreadKind, TimeInForce,
};
use serde_json::{json, Value};

pub fn schwab_order_json(intent: &OrderIntent) -> Result<Value> {
    match intent {
        OrderIntent::Equity {
            side,
            symbol,
            size,
            limit,
            tif,
            session,
            bracket,
        } => {
            if bracket.is_some() {
                bail!("Schwab equity orders do not embed a native bracket");
            }
            let EquitySize::Shares(quantity) = size else {
                bail!("Schwab equity orders require a share quantity");
            };
            equity_order(*side, symbol, *quantity, *limit, *tif, *session)
        }
        OrderIntent::Spread {
            strategy,
            legs,
            contracts,
            net,
            tif,
            session,
        } => {
            if legs.len() < 2 {
                bail!("spread requires at least two legs");
            }
            let (order_type, price) = match net {
                NetPrice::Credit(price) => ("NET_CREDIT", *price),
                NetPrice::Debit(price) => ("NET_DEBIT", *price),
            };
            let leg_json: Vec<Value> = legs
                .iter()
                .map(|leg| {
                    json!({
                        "instruction": instruction(leg.instruction),
                        "quantity": leg.ratio * contracts,
                        "instrument": {
                            "symbol": leg.symbol.trim().to_uppercase(),
                            "assetType": asset(leg.asset)
                        }
                    })
                })
                .collect();
            Ok(json!({
                "orderType": order_type,
                "session": session_str(*session)?,
                "duration": tif_str(*tif),
                "orderStrategyType": "SINGLE",
                "complexOrderStrategyType": strategy_str(*strategy),
                "orderLegCollection": leg_json,
                "price": format_price(price),
            }))
        }
        OrderIntent::ExitBracket(bracket) => Ok(oco_json(bracket)),
    }
}

fn equity_order(
    side: Side,
    symbol: &str,
    quantity: f64,
    limit: Option<f64>,
    tif: TimeInForce,
    session: Session,
) -> Result<Value> {
    let order_type = if limit.is_some() { "LIMIT" } else { "MARKET" };
    let mut order = json!({
        "orderType": order_type,
        "session": session_str(session)?,
        "duration": tif_str(tif),
        "orderStrategyType": "SINGLE",
        "complexOrderStrategyType": "NONE",
        "orderLegCollection": [{
            "instruction": match side {
                Side::Buy => "BUY",
                Side::Sell => "SELL",
            },
            "quantity": quantity,
            "instrument": {
                "symbol": symbol.trim().to_uppercase(),
                "assetType": "EQUITY"
            }
        }],
    });
    if let Some(price) = limit {
        order["price"] = json!(format_price(price));
    }
    Ok(order)
}

pub fn oco_json(bracket: &ExitBracket) -> Value {
    let duration = tif_str(bracket.tif);
    let symbol = bracket.symbol.trim().to_uppercase();
    json!({
        "orderStrategyType": "OCO",
        "childOrderStrategies": [
            {
                "orderType": "LIMIT",
                "session": "NORMAL",
                "price": format!("{:.2}", bracket.take_profit),
                "duration": duration,
                "orderStrategyType": "SINGLE",
                "orderLegCollection": [{
                    "instruction": "SELL",
                    "quantity": bracket.quantity,
                    "instrument": {
                        "symbol": symbol,
                        "assetType": "EQUITY"
                    }
                }]
            },
            {
                "orderType": "STOP_LIMIT",
                "session": "NORMAL",
                "stopPrice": format!("{:.2}", bracket.stop),
                "price": format!("{:.2}", bracket.stop_limit),
                "duration": duration,
                "orderStrategyType": "SINGLE",
                "orderLegCollection": [{
                    "instruction": "SELL",
                    "quantity": bracket.quantity,
                    "instrument": {
                        "symbol": symbol,
                        "assetType": "EQUITY"
                    }
                }]
            }
        ]
    })
}

fn instruction(value: LegInstruction) -> &'static str {
    match value {
        LegInstruction::Buy => "BUY",
        LegInstruction::Sell => "SELL",
        LegInstruction::BuyToOpen => "BUY_TO_OPEN",
        LegInstruction::SellToOpen => "SELL_TO_OPEN",
        LegInstruction::BuyToClose => "BUY_TO_CLOSE",
        LegInstruction::SellToClose => "SELL_TO_CLOSE",
    }
}

fn asset(value: AssetKind) -> &'static str {
    match value {
        AssetKind::Equity => "EQUITY",
        AssetKind::Option => "OPTION",
    }
}

fn strategy_str(value: SpreadKind) -> &'static str {
    match value {
        SpreadKind::Vertical => "VERTICAL",
        SpreadKind::IronCondor => "IRON_CONDOR",
        SpreadKind::Custom => "CUSTOM",
    }
}

fn tif_str(value: TimeInForce) -> &'static str {
    match value {
        TimeInForce::Day => "DAY",
        TimeInForce::Gtc => "GOOD_TILL_CANCEL",
        TimeInForce::Fok => "FILL_OR_KILL",
    }
}

fn session_str(value: Session) -> Result<&'static str> {
    match value {
        Session::Core => Ok("NORMAL"),
        Session::Extended => Ok("SEAMLESS"),
        Session::TwentyFourHours => bail!("Schwab has no 24-hour equity session"),
    }
}

/// Same rule as `schwab_cli::order_builder`: whole numbers omit decimals.
pub fn format_price(price: f64) -> String {
    if price.fract().abs() < f64::EPSILON {
        format!("{price:.0}")
    } else {
        format!("{price:.2}")
    }
}
