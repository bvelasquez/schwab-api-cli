use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Hard trading limits and agent guidance loaded from `safety.json`.
/// The CLI enforces `limits` regardless of LLM instructions or `--trust`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetyConfig {
    pub version: u32,
    /// Free-form rules surfaced to agents via `schwab instructions --json`.
    pub agent_rules: Vec<String>,
    pub limits: SafetyLimits,
    /// When true, `orders place` / `trade buy|sell` must preview before submitting.
    pub require_preview_before_place: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetyLimits {
    /// Maximum estimated notional (USD) for a single order.
    pub max_trade_value_usd: f64,
    /// Maximum shares (or contracts) per order leg.
    pub max_shares_per_order: f64,
    /// Allow MARKET orders.
    pub allow_market_orders: bool,
    /// Allow LIMIT orders.
    pub allow_limit_orders: bool,
    /// Allow short-sale instructions (SELL_SHORT, etc.).
    pub allow_short_sales: bool,
    /// Allow non-equity asset types (OPTION, etc.).
    pub allow_option_orders: bool,
    /// Allow multi-leg / complex orders (spreads, iron condors, etc.).
    pub allow_complex_orders: bool,
    /// Allow conditional orders (OCO, TRIGGER, etc.).
    pub allow_conditional_orders: bool,
    /// Max legs per order (complex spreads).
    pub max_legs_per_order: u32,
    /// If non-empty, only these complexOrderStrategyType values are allowed.
    pub allowed_complex_strategies: Vec<String>,
    /// Permitted instructions (e.g. BUY, SELL). Empty = none allowed.
    pub allowed_instructions: Vec<String>,
    /// Permitted order types (e.g. MARKET, LIMIT). Empty = none allowed.
    pub allowed_order_types: Vec<String>,
    /// If non-empty, only these symbols may be traded (uppercase).
    pub allowed_symbols: Vec<String>,
    /// Symbols that may never be traded (uppercase).
    pub blocked_symbols: Vec<String>,
    /// Max estimated order value as % of account equity (0 = disabled).
    pub max_trade_pct_of_equity: f64,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            version: 1,
            agent_rules: vec![
                "Always run `schwab portfolio summary --json` before rebalancing.".into(),
                "Preview every order before placement unless dry-run.".into(),
                "Respect safety.json limits; the CLI will reject orders that exceed them.".into(),
                "Never enable --trust unless the user explicitly requests autonomous trading.".into(),
            ],
            limits: SafetyLimits::default(),
            require_preview_before_place: true,
        }
    }
}

impl Default for SafetyLimits {
    fn default() -> Self {
        Self {
            max_trade_value_usd: 5_000.0,
            max_shares_per_order: 100.0,
            allow_market_orders: true,
            allow_limit_orders: true,
            allow_short_sales: false,
            allow_option_orders: false,
            allow_complex_orders: false,
            allow_conditional_orders: false,
            max_legs_per_order: 4,
            allowed_complex_strategies: vec![],
            allowed_instructions: vec!["BUY".into(), "SELL".into()],
            allowed_order_types: vec!["MARKET".into(), "LIMIT".into()],
            allowed_symbols: vec![],
            blocked_symbols: vec![],
            max_trade_pct_of_equity: 10.0,
        }
    }
}

impl SafetyConfig {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if path.is_file() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read safety config at {}", path.display()))?;
            let mut cfg: SafetyConfig = serde_json::from_str(&raw)
                .with_context(|| format!("Invalid safety.json at {}", path.display()))?;
            cfg.normalize();
            Ok(cfg)
        } else {
            Ok(Self::default())
        }
    }

    pub fn init_at_default_path() -> Result<PathBuf> {
        let path = config_path();
        if path.is_file() {
            return Ok(path);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config dir {}", parent.display()))?;
        }
        let cfg = Self::default();
        let pretty = serde_json::to_string_pretty(&cfg)?;
        fs::write(&path, pretty)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(path)
    }

    fn normalize(&mut self) {
        self.limits.allowed_instructions = self
            .limits
            .allowed_instructions
            .iter()
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
        self.limits.allowed_order_types = self
            .limits
            .allowed_order_types
            .iter()
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
        self.limits.allowed_symbols = self
            .limits
            .allowed_symbols
            .iter()
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
        self.limits.allowed_complex_strategies = self
            .limits
            .allowed_complex_strategies
            .iter()
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
        self.limits.blocked_symbols = self
            .limits
            .blocked_symbols
            .iter()
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
    }
}

pub fn config_path() -> PathBuf {
    std::env::var("SCHWAB_SAFETY_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_config_path())
}

fn default_config_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "schwabinvestbot")
        .map(|dirs| dirs.config_dir().join("safety.json"))
        .unwrap_or_else(|| PathBuf::from(".schwabinvestbot/safety.json"))
}

/// Parsed fields extracted from a Schwab order JSON payload.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedOrderLeg {
    pub instruction: String,
    pub symbol: String,
    pub asset_type: String,
    pub quantity: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedOrder {
    pub order_type: String,
    pub order_strategy_type: Option<String>,
    pub complex_order_strategy_type: Option<String>,
    pub limit_price: Option<f64>,
    pub legs: Vec<ParsedOrderLeg>,
}

pub fn parse_order(order: &Value) -> Result<ParsedOrder> {
    let legs_raw = order
        .get("orderLegCollection")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .context("orderLegCollection must contain at least one leg")?;

    let mut legs = Vec::with_capacity(legs_raw.len());
    for (idx, leg) in legs_raw.iter().enumerate() {
        let instruction = leg
            .get("instruction")
            .and_then(|v| v.as_str())
            .with_context(|| format!("Missing orderLegCollection[{idx}].instruction"))?
            .to_uppercase();
        let quantity = leg
            .get("quantity")
            .and_then(parse_number)
            .with_context(|| format!("Missing or invalid orderLegCollection[{idx}].quantity"))?;
        let instrument = leg
            .get("instrument")
            .with_context(|| format!("Missing orderLegCollection[{idx}].instrument"))?;
        let symbol = instrument
            .get("symbol")
            .and_then(|v| v.as_str())
            .with_context(|| format!("Missing orderLegCollection[{idx}].instrument.symbol"))?
            .to_uppercase();
        let asset_type = instrument
            .get("assetType")
            .and_then(|v| v.as_str())
            .unwrap_or("EQUITY")
            .to_uppercase();

        legs.push(ParsedOrderLeg {
            instruction,
            symbol,
            asset_type,
            quantity,
        });
    }

    let order_type = order
        .get("orderType")
        .and_then(|v| v.as_str())
        .context("Missing orderType")?
        .to_uppercase();

    let order_strategy_type = order
        .get("orderStrategyType")
        .and_then(|v| v.as_str())
        .map(str::to_uppercase);

    let complex_order_strategy_type = order
        .get("complexOrderStrategyType")
        .and_then(|v| v.as_str())
        .map(str::to_uppercase);

    let limit_price = order.get("price").and_then(parse_number);

    Ok(ParsedOrder {
        order_type,
        order_strategy_type,
        complex_order_strategy_type,
        limit_price,
        legs,
    })
}

fn is_complex_order(parsed: &ParsedOrder) -> bool {
    if parsed.legs.len() > 1 {
        return true;
    }
    parsed
        .complex_order_strategy_type
        .as_deref()
        .is_some_and(|s| s != "NONE")
}

fn underlying_symbol(symbol: &str, asset_type: &str) -> String {
    if asset_type == "OPTION" {
        symbol.split_whitespace().next().unwrap_or(symbol).to_string()
    } else {
        symbol.to_string()
    }
}

pub fn estimate_notional(parsed: &ParsedOrder, preview: Option<&Value>) -> Option<f64> {
    let net_types = ["NET_DEBIT", "NET_CREDIT", "NET_ZERO"];
    if net_types.contains(&parsed.order_type.as_str()) {
        if let Some(price) = parsed.limit_price {
            let contracts = parsed
                .legs
                .iter()
                .map(|leg| leg.quantity)
                .fold(0.0_f64, f64::max);
            return Some(price.abs() * contracts * 100.0);
        }
    }

    if let Some(price) = parsed.limit_price {
        let total_qty: f64 = parsed.legs.iter().map(|leg| leg.quantity).sum();
        return Some(total_qty * price);
    }

    if parsed.order_type == "MARKET" && parsed.legs.len() == 1 {
        return None;
    }

    if let Some(preview) = preview {
        for key in [
            "orderValue",
            "estimatedTotalAmount",
            "totalOrderAmount",
            "netAmount",
            "totalCost",
        ] {
            if let Some(v) = find_f64_by_key(preview, key) {
                return Some(v.abs());
            }
        }
        if let Some(strategy) = preview.get("orderStrategy") {
            for key in ["orderValue", "estimatedTotalAmount", "totalOrderAmount"] {
                if let Some(v) = find_f64_by_key(strategy, key) {
                    return Some(v.abs());
                }
            }
        }
    }

    None
}

pub fn validate_order(
    config: &SafetyConfig,
    order: &Value,
    preview: Option<&Value>,
    account_equity: Option<f64>,
) -> Result<()> {
    validate_order_tree(config, order, preview, account_equity)
}

fn validate_order_tree(
    config: &SafetyConfig,
    order: &Value,
    preview: Option<&Value>,
    account_equity: Option<f64>,
) -> Result<()> {
    let has_legs = order
        .get("orderLegCollection")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());

    if has_legs {
        let parsed = parse_order(order)?;
        validate_parsed_order(config, &parsed, preview, account_equity)?;
    }

    let strategy = order
        .get("orderStrategyType")
        .and_then(|v| v.as_str())
        .unwrap_or("SINGLE")
        .to_uppercase();

    if matches!(strategy.as_str(), "OCO" | "TRIGGER" | "BLAST_ALL")
        && !config.limits.allow_conditional_orders
    {
        anyhow::bail!(
            "orderStrategyType `{strategy}` requires allow_conditional_orders=true in safety.json"
        );
    }

    if let Some(children) = order.get("childOrderStrategies").and_then(|v| v.as_array()) {
        for child in children {
            validate_order_tree(config, child, preview, account_equity)?;
        }
    }

    Ok(())
}

fn validate_parsed_order(
    config: &SafetyConfig,
    parsed: &ParsedOrder,
    preview: Option<&Value>,
    account_equity: Option<f64>,
) -> Result<()> {
    let limits = &config.limits;

    if parsed.legs.len() as u32 > limits.max_legs_per_order {
        anyhow::bail!(
            "Order has {} legs; max_legs_per_order is {}",
            parsed.legs.len(),
            limits.max_legs_per_order
        );
    }

    if is_complex_order(parsed) && !limits.allow_complex_orders {
        anyhow::bail!("Multi-leg/complex orders require allow_complex_orders=true in safety.json");
    }

    if let Some(strategy) = &parsed.complex_order_strategy_type {
        if !limits.allowed_complex_strategies.is_empty()
            && !limits.allowed_complex_strategies.contains(strategy)
        {
            anyhow::bail!(
                "complexOrderStrategyType `{strategy}` is not in allowed_complex_strategies"
            );
        }
    }

    if !limits.allowed_order_types.contains(&parsed.order_type) {
        anyhow::bail!(
            "Order type `{}` is not allowed (allowed: {:?})",
            parsed.order_type,
            limits.allowed_order_types
        );
    }

    match parsed.order_type.as_str() {
        "MARKET" if !limits.allow_market_orders => {
            anyhow::bail!("MARKET orders are disabled in safety.json");
        }
        "LIMIT" | "NET_DEBIT" | "NET_CREDIT" | "LIMIT_ON_CLOSE" if !limits.allow_limit_orders => {
            anyhow::bail!("Limit-style orders are disabled in safety.json");
        }
        _ => {}
    }

    for leg in &parsed.legs {
        if !limits.allowed_instructions.contains(&leg.instruction) {
            anyhow::bail!(
                "Instruction `{}` is not allowed (allowed: {:?})",
                leg.instruction,
                limits.allowed_instructions
            );
        }

        if leg.instruction.contains("SHORT") && !limits.allow_short_sales {
            anyhow::bail!("Short sales are disabled in safety.json");
        }

        if leg.asset_type != "EQUITY" && !limits.allow_option_orders {
            anyhow::bail!(
                "Asset type `{}` is not allowed (allow_option_orders=false)",
                leg.asset_type
            );
        }

        let check_symbol = underlying_symbol(&leg.symbol, &leg.asset_type);
        if limits.blocked_symbols.contains(&check_symbol) {
            anyhow::bail!("Symbol `{check_symbol}` is blocked in safety.json");
        }

        if !limits.allowed_symbols.is_empty() && !limits.allowed_symbols.contains(&check_symbol) {
            anyhow::bail!(
                "Symbol `{check_symbol}` is not in the allowed_symbols whitelist"
            );
        }

        if leg.quantity > limits.max_shares_per_order {
            anyhow::bail!(
                "Leg quantity {} exceeds max_shares_per_order ({})",
                leg.quantity,
                limits.max_shares_per_order
            );
        }
    }

    if let Some(notional) = estimate_notional(parsed, preview) {
        if notional > limits.max_trade_value_usd {
            anyhow::bail!(
                "Estimated order value ${notional:.2} exceeds max_trade_value_usd ({})",
                limits.max_trade_value_usd
            );
        }

        if limits.max_trade_pct_of_equity > 0.0 {
            if let Some(equity) = account_equity {
                if equity > 0.0 {
                    let pct = (notional / equity) * 100.0;
                    if pct > limits.max_trade_pct_of_equity {
                        anyhow::bail!(
                            "Order is {pct:.1}% of account equity; max_trade_pct_of_equity is {}",
                            limits.max_trade_pct_of_equity
                        );
                    }
                }
            }
        }
    } else if (limits.max_trade_value_usd > 0.0 || limits.max_trade_pct_of_equity > 0.0)
        && parsed.order_type != "MARKET"
    {
        anyhow::bail!(
            "Could not estimate order notional; provide a limit/net price or preview data"
        );
    }

    Ok(())
}

fn parse_number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .or_else(|| v.as_i64().map(|n| n as f64))
}

fn find_f64_by_key(value: &Value, key: &str) -> Option<f64> {
    match value {
        Value::Object(map) => {
            if let Some(v) = map.get(key).and_then(parse_number) {
                return Some(v);
            }
            for child in map.values() {
                if let Some(v) = find_f64_by_key(child, key) {
                    return Some(v);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(|item| find_f64_by_key(item, key)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_limits_are_conservative() {
        let cfg = SafetyConfig::default();
        assert_eq!(cfg.limits.max_trade_value_usd, 5_000.0);
        assert!(!cfg.limits.allow_short_sales);
    }

    #[test]
    fn validate_blocks_excess_shares() {
        let cfg = SafetyConfig::default();
        let order = json!({
            "orderType": "MARKET",
            "orderLegCollection": [{
                "instruction": "BUY",
                "quantity": 500,
                "instrument": { "symbol": "AAPL", "assetType": "EQUITY" }
            }]
        });
        let err = validate_order(&cfg, &order, None, None).unwrap_err();
        assert!(err.to_string().contains("max_shares_per_order"));
    }

    #[test]
    fn validate_blocks_symbol() {
        let mut cfg = SafetyConfig::default();
        cfg.limits.blocked_symbols = vec!["TSLA".into()];
        let order = json!({
            "orderType": "LIMIT",
            "price": "100",
            "orderLegCollection": [{
                "instruction": "BUY",
                "quantity": 1,
                "instrument": { "symbol": "TSLA", "assetType": "EQUITY" }
            }]
        });
        let err = validate_order(&cfg, &order, None, Some(100_000.0)).unwrap_err();
        assert!(err.to_string().contains("blocked"));
    }

    #[test]
    fn limit_notional_from_price() {
        let cfg = SafetyConfig::default();
        let order = json!({
            "orderType": "LIMIT",
            "price": "600",
            "orderLegCollection": [{
                "instruction": "BUY",
                "quantity": 10,
                "instrument": { "symbol": "AAPL", "assetType": "EQUITY" }
            }]
        });
        let err = validate_order(&cfg, &order, None, None).unwrap_err();
        assert!(err.to_string().contains("max_trade_value_usd"));
    }

    #[test]
    fn blocks_multi_leg_without_complex_flag() {
        let mut cfg = SafetyConfig::default();
        cfg.limits.allow_option_orders = true;
        cfg.limits.allowed_instructions = vec![
            "BUY_TO_OPEN".into(),
            "SELL_TO_OPEN".into(),
        ];
        cfg.limits.allowed_order_types = vec!["NET_DEBIT".into()];
        let order = json!({
            "orderType": "NET_DEBIT",
            "price": "0.10",
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
        });
        let err = validate_order(&cfg, &order, None, None).unwrap_err();
        assert!(err.to_string().contains("allow_complex_orders"));
    }

    #[test]
    fn allows_vertical_spread_when_enabled() {
        let mut cfg = SafetyConfig::default();
        cfg.limits.allow_option_orders = true;
        cfg.limits.allow_complex_orders = true;
        cfg.limits.allowed_instructions = vec![
            "BUY_TO_OPEN".into(),
            "SELL_TO_OPEN".into(),
        ];
        cfg.limits.allowed_order_types = vec!["NET_DEBIT".into()];
        let order = json!({
            "orderType": "NET_DEBIT",
            "price": "0.10",
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
        });
        validate_order(&cfg, &order, None, None).unwrap();
    }
}
