//! Schwab OrderRequest JSON schema and structural validation for agents.

use anyhow::{bail, Context, Result};
use schwab_api::models::order::ComplexOrderStrategyType;
use serde_json::{json, Value};

pub fn order_request_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://schwab-api-cli.local/schemas/order-request.json",
        "title": "Schwab OrderRequest",
        "description": "POST /accounts/{accountNumber}/orders request body (Trader API v1)",
        "type": "object",
        "required": ["orderType", "orderLegCollection"],
        "properties": {
            "session": { "type": "string", "enum": ["NORMAL", "AM", "PM", "SEAMLESS"] },
            "duration": { "type": "string", "enum": ["DAY", "GOOD_TILL_CANCEL", "FILL_OR_KILL"] },
            "orderType": {
                "type": "string",
                "enum": [
                    "MARKET", "LIMIT", "STOP", "STOP_LIMIT", "TRAILING_STOP", "CABINET",
                    "NON_MARKETABLE", "MARKET_ON_CLOSE", "EXERCISE", "TRAILING_STOP_LIMIT",
                    "NET_DEBIT", "NET_CREDIT", "NET_ZERO", "LIMIT_ON_CLOSE"
                ]
            },
            "cancelTime": { "type": "string", "format": "date-time", "description": "ISO-8601 auto-cancel time" },
            "complexOrderStrategyType": {
                "type": "string",
                "enum": enum_strings(ComplexOrderStrategyType::all_values())
            },
            "quantity": { "type": "number" },
            "stopPrice": { "type": "number" },
            "price": { "type": ["number", "string"], "description": "Limit/net debit/net credit price" },
            "taxLotMethod": {
                "type": "string",
                "enum": ["FIFO", "LIFO", "HIGH_COST", "LOW_COST", "AVERAGE_COST", "SPECIFIC_LOT", "LOSS_HARVESTER"]
            },
            "specialInstruction": {
                "type": "string",
                "enum": ["ALL_OR_NONE", "DO_NOT_REDUCE", "ALL_OR_NONE_DO_NOT_REDUCE"]
            },
            "orderStrategyType": {
                "type": "string",
                "enum": ["SINGLE", "CANCEL", "RECALL", "PAIR", "FLATTEN", "TWO_DAY_SWAP", "BLAST_ALL", "OCO", "TRIGGER"]
            },
            "orderLegCollection": {
                "type": "array",
                "minItems": 1,
                "items": { "$ref": "#/$defs/orderLeg" }
            },
            "childOrderStrategies": {
                "type": "array",
                "items": { "$ref": "#" },
                "description": "Nested strategies for OCO/TRIGGER"
            }
        },
        "$defs": {
            "orderLeg": {
                "type": "object",
                "required": ["instruction", "quantity", "instrument"],
                "properties": {
                    "orderLegType": {
                        "type": "string",
                        "enum": ["EQUITY", "OPTION", "INDEX", "MUTUAL_FUND", "CASH_EQUIVALENT", "FIXED_INCOME", "CURRENCY", "COLLECTIVE_INVESTMENT"]
                    },
                    "legId": { "type": "integer" },
                    "instruction": {
                        "type": "string",
                        "enum": [
                            "BUY", "SELL", "BUY_TO_OPEN", "SELL_TO_CLOSE", "SELL_TO_OPEN",
                            "BUY_TO_CLOSE", "SELL_SHORT", "BUY_TO_COVER"
                        ]
                    },
                    "positionEffect": { "type": "string", "enum": ["OPENING", "CLOSING", "AUTOMATIC"] },
                    "quantity": { "type": "number", "exclusiveMinimum": 0 },
                    "instrument": {
                        "type": "object",
                        "required": ["symbol", "assetType"],
                        "properties": {
                            "symbol": { "type": "string" },
                            "assetType": {
                                "type": "string",
                                "enum": ["EQUITY", "OPTION", "INDEX", "MUTUAL_FUND", "CASH_EQUIVALENT", "FIXED_INCOME", "CURRENCY", "COLLECTIVE_INVESTMENT"]
                            }
                        }
                    }
                }
            }
        },
        "examples": [
            {
                "orderType": "LIMIT",
                "session": "NORMAL",
                "duration": "DAY",
                "price": "100.50",
                "orderStrategyType": "SINGLE",
                "complexOrderStrategyType": "NONE",
                "orderLegCollection": [{
                    "instruction": "BUY",
                    "quantity": 10,
                    "instrument": { "symbol": "AAPL", "assetType": "EQUITY" }
                }]
            },
            {
                "orderType": "NET_DEBIT",
                "session": "NORMAL",
                "duration": "DAY",
                "price": "0.50",
                "orderStrategyType": "SINGLE",
                "complexOrderStrategyType": "VERTICAL",
                "orderLegCollection": [
                    {
                        "instruction": "BUY_TO_OPEN",
                        "quantity": 1,
                        "instrument": { "symbol": "AAPL  260620C00180000", "assetType": "OPTION" }
                    },
                    {
                        "instruction": "SELL_TO_OPEN",
                        "quantity": 1,
                        "instrument": { "symbol": "AAPL  260620C00185000", "assetType": "OPTION" }
                    }
                ]
            }
        ]
    })
}

pub fn order_schema_meta() -> Value {
    json!({
        "endpoint": "POST /accounts/{accountNumber}/orders",
        "previewEndpoint": "POST /accounts/{accountNumber}/previewOrder",
        "complexOrderStrategyTypes": enum_strings(ComplexOrderStrategyType::all_values()),
        "notes": [
            "Use complexOrderStrategyType for multi-leg spreads (VERTICAL, IRON_CONDOR, etc.)",
            "Use cancelTime (ISO-8601) for day orders that should auto-cancel",
            "Spreads typically use orderType NET_DEBIT or NET_CREDIT with top-level price",
            "Always run `schwab orders preview` before `schwab orders place` for complex orders",
            "Enable allow_complex_orders and allow_option_orders in safety.json for options/spreads"
        ]
    })
}

pub fn validate_order_shape(order: &Value) -> Result<()> {
    let has_legs = order
        .get("orderLegCollection")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());
    let has_children = order
        .get("childOrderStrategies")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());

    if !has_legs && !has_children {
        bail!("Order must include orderLegCollection or childOrderStrategies");
    }

    if has_legs {
        validate_legs(order.get("orderLegCollection").unwrap())?;
        order
            .get("orderType")
            .and_then(|v| v.as_str())
            .context("Missing orderType on order with legs")?;
    }

    if let Some(children) = order.get("childOrderStrategies").and_then(|v| v.as_array()) {
        for child in children {
            validate_order_shape(child)?;
        }
    }

    if let Some(strategy) = order
        .get("complexOrderStrategyType")
        .and_then(|v| v.as_str())
    {
        let valid: Vec<&str> = enum_strings(ComplexOrderStrategyType::all_values());
        if !valid.contains(&strategy) {
            bail!("Invalid complexOrderStrategyType `{strategy}`");
        }
    }

    if let Some(cancel_time) = order.get("cancelTime").and_then(|v| v.as_str()) {
        if cancel_time.is_empty() {
            bail!("cancelTime must be a non-empty ISO-8601 date-time string");
        }
    }

    if has_legs {
        let order_type = order.get("orderType").and_then(|v| v.as_str()).unwrap_or("");
        match order_type {
            "LIMIT" | "STOP_LIMIT" | "NET_DEBIT" | "NET_CREDIT" | "LIMIT_ON_CLOSE" => {
                if order.get("price").is_none() {
                    bail!("orderType {order_type} requires top-level price");
                }
            }
            "STOP" | "TRAILING_STOP" => {
                if order.get("stopPrice").is_none() && order.get("stopPriceOffset").is_none() {
                    bail!("orderType {order_type} requires stopPrice or stopPriceOffset");
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn validate_legs(legs_value: &Value) -> Result<()> {
    let legs = legs_value
        .as_array()
        .filter(|a| !a.is_empty())
        .context("orderLegCollection must be a non-empty array")?;

    for (idx, leg) in legs.iter().enumerate() {
        leg.get("instruction")
            .and_then(|v| v.as_str())
            .with_context(|| format!("orderLegCollection[{idx}].instruction is required"))?;
        leg.get("quantity")
            .and_then(parse_number)
            .filter(|q| *q > 0.0)
            .with_context(|| format!("orderLegCollection[{idx}].quantity must be positive"))?;
        let instrument = leg
            .get("instrument")
            .with_context(|| format!("orderLegCollection[{idx}].instrument is required"))?;
        instrument
            .get("symbol")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .with_context(|| format!("orderLegCollection[{idx}].instrument.symbol is required"))?;
        instrument
            .get("assetType")
            .and_then(|v| v.as_str())
            .with_context(|| format!("orderLegCollection[{idx}].instrument.assetType is required"))?;
    }

    Ok(())
}

pub fn order_examples() -> Value {
    json!({
        "equityMarketBuy": {
            "description": "Buy 15 shares at market, day order",
            "order": {
                "orderType": "MARKET",
                "session": "NORMAL",
                "duration": "DAY",
                "orderStrategyType": "SINGLE",
                "orderLegCollection": [{
                    "instruction": "BUY",
                    "quantity": 15,
                    "instrument": { "symbol": "XYZ", "assetType": "EQUITY" }
                }]
            }
        },
        "singleOptionLimit": {
            "description": "Buy to open 10 call contracts at limit",
            "order": {
                "complexOrderStrategyType": "NONE",
                "orderType": "LIMIT",
                "session": "NORMAL",
                "price": "6.45",
                "duration": "DAY",
                "orderStrategyType": "SINGLE",
                "orderLegCollection": [{
                    "instruction": "BUY_TO_OPEN",
                    "quantity": 10,
                    "instrument": { "symbol": "XYZ   240315C00500000", "assetType": "OPTION" }
                }]
            }
        },
        "verticalPutSpread": {
            "description": "Vertical put spread — NET_DEBIT (from Schwab docs)",
            "order": {
                "orderType": "NET_DEBIT",
                "session": "NORMAL",
                "price": "0.10",
                "duration": "DAY",
                "orderStrategyType": "SINGLE",
                "orderLegCollection": [
                    {
                        "instruction": "BUY_TO_OPEN",
                        "quantity": 2,
                        "instrument": { "symbol": "XYZ   240315P00045000", "assetType": "OPTION" }
                    },
                    {
                        "instruction": "SELL_TO_OPEN",
                        "quantity": 2,
                        "instrument": { "symbol": "XYZ   240315P00043000", "assetType": "OPTION" }
                    }
                ]
            }
        },
        "triggerSequence": {
            "description": "Buy limit triggers sell limit (1st Trigger Sequence)",
            "order": {
                "orderType": "LIMIT",
                "session": "NORMAL",
                "price": "34.97",
                "duration": "DAY",
                "orderStrategyType": "TRIGGER",
                "orderLegCollection": [{
                    "instruction": "BUY",
                    "quantity": 10,
                    "instrument": { "symbol": "XYZ", "assetType": "EQUITY" }
                }],
                "childOrderStrategies": [{
                    "orderType": "LIMIT",
                    "session": "NORMAL",
                    "price": "42.03",
                    "duration": "DAY",
                    "orderStrategyType": "SINGLE",
                    "orderLegCollection": [{
                        "instruction": "SELL",
                        "quantity": 10,
                        "instrument": { "symbol": "XYZ", "assetType": "EQUITY" }
                    }]
                }]
            }
        },
        "oco": {
            "description": "One-Cancels-Another — limit sell + stop limit sell",
            "order": {
                "orderStrategyType": "OCO",
                "childOrderStrategies": [
                    {
                        "orderType": "LIMIT",
                        "session": "NORMAL",
                        "price": "45.97",
                        "duration": "DAY",
                        "orderStrategyType": "SINGLE",
                        "orderLegCollection": [{
                            "instruction": "SELL",
                            "quantity": 2,
                            "instrument": { "symbol": "XYZ", "assetType": "EQUITY" }
                        }]
                    },
                    {
                        "orderType": "STOP_LIMIT",
                        "session": "NORMAL",
                        "price": "37.00",
                        "stopPrice": "37.03",
                        "duration": "DAY",
                        "orderStrategyType": "SINGLE",
                        "orderLegCollection": [{
                            "instruction": "SELL",
                            "quantity": 2,
                            "instrument": { "symbol": "XYZ", "assetType": "EQUITY" }
                        }]
                    }
                ]
            }
        },
        "trailingStop": {
            "description": "Trailing stop sell with $10 offset",
            "order": {
                "complexOrderStrategyType": "NONE",
                "orderType": "TRAILING_STOP",
                "session": "NORMAL",
                "stopPriceLinkBasis": "BID",
                "stopPriceLinkType": "VALUE",
                "stopPriceOffset": 10,
                "duration": "DAY",
                "orderStrategyType": "SINGLE",
                "orderLegCollection": [{
                    "instruction": "SELL",
                    "quantity": 10,
                    "instrument": { "symbol": "XYZ", "assetType": "EQUITY" }
                }]
            }
        },
        "optionSymbology": {
            "format": "UNDERLYING(6 chars) | YYMMDD | C/P | Strike(8 digits, 5+3)",
            "examples": [
                { "symbol": "XYZ   240315C00500000", "meaning": "XYZ $50 Call exp 2024-03-15" },
                { "symbol": "XYZ   240315P00045000", "meaning": "XYZ $45 Put exp 2024-03-15" }
            ]
        }
    })
}

fn enum_strings(values: &[ComplexOrderStrategyType]) -> Vec<&'static str> {
    values.iter().map(|v| v.as_str()).collect()
}

fn parse_number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .or_else(|| v.as_i64().map(|n| n as f64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schema_includes_complex_strategies() {
        let schema = order_request_schema();
        let strategies = schema["properties"]["complexOrderStrategyType"]["enum"]
            .as_array()
            .unwrap();
        assert!(strategies.iter().any(|v| v == "VERTICAL"));
        assert!(strategies.iter().any(|v| v == "IRON_CONDOR"));
    }

    #[test]
    fn validates_vertical_spread_shape() {
        let order = json!({
            "orderType": "NET_DEBIT",
            "price": "0.50",
            "complexOrderStrategyType": "VERTICAL",
            "orderLegCollection": [
                {
                    "instruction": "BUY_TO_OPEN",
                    "quantity": 1,
                    "instrument": { "symbol": "AAPL  260620C00180000", "assetType": "OPTION" }
                },
                {
                    "instruction": "SELL_TO_OPEN",
                    "quantity": 1,
                    "instrument": { "symbol": "AAPL  260620C00185000", "assetType": "OPTION" }
                }
            ]
        });
        validate_order_shape(&order).unwrap();
    }

    #[test]
    fn validates_oco_without_top_level_legs() {
        let order = order_examples()["oco"]["order"].clone();
        validate_order_shape(&order).unwrap();
    }
}
