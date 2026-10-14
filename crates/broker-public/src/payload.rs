use anyhow::{bail, Result};
use broker_core::{
    AssetKind, EquitySize, ExitBracket, LegInstruction, NetPrice, OrderIntent, Session, Side,
    SpreadLeg, TimeInForce,
};
use serde_json::{json, Value};

pub fn single_leg_body(intent: &OrderIntent, order_id: Option<&str>, use_margin: bool) -> Result<Value> {
    let OrderIntent::Equity {
        side,
        symbol,
        size,
        limit,
        tif,
        session,
        bracket,
    } = intent
    else {
        bail!("single-leg payload requires an equity intent");
    };
    let mut body = json!({
        "instrument": {"symbol": symbol.trim().to_uppercase(), "type": "EQUITY"},
        "orderSide": match side { Side::Buy => "BUY", Side::Sell => "SELL" },
        "orderType": if limit.is_some() { "LIMIT" } else { "MARKET" },
        "expiration": {"timeInForce": tif_str(*tif)},
        "equityMarketSession": session_str(*session),
        "useMargin": use_margin,
    });
    if let Some(order_id) = order_id {
        body["orderId"] = json!(order_id);
    }
    match size {
        EquitySize::Shares(qty) => body["quantity"] = json!(trim_num(*qty)),
        EquitySize::Notional(amount) => body["amount"] = json!(trim_num(*amount)),
    }
    if let Some(price) = limit {
        body["limitPrice"] = json!(format!("{price:.2}"));
    }
    if let Some(bracket) = bracket {
        body["orderClass"] = json!("BRACKET");
        body["takeProfit"] = json!({"limitPrice": format!("{:.2}", bracket.take_profit)});
        body["stopLoss"] = json!({
            "stopPrice": format!("{:.2}", bracket.stop),
            "limitPrice": format!("{:.2}", bracket.stop_limit),
        });
    }
    Ok(body)
}

pub fn exit_bracket_body(bracket: &ExitBracket, order_id: &str, use_margin: bool) -> Value {
    json!({
        "orderId": order_id,
        "instrument": {"symbol": bracket.symbol.trim().to_uppercase(), "type": "EQUITY"},
        "orderSide": "SELL",
        "orderType": "LIMIT",
        "orderClass": "OCO",
        "expiration": {"timeInForce": tif_str(bracket.tif)},
        "equityMarketSession": "CORE",
        "quantity": trim_num(bracket.quantity),
        "limitPrice": format!("{:.2}", bracket.take_profit),
        "useMargin": use_margin,
        "takeProfit": {"limitPrice": format!("{:.2}", bracket.take_profit)},
        "stopLoss": {
            "stopPrice": format!("{:.2}", bracket.stop),
            "limitPrice": format!("{:.2}", bracket.stop_limit)
        }
    })
}

pub fn multileg_body(intent: &OrderIntent, order_id: Option<&str>, use_margin: bool) -> Result<Value> {
    let OrderIntent::Spread {
        legs,
        contracts,
        net,
        tif,
        ..
    } = intent
    else {
        bail!("multi-leg payload requires a spread intent");
    };
    if legs.len() < 2 || legs.len() > 6 {
        bail!("Public multi-leg orders take 2 to 6 legs");
    }
    let equity_legs = legs.iter().filter(|leg| leg.asset == AssetKind::Equity).count();
    if equity_legs > 1 {
        bail!("Public allows at most one equity leg on a multi-leg order");
    }
    let limit = match net {
        NetPrice::Debit(price) => *price,
        NetPrice::Credit(price) => -price.abs(),
    };
    let mut body = json!({
        "quantity": contracts.round() as i64,
        "orderType": "LIMIT",
        "limitPrice": format!("{limit:.2}"),
        "expiration": {"timeInForce": tif_str(*tif)},
        "useMargin": use_margin,
        "legs": legs.iter().map(leg_json).collect::<Vec<_>>(),
    });
    if let Some(order_id) = order_id {
        body["orderId"] = json!(order_id);
    }
    Ok(body)
}

fn leg_json(leg: &SpreadLeg) -> Value {
    let (side, open_close) = match leg.instruction {
        LegInstruction::Buy | LegInstruction::BuyToOpen => ("BUY", "OPEN"),
        LegInstruction::BuyToClose => ("BUY", "CLOSE"),
        LegInstruction::Sell | LegInstruction::SellToClose => ("SELL", "CLOSE"),
        LegInstruction::SellToOpen => ("SELL", "OPEN"),
    };
    json!({
        "instrument": {
            "symbol": leg.symbol.trim().to_uppercase(),
            "type": match leg.asset { AssetKind::Equity => "EQUITY", AssetKind::Option => "OPTION" }
        },
        "side": side,
        "openCloseIndicator": open_close,
        "ratioQuantity": leg.ratio.round() as i64
    })
}

fn tif_str(value: TimeInForce) -> &'static str {
    match value {
        TimeInForce::Day => "DAY",
        TimeInForce::Gtc => "GTC",
        TimeInForce::Fok => "IOC",
    }
}

fn session_str(value: Session) -> &'static str {
    match value {
        Session::Core => "CORE",
        Session::Extended => "EXTENDED",
        Session::TwentyFourHours => "TWENTY_FOUR_HOURS",
    }
}

fn trim_num(value: f64) -> String {
    if value.fract().abs() < 1e-9 {
        format!("{value:.0}")
    } else {
        format!("{value:.4}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use broker_core::SpreadKind;

    #[test]
    fn credit_spread_limit_is_negative() {
        let intent = OrderIntent::Spread {
            strategy: SpreadKind::Vertical,
            legs: vec![
                SpreadLeg {
                    instruction: LegInstruction::SellToOpen,
                    symbol: "SPY   260815P00520000".into(),
                    asset: AssetKind::Option,
                    ratio: 1.0,
                },
                SpreadLeg {
                    instruction: LegInstruction::BuyToOpen,
                    symbol: "SPY   260815P00515000".into(),
                    asset: AssetKind::Option,
                    ratio: 1.0,
                },
            ],
            contracts: 1.0,
            net: NetPrice::Credit(0.5),
            tif: TimeInForce::Day,
            session: Session::Core,
        };
        let body = multileg_body(&intent, Some("00000000-0000-4000-8000-000000000001"), false).unwrap();
        assert_eq!(body["orderType"], "LIMIT");
        assert!(body.get("type").is_none());
        assert_eq!(body["limitPrice"], "-0.50");
        assert_eq!(body["legs"][0]["side"], "SELL");
        assert_eq!(body["legs"][0]["openCloseIndicator"], "OPEN");
        assert_eq!(body["useMargin"], false);
    }
}
