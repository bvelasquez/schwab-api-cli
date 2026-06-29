use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::rules::TraderRules;

#[derive(Debug, Clone, Deserialize)]
struct OptionsAgentState {
    #[serde(default)]
    open_positions: std::collections::HashMap<String, OptionsTrackedPosition>,
    #[serde(default)]
    pending_orders: Vec<OptionsPendingOrder>,
}

#[derive(Debug, Clone, Deserialize)]
struct OptionsTrackedPosition {
    #[serde(default)]
    max_loss_usd: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct OptionsPendingOrder {
    #[serde(default)]
    reserved_risk_usd: f64,
    action: Option<String>,
}

pub struct OptionsReserve {
    pub reserved_risk_usd: f64,
    pub source: String,
    pub state_path: Option<String>,
}

pub fn load_options_reserve(rules: &TraderRules) -> OptionsReserve {
    let cfg = &rules.capital.options_risk;
    let rules_path = Path::new(&cfg.rules_file);
    if cfg.rules_file.is_empty() || !rules_path.is_file() {
        return OptionsReserve {
            reserved_risk_usd: cfg.fallback_reserve_usd,
            source: "fallback".into(),
            state_path: None,
        };
    }

    let state_path = schwab_cli::agent::paths::default_state_path(rules_path);
    match load_reserved_from_state(&state_path) {
        Ok(reserved) => OptionsReserve {
            reserved_risk_usd: reserved,
            source: "agent_state".into(),
            state_path: Some(state_path.display().to_string()),
        },
        Err(_) => OptionsReserve {
            reserved_risk_usd: cfg.fallback_reserve_usd,
            source: "fallback_unreadable_state".into(),
            state_path: Some(state_path.display().to_string()),
        },
    }
}

fn load_reserved_from_state(path: &Path) -> Result<f64> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read options state {}", path.display()))?;
    let state: OptionsAgentState = serde_json::from_str(&raw)?;
    Ok(compute_reserved(&state))
}

fn compute_reserved(state: &OptionsAgentState) -> f64 {
    let open: f64 = state
        .open_positions
        .values()
        .map(|p| p.max_loss_usd.max(0.0))
        .sum();
    let pending: f64 = state
        .pending_orders
        .iter()
        .filter(|p| p.action.as_deref() != Some("exit"))
        .map(|p| p.reserved_risk_usd.max(0.0))
        .sum();
    open + pending
}

pub fn options_buffer_usd(rules: &TraderRules, reserved: f64) -> f64 {
    reserved * (1.0 + rules.capital.options_risk.buffer_pct / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_sums_open_and_pending() {
        let state = OptionsAgentState {
            open_positions: [(
                "SPY|2026-07-18".into(),
                OptionsTrackedPosition {
                    max_loss_usd: 170.0,
                },
            )]
            .into(),
            pending_orders: vec![OptionsPendingOrder {
                reserved_risk_usd: 175.0,
                action: Some("entry".into()),
            }],
        };
        assert!((compute_reserved(&state) - 345.0).abs() < 0.01);
    }
}
