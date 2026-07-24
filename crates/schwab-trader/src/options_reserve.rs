use std::path::{Path, PathBuf};

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
    /// Every options state file that contributed (or was attempted).
    pub state_paths: Vec<String>,
}

pub fn load_options_reserve(rules: &TraderRules) -> OptionsReserve {
    let cfg = &rules.capital.options_risk;
    let paths = cfg.all_rules_files();
    if paths.is_empty() {
        return OptionsReserve {
            reserved_risk_usd: cfg.fallback_reserve_usd,
            source: "fallback".into(),
            state_path: None,
            state_paths: vec![],
        };
    }

    let mut total = 0.0;
    let mut sources = Vec::new();
    let mut state_paths = Vec::new();
    let mut any_ok = false;

    for rel in &paths {
        let rules_path = resolve_rules_path(rel);
        if !rules_path.is_file() {
            sources.push(format!("{rel}:missing_rules"));
            continue;
        }
        let state_path = schwab_cli::agent::paths::default_state_path(&rules_path);
        state_paths.push(state_path.display().to_string());
        match load_reserved_from_state(&state_path) {
            Ok(reserved) => {
                total += reserved;
                any_ok = true;
                sources.push(format!(
                    "{}:agent_state({:.0})",
                    rules_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(rel),
                    reserved
                ));
            }
            Err(_) => {
                sources.push(format!(
                    "{}:unreadable_state",
                    rules_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(rel)
                ));
            }
        }
    }

    if !any_ok {
        return OptionsReserve {
            reserved_risk_usd: cfg.fallback_reserve_usd,
            source: format!("fallback_unreadable ({})", sources.join("; ")),
            state_path: state_paths.first().cloned(),
            state_paths,
        };
    }

    OptionsReserve {
        reserved_risk_usd: total,
        source: sources.join(" + "),
        state_path: state_paths.first().cloned(),
        state_paths,
    }
}

fn resolve_rules_path(rel: &str) -> PathBuf {
    let p = Path::new(rel);
    if p.is_file() {
        return p.to_path_buf();
    }
    // Relative to cwd (repo root when launched from project).
    p.to_path_buf()
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
    use crate::rules::OptionsRiskConfig;

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

    #[test]
    fn all_rules_files_merges_singular_and_plural() {
        let cfg = OptionsRiskConfig {
            rules_files: vec!["rules/a.yaml".into(), "rules/b.yaml".into()],
            rules_file: "rules/a.yaml".into(), // duplicate singular ignored
            ..Default::default()
        };
        assert_eq!(cfg.all_rules_files().len(), 2);

        let legacy = OptionsRiskConfig {
            rules_file: "rules/only.yaml".into(),
            ..Default::default()
        };
        assert_eq!(legacy.all_rules_files(), vec!["rules/only.yaml".to_string()]);
    }
}
