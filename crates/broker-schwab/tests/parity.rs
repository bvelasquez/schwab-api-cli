use broker_core::{
    AssetKind, EquitySize, ExitBracket, LegInstruction, NetPrice, OrderIntent, Session, Side,
    SpreadKind, SpreadLeg, TimeInForce,
};
use broker_schwab::schwab_order_json;
use schwab_api::models::order::{
    ComplexOrderStrategyType, OrderDuration, OrderInstruction, OrderSession, OrderTypeRequest,
};
use schwab_cli::order_builder::{
    build_complex_option_order, build_equity_order, OrderLegSpec, TradeOrderType, TradeSide,
};
use serde_json::Value;

fn equity(side: TradeSide, symbol: &str, qty: f64, limit: Option<f64>) -> Value {
    build_equity_order(
        side,
        symbol,
        qty,
        if limit.is_some() {
            TradeOrderType::Limit
        } else {
            TradeOrderType::Market
        },
        limit,
        OrderDuration::Day,
        OrderSession::Normal,
    )
    .unwrap()
}

#[test]
fn market_buy_matches_legacy_builder() {
    let legacy = equity(TradeSide::Buy, "aapl", 5.0, None);
    let intent = OrderIntent::Equity {
        side: Side::Buy,
        symbol: "aapl".into(),
        size: EquitySize::Shares(5.0),
        limit: None,
        tif: TimeInForce::Day,
        session: Session::Core,
        bracket: None,
    };
    assert_eq!(schwab_order_json(&intent).unwrap(), legacy);
}

#[test]
fn limit_sell_matches_legacy_builder() {
    let legacy = equity(TradeSide::Sell, "MSFT", 2.0, Some(350.5));
    let intent = OrderIntent::Equity {
        side: Side::Sell,
        symbol: "MSFT".into(),
        size: EquitySize::Shares(2.0),
        limit: Some(350.5),
        tif: TimeInForce::Day,
        session: Session::Core,
        bracket: None,
    };
    assert_eq!(schwab_order_json(&intent).unwrap(), legacy);
}

#[test]
fn vertical_matches_legacy_builder() {
    let legacy = build_complex_option_order(
        ComplexOrderStrategyType::Vertical,
        OrderTypeRequest::NetCredit,
        vec![
            OrderLegSpec {
                instruction: OrderInstruction::SellToOpen,
                symbol: "SPY   260815P00520000".into(),
                asset_type: "OPTION",
                quantity: 1.0,
            },
            OrderLegSpec {
                instruction: OrderInstruction::BuyToOpen,
                symbol: "SPY   260815P00515000".into(),
                asset_type: "OPTION",
                quantity: 1.0,
            },
        ],
        Some(0.50),
        OrderDuration::Day,
        OrderSession::Normal,
        None,
    )
    .unwrap();
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
        net: NetPrice::Credit(0.50),
        tif: TimeInForce::Day,
        session: Session::Core,
    };
    assert_eq!(schwab_order_json(&intent).unwrap(), legacy);
}

#[test]
fn oco_matches_legacy_builder() {
    let legacy = schwab_trader_oco("AAPL", 10.0, 190.5, 180.0, 179.5, "DAY");
    let intent = OrderIntent::ExitBracket(ExitBracket {
        symbol: "aapl".into(),
        quantity: 10.0,
        take_profit: 190.5,
        stop: 180.0,
        stop_limit: 179.5,
        tif: TimeInForce::Day,
    });
    assert_eq!(schwab_order_json(&intent).unwrap(), legacy);
}

fn schwab_trader_oco(
    symbol: &str,
    quantity: f64,
    profit_limit: f64,
    stop_price: f64,
    stop_limit: f64,
    duration: &str,
) -> Value {
    serde_json::json!({
        "orderStrategyType": "OCO",
        "childOrderStrategies": [
            {
                "orderType": "LIMIT",
                "session": "NORMAL",
                "price": format!("{profit_limit:.2}"),
                "duration": duration,
                "orderStrategyType": "SINGLE",
                "orderLegCollection": [{
                    "instruction": "SELL",
                    "quantity": quantity,
                    "instrument": {
                        "symbol": symbol.trim().to_uppercase(),
                        "assetType": "EQUITY"
                    }
                }]
            },
            {
                "orderType": "STOP_LIMIT",
                "session": "NORMAL",
                "stopPrice": format!("{stop_price:.2}"),
                "price": format!("{stop_limit:.2}"),
                "duration": duration,
                "orderStrategyType": "SINGLE",
                "orderLegCollection": [{
                    "instruction": "SELL",
                    "quantity": quantity,
                    "instrument": {
                        "symbol": symbol.trim().to_uppercase(),
                        "assetType": "EQUITY"
                    }
                }]
            }
        ]
    })
}
