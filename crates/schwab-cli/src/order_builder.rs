use anyhow::{bail, Result};
use schwab_api::models::order::{
    ComplexOrderStrategyType, OrderDuration, OrderInstruction, OrderSession, OrderStrategyType,
    OrderTypeRequest,
};
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

#[derive(Debug, Clone)]
pub struct OrderLegSpec {
    pub instruction: OrderInstruction,
    pub symbol: String,
    pub asset_type: &'static str,
    pub quantity: f64,
}

#[derive(Debug, Clone)]
pub struct OrderRequestSpec {
    pub session: OrderSession,
    pub duration: OrderDuration,
    pub order_type: OrderTypeRequest,
    pub order_strategy_type: OrderStrategyType,
    pub complex_strategy: ComplexOrderStrategyType,
    pub legs: Vec<OrderLegSpec>,
    pub price: Option<f64>,
    pub stop_price: Option<f64>,
    pub cancel_time: Option<String>,
}

pub fn build_order_request(spec: OrderRequestSpec) -> Result<Value> {
    if spec.legs.is_empty() {
        bail!("orderLegCollection must contain at least one leg");
    }

    for leg in &spec.legs {
        if leg.quantity <= 0.0 {
            bail!("leg quantity must be positive");
        }
    }

    if matches!(
        spec.order_type,
        OrderTypeRequest::Limit
            | OrderTypeRequest::StopLimit
            | OrderTypeRequest::NetDebit
            | OrderTypeRequest::NetCredit
            | OrderTypeRequest::LimitOnClose
    ) && spec.price.is_none()
    {
        bail!("price is required for {:?}", spec.order_type);
    }

    let legs: Vec<Value> = spec
        .legs
        .iter()
        .map(|leg| {
            json!({
                "instruction": leg.instruction,
                "quantity": leg.quantity,
                "instrument": {
                    "symbol": leg.symbol.trim().to_uppercase(),
                    "assetType": leg.asset_type
                }
            })
        })
        .collect();

    let mut order = json!({
        "orderType": spec.order_type,
        "session": spec.session,
        "duration": spec.duration,
        "orderStrategyType": spec.order_strategy_type,
        "complexOrderStrategyType": spec.complex_strategy,
        "orderLegCollection": legs,
    });

    if let Some(price) = spec.price {
        order["price"] = json!(format_price(price));
    }
    if let Some(stop) = spec.stop_price {
        order["stopPrice"] = json!(format_price(stop));
    }
    if let Some(cancel_time) = spec.cancel_time {
        order["cancelTime"] = json!(cancel_time);
    }

    Ok(order)
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
    let instruction = match side {
        TradeSide::Buy => OrderInstruction::Buy,
        TradeSide::Sell => OrderInstruction::Sell,
    };

    let order_type_api = match order_type {
        TradeOrderType::Market => OrderTypeRequest::Market,
        TradeOrderType::Limit => OrderTypeRequest::Limit,
    };

    build_order_request(OrderRequestSpec {
        session,
        duration,
        order_type: order_type_api,
        order_strategy_type: OrderStrategyType::Single,
        complex_strategy: ComplexOrderStrategyType::None,
        legs: vec![OrderLegSpec {
            instruction,
            symbol: symbol.to_string(),
            asset_type: "EQUITY",
            quantity,
        }],
        price: limit_price,
        stop_price: None,
        cancel_time: None,
    })
}

/// Build a single-leg option order (not a spread).
#[allow(clippy::too_many_arguments, dead_code)]
pub fn build_single_option_order(
    instruction: OrderInstruction,
    option_symbol: &str,
    quantity: f64,
    order_type: OrderTypeRequest,
    price: Option<f64>,
    duration: OrderDuration,
    session: OrderSession,
    cancel_time: Option<String>,
) -> Result<Value> {
    build_order_request(OrderRequestSpec {
        session,
        duration,
        order_type,
        order_strategy_type: OrderStrategyType::Single,
        complex_strategy: ComplexOrderStrategyType::None,
        legs: vec![OrderLegSpec {
            instruction,
            symbol: option_symbol.to_string(),
            asset_type: "OPTION",
            quantity,
        }],
        price,
        stop_price: None,
        cancel_time,
    })
}

/// Build a multi-leg complex option order (spread, iron condor, etc.).
#[allow(dead_code)]
pub fn build_complex_option_order(
    complex_strategy: ComplexOrderStrategyType,
    order_type: OrderTypeRequest,
    legs: Vec<OrderLegSpec>,
    price: Option<f64>,
    duration: OrderDuration,
    session: OrderSession,
    cancel_time: Option<String>,
) -> Result<Value> {
    if legs.len() < 2 {
        bail!("complex option orders require at least two legs");
    }
    if complex_strategy == ComplexOrderStrategyType::None {
        bail!("complexOrderStrategyType must be set for multi-leg option orders");
    }

    build_order_request(OrderRequestSpec {
        session,
        duration,
        order_type,
        order_strategy_type: OrderStrategyType::Single,
        complex_strategy,
        legs,
        price,
        stop_price: None,
        cancel_time,
    })
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

#[allow(dead_code)]
pub fn parse_order_type_request(raw: &str) -> Result<OrderTypeRequest> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "MARKET" => Ok(OrderTypeRequest::Market),
        "LIMIT" => Ok(OrderTypeRequest::Limit),
        "STOP" => Ok(OrderTypeRequest::Stop),
        "STOP_LIMIT" => Ok(OrderTypeRequest::StopLimit),
        "TRAILING_STOP" => Ok(OrderTypeRequest::TrailingStop),
        "NET_DEBIT" => Ok(OrderTypeRequest::NetDebit),
        "NET_CREDIT" => Ok(OrderTypeRequest::NetCredit),
        "NET_ZERO" => Ok(OrderTypeRequest::NetZero),
        "LIMIT_ON_CLOSE" => Ok(OrderTypeRequest::LimitOnClose),
        "MARKET_ON_CLOSE" => Ok(OrderTypeRequest::MarketOnClose),
        "EXERCISE" => Ok(OrderTypeRequest::Exercise),
        other => bail!("Unknown order type `{other}`"),
    }
}

#[allow(dead_code)]
pub fn parse_order_instruction(raw: &str) -> Result<OrderInstruction> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "BUY" => Ok(OrderInstruction::Buy),
        "SELL" => Ok(OrderInstruction::Sell),
        "BUY_TO_OPEN" => Ok(OrderInstruction::BuyToOpen),
        "SELL_TO_CLOSE" => Ok(OrderInstruction::SellToClose),
        "SELL_TO_OPEN" => Ok(OrderInstruction::SellToOpen),
        "BUY_TO_CLOSE" => Ok(OrderInstruction::BuyToClose),
        "SELL_SHORT" => Ok(OrderInstruction::SellShort),
        "BUY_TO_COVER" => Ok(OrderInstruction::BuyToCover),
        other => bail!("Unknown instruction `{other}`"),
    }
}

#[allow(dead_code)]
pub fn parse_complex_order_strategy_type(raw: &str) -> Result<ComplexOrderStrategyType> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "NONE" => Ok(ComplexOrderStrategyType::None),
        "COVERED" => Ok(ComplexOrderStrategyType::Covered),
        "VERTICAL" => Ok(ComplexOrderStrategyType::Vertical),
        "BACK_RATIO" => Ok(ComplexOrderStrategyType::BackRatio),
        "CALENDAR" => Ok(ComplexOrderStrategyType::Calendar),
        "DIAGONAL" => Ok(ComplexOrderStrategyType::Diagonal),
        "STRADDLE" => Ok(ComplexOrderStrategyType::Straddle),
        "STRANGLE" => Ok(ComplexOrderStrategyType::Strangle),
        "COLLAR_SYNTHETIC" => Ok(ComplexOrderStrategyType::CollarSynthetic),
        "BUTTERFLY" => Ok(ComplexOrderStrategyType::Butterfly),
        "CONDOR" => Ok(ComplexOrderStrategyType::Condor),
        "IRON_CONDOR" => Ok(ComplexOrderStrategyType::IronCondor),
        "VERTICAL_ROLL" => Ok(ComplexOrderStrategyType::VerticalRoll),
        "COLLAR_WITH_STOCK" => Ok(ComplexOrderStrategyType::CollarWithStock),
        "DOUBLE_DIAGONAL" => Ok(ComplexOrderStrategyType::DoubleDiagonal),
        "UNBALANCED_BUTTERFLY" => Ok(ComplexOrderStrategyType::UnbalancedButterfly),
        "UNBALANCED_CONDOR" => Ok(ComplexOrderStrategyType::UnbalancedCondor),
        "UNBALANCED_IRON_CONDOR" => Ok(ComplexOrderStrategyType::UnbalancedIronCondor),
        "UNBALANCED_VERTICAL_ROLL" => Ok(ComplexOrderStrategyType::UnbalancedVerticalRoll),
        "MUTUAL_FUND_SWAP" => Ok(ComplexOrderStrategyType::MutualFundSwap),
        "CUSTOM" => Ok(ComplexOrderStrategyType::Custom),
        other => bail!("Unknown complexOrderStrategyType `{other}`"),
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
        assert_eq!(order["complexOrderStrategyType"], "NONE");
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

    #[test]
    fn builds_vertical_spread_with_cancel_time() {
        let order = build_complex_option_order(
            ComplexOrderStrategyType::Vertical,
            OrderTypeRequest::NetDebit,
            vec![
                OrderLegSpec {
                    instruction: OrderInstruction::BuyToOpen,
                    symbol: "AAPL  260620C00180000".into(),
                    asset_type: "OPTION",
                    quantity: 1.0,
                },
                OrderLegSpec {
                    instruction: OrderInstruction::SellToOpen,
                    symbol: "AAPL  260620C00185000".into(),
                    asset_type: "OPTION",
                    quantity: 1.0,
                },
            ],
            Some(0.50),
            OrderDuration::Day,
            OrderSession::Normal,
            Some("2026-06-19T16:00:00-04:00".into()),
        )
        .unwrap();
        assert_eq!(order["complexOrderStrategyType"], "VERTICAL");
        assert_eq!(order["orderType"], "NET_DEBIT");
        assert_eq!(order["cancelTime"], "2026-06-19T16:00:00-04:00");
        assert_eq!(order["orderLegCollection"].as_array().unwrap().len(), 2);
    }
}
