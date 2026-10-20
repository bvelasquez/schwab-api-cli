use std::collections::HashMap;

use anyhow::{bail, Result};
use broker_core::{AccountRef, OrderIntent};
use serde::{Deserialize, Serialize};

use crate::paths::safety_path;

#[derive(Debug, Clone)]
pub struct TradingFlags {
    pub yes: bool,
    pub trust: bool,
    pub dry_run: bool,
    pub simulate: bool,
}

pub fn require_trading_approval(flags: &TradingFlags, command: &str) -> Result<()> {
    if flags.dry_run || flags.simulate {
        return Ok(());
    }
    if flags.trust && flags.yes {
        return Ok(());
    }
    bail!(
        "trading `{command}` needs --trust --yes (or --dry-run / --simulate). safety.json still applies"
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SafetyGate {
    pub version: u32,
    pub max_trade_value_usd: f64,
    pub max_shares_per_order: f64,
    pub allow_market_orders: bool,
    pub allow_limit_orders: bool,
    pub allow_option_orders: bool,
    pub allow_complex_orders: bool,
    pub allowed_symbols: Vec<String>,
    pub blocked_symbols: Vec<String>,
    pub max_trade_pct_of_equity: f64,
    pub brokers: HashMap<String, LimitOverride>,
    pub accounts: HashMap<String, LimitOverride>,
    pub public_license_ack: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LimitOverride {
    pub max_trade_value_usd: Option<f64>,
    pub max_shares_per_order: Option<f64>,
    pub allowed_symbols: Option<Vec<String>>,
    pub blocked_symbols: Option<Vec<String>>,
}

impl Default for SafetyGate {
    fn default() -> Self {
        Self {
            version: 1,
            max_trade_value_usd: 5_000.0,
            max_shares_per_order: 100.0,
            allow_market_orders: true,
            allow_limit_orders: true,
            allow_option_orders: false,
            allow_complex_orders: false,
            allowed_symbols: vec![],
            blocked_symbols: vec![],
            max_trade_pct_of_equity: 10.0,
            brokers: HashMap::new(),
            accounts: HashMap::new(),
            public_license_ack: false,
        }
    }
}

impl SafetyGate {
    pub fn load() -> Result<Self> {
        let path = safety_path()?;
        if !path.is_file() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)?;
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        let mut gate: SafetyGate = serde_json::from_value(value.clone()).unwrap_or_default();
        if let Some(limits) = value.get("limits") {
            if let Some(amount) = limits.get("max_trade_value_usd").and_then(|v| v.as_f64()) {
                gate.max_trade_value_usd = amount;
            }
            if let Some(shares) = limits.get("max_shares_per_order").and_then(|v| v.as_f64()) {
                gate.max_shares_per_order = shares;
            }
            if let Some(flag) = limits.get("allow_market_orders").and_then(|v| v.as_bool()) {
                gate.allow_market_orders = flag;
            }
            if let Some(flag) = limits.get("allow_limit_orders").and_then(|v| v.as_bool()) {
                gate.allow_limit_orders = flag;
            }
            if let Some(flag) = limits.get("allow_option_orders").and_then(|v| v.as_bool()) {
                gate.allow_option_orders = flag;
            }
            if let Some(flag) = limits.get("allow_complex_orders").and_then(|v| v.as_bool()) {
                gate.allow_complex_orders = flag;
            }
            if let Some(pct) = limits
                .get("max_trade_pct_of_equity")
                .and_then(|v| v.as_f64())
            {
                gate.max_trade_pct_of_equity = pct;
            }
            gate.allowed_symbols =
                string_list(limits.get("allowed_symbols")).unwrap_or(gate.allowed_symbols);
            gate.blocked_symbols =
                string_list(limits.get("blocked_symbols")).unwrap_or(gate.blocked_symbols);
        }
        gate.normalize();
        Ok(gate)
    }

    fn normalize(&mut self) {
        self.allowed_symbols = upper(&self.allowed_symbols);
        self.blocked_symbols = upper(&self.blocked_symbols);
    }

    pub fn check(&self, account: &AccountRef, intent: &OrderIntent, equity: f64) -> Result<()> {
        if account.broker == broker_core::BrokerKind::Public && !self.public_license_ack {
            bail!(
                "Public trading requires public_license_ack=true in safety.json (personal, non-commercial use)"
            );
        }
        let limits = self.resolved(account);
        match intent {
            OrderIntent::Equity { limit, .. } => {
                if limit.is_none() && !self.allow_market_orders {
                    bail!("market orders are disabled");
                }
                if limit.is_some() && !self.allow_limit_orders {
                    bail!("limit orders are disabled");
                }
            }
            OrderIntent::Spread { .. } => {
                if !self.allow_option_orders || !self.allow_complex_orders {
                    bail!("option spread orders are disabled in safety.json");
                }
            }
            OrderIntent::ExitBracket(_) => {}
        }
        let qty = intent.max_quantity();
        if qty > limits.max_shares {
            bail!(
                "quantity {qty} exceeds max_shares_per_order {}",
                limits.max_shares
            );
        }
        let notional = intent.estimated_notional();
        if notional > limits.max_value && notional > 0.0 {
            bail!(
                "notional {notional:.2} exceeds max_trade_value_usd {}",
                limits.max_value
            );
        }
        if self.max_trade_pct_of_equity > 0.0 && equity > 0.0 && notional > 0.0 {
            let cap = equity * self.max_trade_pct_of_equity / 100.0;
            if notional > cap {
                bail!(
                    "notional {notional:.2} exceeds {cap:.2} ({:.1}% of equity)",
                    self.max_trade_pct_of_equity
                );
            }
        }
        for symbol in intent.symbols() {
            let symbol = symbol.trim().to_uppercase();
            let root = symbol.split_whitespace().next().unwrap_or(&symbol);
            if limits
                .blocked
                .iter()
                .any(|item| item == root || item == &symbol)
            {
                bail!("symbol {root} is blocked");
            }
            if !limits.allowed.is_empty()
                && !limits
                    .allowed
                    .iter()
                    .any(|item| item == root || item == &symbol)
            {
                bail!("symbol {root} is not in allowed_symbols");
            }
        }
        Ok(())
    }

    fn resolved(&self, account: &AccountRef) -> Resolved {
        let mut max_value = self.max_trade_value_usd;
        let mut max_shares = self.max_shares_per_order;
        let mut allowed = self.allowed_symbols.clone();
        let mut blocked = self.blocked_symbols.clone();
        if let Some(over) = self.brokers.get(account.broker.as_str()) {
            apply(
                &mut max_value,
                &mut max_shares,
                &mut allowed,
                &mut blocked,
                over,
            );
        }
        if let Some(over) = self.accounts.get(&account.address()) {
            apply(
                &mut max_value,
                &mut max_shares,
                &mut allowed,
                &mut blocked,
                over,
            );
        }
        Resolved {
            max_value,
            max_shares,
            allowed,
            blocked,
        }
    }
}

struct Resolved {
    max_value: f64,
    max_shares: f64,
    allowed: Vec<String>,
    blocked: Vec<String>,
}

fn apply(
    max_value: &mut f64,
    max_shares: &mut f64,
    allowed: &mut Vec<String>,
    blocked: &mut Vec<String>,
    over: &LimitOverride,
) {
    if let Some(value) = over.max_trade_value_usd {
        *max_value = value;
    }
    if let Some(value) = over.max_shares_per_order {
        *max_shares = value;
    }
    if let Some(symbols) = &over.allowed_symbols {
        *allowed = upper(symbols);
    }
    if let Some(symbols) = &over.blocked_symbols {
        *blocked = upper(symbols);
    }
}

fn string_list(value: Option<&serde_json::Value>) -> Option<Vec<String>> {
    value.and_then(|value| value.as_array()).map(|rows| {
        rows.iter()
            .filter_map(|row| row.as_str().map(str::to_string))
            .collect()
    })
}

fn upper(symbols: &[String]) -> Vec<String> {
    symbols
        .iter()
        .map(|symbol| symbol.trim().to_uppercase())
        .filter(|symbol| !symbol.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use broker_core::{BrokerKind, EquitySize, Session, Side, TimeInForce};

    #[test]
    fn blocks_oversized_equity_order() {
        let gate = SafetyGate {
            max_trade_value_usd: 100.0,
            ..SafetyGate::default()
        };
        let account = AccountRef::new(BrokerKind::Schwab, "HASH");
        let intent = OrderIntent::Equity {
            side: Side::Buy,
            symbol: "AAPL".into(),
            size: EquitySize::Shares(10.0),
            limit: Some(50.0),
            tif: TimeInForce::Day,
            session: Session::Core,
            bracket: None,
        };
        assert!(gate.check(&account, &intent, 10_000.0).is_err());
    }
}
