use anyhow::{bail, Result};
use schwab_api::models::order::{OrderDuration, OrderInstruction, OrderSession, OrderType};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeOrderType {
    Market,
    Limit,
}

pub fn build_equity_order(
    side: TradeSide,
    symbol: &str,
    quantity: f64,
    order_type: TradeOrderType,
    limit_price: Option<f64>,
    duration: OrderDuration,
    session: OrderSession,
) -> Result<Value> {
    if quantity <= 0.0 {
        bail!("quantity must be positive");
    }

    let instruction = match side {
        TradeSide::Buy => OrderInstruction::Buy,
        TradeSide::Sell => OrderInstruction::Sell,
    };

    let order_type_api = match order_type {
        TradeOrderType::Market => OrderType::Market,
        TradeOrderType::Limit => OrderType::Limit,
    };

    if order_type == TradeOrderType::Limit && limit_price.is_none() {
        bail!("limit price is required for LIMIT orders");
    }

    let mut order = json!({
        "orderType": order_type_api,
        "session": session,
        "duration": duration,
        "orderStrategyType": "SINGLE",
        "orderLegCollection": [{
            "instruction": instruction,
            "quantity": quantity,
            "instrument": {
                "symbol": symbol.trim().to_uppercase(),
                "assetType": "EQUITY"
            }
        }]
    });

    if let Some(price) = limit_price {
        order["price"] = json!(format_price(price));
    }

    Ok(order)
}

fn format_price(price: f64) -> String {
    if (price.fract()).abs() < f64::EPSILON {
        format!("{price:.0}")
    } else {
        format!("{price:.2}")
    }
}

pub fn parse_trade_order_type(raw: &str) -> Result<TradeOrderType> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "market" | "mkt" => Ok(TradeOrderType::Market),
        "limit" | "lmt" => Ok(TradeOrderType::Limit),
        other => bail!("Unknown order type `{other}` (use market or limit)"),
    }
}

pub fn parse_duration(raw: Option<&str>) -> Result<OrderDuration> {
    match raw
        .unwrap_or("day")
        .trim()
        .to_ascii_uppercase()
        .as_str()
    {
        "DAY" => Ok(OrderDuration::Day),
        "GTC" | "GOOD_TILL_CANCEL" => Ok(OrderDuration::GoodTillCancel),
        "FOK" | "FILL_OR_KILL" => Ok(OrderDuration::FillOrKill),
        other => bail!("Unknown duration `{other}` (use day, gtc, or fok)"),
    }
}

pub fn parse_session(raw: Option<&str>) -> Result<OrderSession> {
    match raw
        .unwrap_or("normal")
        .trim()
        .to_ascii_uppercase()
        .as_str()
    {
        "NORMAL" => Ok(OrderSession::Normal),
        "AM" => Ok(OrderSession::Am),
        "PM" => Ok(OrderSession::Pm),
        "SEAMLESS" => Ok(OrderSession::Seamless),
        other => bail!("Unknown session `{other}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_market_buy() {
        let order = build_equity_order(
            TradeSide::Buy,
            "aapl",
            5.0,
            TradeOrderType::Market,
            None,
            OrderDuration::Day,
            OrderSession::Normal,
        )
        .unwrap();
        assert_eq!(order["orderType"], "MARKET");
        assert_eq!(order["orderLegCollection"][0]["instruction"], "BUY");
        assert_eq!(order["orderLegCollection"][0]["instrument"]["symbol"], "AAPL");
    }

    #[test]
    fn builds_limit_sell() {
        let order = build_equity_order(
            TradeSide::Sell,
            "MSFT",
            2.0,
            TradeOrderType::Limit,
            Some(350.5),
            OrderDuration::Day,
            OrderSession::Normal,
        )
        .unwrap();
        assert_eq!(order["orderType"], "LIMIT");
        assert_eq!(order["price"], "350.50");
    }
}
