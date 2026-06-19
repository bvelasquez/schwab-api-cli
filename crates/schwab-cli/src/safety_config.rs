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
pub struct ParsedOrder {
    pub instruction: String,
    pub order_type: String,
    pub symbol: String,
    pub asset_type: String,
    pub quantity: f64,
    pub limit_price: Option<f64>,
}

pub fn parse_order(order: &Value) -> Result<ParsedOrder> {
    let legs = order
        .get("orderLegCollection")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .context("orderLegCollection must contain at least one leg")?;

    if legs.len() > 1 {
        anyhow::bail!("Multi-leg and complex orders are not supported by safety validation yet");
    }

    let leg = &legs[0];
    let instruction = leg
        .get("instruction")
        .and_then(|v| v.as_str())
        .context("Missing orderLegCollection[0].instruction")?
        .to_uppercase();
    let quantity = leg
        .get("quantity")
        .and_then(parse_number)
        .context("Missing or invalid orderLegCollection[0].quantity")?;
    let instrument = leg
        .get("instrument")
        .context("Missing orderLegCollection[0].instrument")?;
    let symbol = instrument
        .get("symbol")
        .and_then(|v| v.as_str())
        .context("Missing instrument.symbol")?
        .to_uppercase();
    let asset_type = instrument
        .get("assetType")
        .and_then(|v| v.as_str())
        .unwrap_or("EQUITY")
        .to_uppercase();

    let order_type = order
        .get("orderType")
        .and_then(|v| v.as_str())
        .context("Missing orderType")?
        .to_uppercase();

    let limit_price = order.get("price").and_then(parse_number);

    Ok(ParsedOrder {
        instruction,
        order_type,
        symbol,
        asset_type,
        quantity,
        limit_price,
    })
}

pub fn estimate_notional(parsed: &ParsedOrder, preview: Option<&Value>) -> Option<f64> {
    if let Some(price) = parsed.limit_price {
        return Some(parsed.quantity * price);
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
    let parsed = parse_order(order)?;
    let limits = &config.limits;

    if !limits.allowed_instructions.contains(&parsed.instruction) {
        anyhow::bail!(
            "Instruction `{}` is not allowed (allowed: {:?})",
            parsed.instruction,
            limits.allowed_instructions
        );
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
        "LIMIT" if !limits.allow_limit_orders => {
            anyhow::bail!("LIMIT orders are disabled in safety.json");
        }
        _ => {}
    }

    if parsed.instruction.contains("SHORT") && !limits.allow_short_sales {
        anyhow::bail!("Short sales are disabled in safety.json");
    }

    if parsed.asset_type != "EQUITY" && !limits.allow_option_orders {
        anyhow::bail!(
            "Asset type `{}` is not allowed (allow_option_orders=false)",
            parsed.asset_type
        );
    }

    if limits.blocked_symbols.contains(&parsed.symbol) {
        anyhow::bail!("Symbol `{}` is blocked in safety.json", parsed.symbol);
    }

    if !limits.allowed_symbols.is_empty() && !limits.allowed_symbols.contains(&parsed.symbol) {
        anyhow::bail!(
            "Symbol `{}` is not in the allowed_symbols whitelist",
            parsed.symbol
        );
    }

    if parsed.quantity > limits.max_shares_per_order {
        anyhow::bail!(
            "Quantity {} exceeds max_shares_per_order ({})",
            parsed.quantity,
            limits.max_shares_per_order
        );
    }

    if let Some(notional) = estimate_notional(&parsed, preview) {
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
    } else if limits.max_trade_value_usd > 0.0 || limits.max_trade_pct_of_equity > 0.0 {
        anyhow::bail!(
            "Could not estimate order notional; provide a limit price or preview data"
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
}
