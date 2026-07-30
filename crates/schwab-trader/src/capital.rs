use anyhow::Result;
use schwab_api::TraderApi;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

use crate::agent::state::TraderState;
use crate::options_reserve::options_buffer_usd;
use crate::rules::TraderRules;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapitalCheck {
    pub cash_available: f64,
    pub options_reserved_usd: f64,
    pub options_buffer_usd: f64,
    pub options_reserve_source: String,
    pub min_cash_floor_usd: f64,
    pub free_cash_usd: f64,
    pub max_pct_of_free_cash: f64,
    pub pct_budget_usd: f64,
    pub fixed_cap_usd: f64,
    pub equity_deployed_usd: f64,
    pub pending_buy_usd: f64,
    pub cap_remaining_usd: f64,
    pub tradable_budget_usd: f64,
    pub estimated_cost_usd: Option<f64>,
    pub stop_risk_usd: Option<f64>,
    pub open_equity_risk_usd: f64,
    #[serde(default)]
    pub sibling_deployed_usd: f64,
    /// Open + pending stop risk as % of fixed sleeve cap.
    #[serde(default)]
    pub portfolio_heat_pct: f64,
    /// Remaining heat budget before `heat_ceiling_pct`.
    #[serde(default)]
    pub heat_headroom_pct: f64,
    #[serde(default)]
    pub heat_ceiling_pct: f64,
    #[serde(default)]
    pub positions_open: u32,
    pub passed: bool,
    #[serde(default)]
    pub reject_reason: Option<String>,
}

pub async fn compute_capital_check(
    api: &Arc<TraderApi>,
    rules: &TraderRules,
    state: &TraderState,
    account_hash: &str,
    pending_cost: Option<f64>,
    pending_stop_risk: Option<f64>,
    simulate: bool,
    rules_path: Option<&Path>,
) -> Result<CapitalCheck> {
    let bp = if simulate {
        let ledger = state.sim.as_ref();
        let cash = ledger.map(|l| l.cash_usd).unwrap_or_else(|| {
            rules
                .simulation
                .as_ref()
                .map(|s| s.starting_cash_usd)
                .unwrap_or(rules.capital.fixed_sleeve_cap_usd)
        });
        schwab_cli::portfolio::BuyingPower {
            cash_available_for_trading: cash,
            cash_balance: cash,
            option_buying_power: None,
            liquidation_value: Some(cash),
        }
    } else {
        schwab_cli::portfolio::account_buying_power(api, account_hash).await?
    };
    let cash_available = bp.cash_available_for_trading;

    let opt = if simulate {
        crate::options_reserve::OptionsReserve {
            reserved_risk_usd: 0.0,
            source: "simulation".into(),
            state_path: None,
            state_paths: vec![],
        }
    } else {
        crate::options_reserve::load_options_reserve(rules)
    };
    let options_buffer = options_buffer_usd(rules, opt.reserved_risk_usd);

    let min_floor = rules.capital.min_cash_floor_usd;
    let free_cash = (cash_available - options_buffer - min_floor).max(0.0);
    let pct_budget = free_cash * (rules.capital.max_pct_of_free_cash / 100.0);

    let equity_deployed = state.equity_deployed_usd();
    let pending_buy = state.pending_buy_usd();
    let sibling_deployed = rules_path
        .map(|p| crate::risk::sibling_sleeve_deployed(p, rules))
        .unwrap_or(0.0);
    let cap_remaining = (rules.capital.fixed_sleeve_cap_usd
        - equity_deployed
        - pending_buy
        - sibling_deployed)
        .max(0.0);

    let tradable_budget = if simulate {
        state
            .sim
            .as_ref()
            .map(|l| crate::sim::sim_tradable_budget(l, rules, equity_deployed))
            .unwrap_or_else(|| pct_budget.min(cap_remaining))
    } else {
        pct_budget.min(cap_remaining)
    };

    let open_equity_risk = state.open_stop_risk_usd();
    let heat = portfolio_heat_metrics(rules, state, open_equity_risk, pending_stop_risk);
    let mut check = CapitalCheck {
        cash_available,
        options_reserved_usd: opt.reserved_risk_usd,
        options_buffer_usd: options_buffer,
        options_reserve_source: opt.source,
        min_cash_floor_usd: min_floor,
        free_cash_usd: free_cash,
        max_pct_of_free_cash: rules.capital.max_pct_of_free_cash,
        pct_budget_usd: pct_budget,
        fixed_cap_usd: rules.capital.fixed_sleeve_cap_usd,
        equity_deployed_usd: equity_deployed,
        pending_buy_usd: pending_buy,
        cap_remaining_usd: cap_remaining,
        tradable_budget_usd: tradable_budget,
        estimated_cost_usd: pending_cost,
        stop_risk_usd: pending_stop_risk,
        open_equity_risk_usd: open_equity_risk,
        sibling_deployed_usd: sibling_deployed,
        portfolio_heat_pct: heat.portfolio_heat_pct,
        heat_headroom_pct: heat.heat_headroom_pct,
        heat_ceiling_pct: heat.heat_ceiling_pct,
        positions_open: heat.positions_open,
        passed: true,
        reject_reason: None,
    };

    if tradable_budget <= 0.0 {
        check.passed = false;
        check.reject_reason = Some("tradable_budget is zero".into());
        return Ok(check);
    }

    if let Some(cost) = pending_cost {
        if cost > tradable_budget {
            check.passed = false;
            check.reject_reason = Some(format!(
                "estimated cost ${cost:.2} exceeds tradable budget ${tradable_budget:.2}"
            ));
        }
        if cost > bp.cash_available_for_trading {
            check.passed = false;
            check.reject_reason = Some(format!(
                "estimated cost ${cost:.2} exceeds Schwab cash available ${:.2}",
                bp.cash_available_for_trading
            ));
        }
    }

    if let Some(stop_risk) = pending_stop_risk {
        let heat_limit =
            rules.capital.fixed_sleeve_cap_usd * rules.risk.max_portfolio_heat_pct / 100.0;
        if open_equity_risk + stop_risk > heat_limit {
            check.passed = false;
            check.reject_reason = Some(format!(
                "portfolio heat ${:.2} would exceed limit ${heat_limit:.2}",
                open_equity_risk + stop_risk
            ));
        }
    }

    if let Some(reason) = crate::risk::drawdown_halt_reason(state, rules) {
        check.passed = false;
        check.reject_reason = Some(reason);
    }

    Ok(check)
}

#[derive(Debug, Clone, Copy)]
pub struct PortfolioHeatMetrics {
    pub portfolio_heat_pct: f64,
    pub heat_headroom_pct: f64,
    pub heat_ceiling_pct: f64,
    pub positions_open: u32,
}

pub fn portfolio_heat_metrics(
    rules: &TraderRules,
    state: &TraderState,
    open_equity_risk_usd: f64,
    pending_stop_risk: Option<f64>,
) -> PortfolioHeatMetrics {
    let ceiling_pct = rules.risk.max_portfolio_heat_pct;
    let sleeve = rules.capital.fixed_sleeve_cap_usd;
    let heat_usd = open_equity_risk_usd + pending_stop_risk.unwrap_or(0.0);
    let portfolio_heat_pct = if sleeve > 0.0 {
        heat_usd / sleeve * 100.0
    } else {
        0.0
    };
    PortfolioHeatMetrics {
        portfolio_heat_pct,
        heat_headroom_pct: (ceiling_pct - portfolio_heat_pct).max(0.0),
        heat_ceiling_pct: ceiling_pct,
        positions_open: state.open_positions.len() as u32,
    }
}

pub fn ensure_capital_check(check: &CapitalCheck) -> Result<()> {
    if check.passed {
        return Ok(());
    }
    anyhow::bail!(
        "Capital check failed: {}",
        check.reject_reason.as_deref().unwrap_or("unknown")
    );
}

pub fn capital_check_to_json(check: &CapitalCheck) -> Value {
    serde_json::to_value(check).unwrap_or(json!({}))
}

pub fn effective_profit_target_pct(
    entry_price: f64,
    rules: &TraderRules,
    atr_14: Option<f64>,
) -> f64 {
    exit_geometry(entry_price, rules, atr_14).effective_target_pct
}

/// Breakdown of how the effective target/stop were chosen (for CLI / watch UI).
#[derive(Debug, Clone)]
pub struct ExitGeometry {
    pub atr_pct: Option<f64>,
    pub base_target_pct: f64,
    pub atr_cap_pct: Option<f64>,
    pub horizon_cap_pct: Option<f64>,
    pub effective_target_pct: f64,
    pub effective_stop_pct: f64,
    /// Which target source binds: `fixed`, `atr`, or `horizon`.
    pub target_binding: &'static str,
    pub target_price: f64,
    pub stop_price: f64,
    pub reward_risk: f64,
}

pub fn exit_geometry(
    entry_price: f64,
    rules: &TraderRules,
    atr_14: Option<f64>,
) -> ExitGeometry {
    let base_target = rules.playbook.exit.profit_target_pct;
    let atr_cap_cfg = &rules.playbook.exit.profit_target_atr_cap;
    let horizon_cfg = &rules.playbook.exit.profit_target_horizon_cap;

    let atr_pct = atr_14
        .filter(|a| *a > 0.0 && entry_price > 0.0)
        .map(|atr| (atr / entry_price) * 100.0);

    let atr_cap_pct = atr_pct.filter(|_| atr_cap_cfg.enabled).map(|p| atr_cap_cfg.atr_multiple * p);
    let horizon_cap_pct = atr_pct.filter(|_| horizon_cfg.enabled).map(|p| {
        let days = rules.playbook.holding_period.target_days.max(1) as f64;
        horizon_cfg.sqrt_days_multiple * p * days.sqrt()
    });

    let mut effective_target = base_target;
    let mut target_binding = "fixed";
    if let Some(cap) = atr_cap_pct {
        if cap + f64::EPSILON < effective_target {
            effective_target = cap;
            target_binding = "atr";
        }
    }
    if let Some(cap) = horizon_cap_pct {
        if cap + f64::EPSILON < effective_target {
            effective_target = cap;
            target_binding = "horizon";
        }
    }

    let effective_stop = effective_stop_loss_pct(entry_price, rules, atr_14);
    let reward_risk = if effective_stop > 0.0 {
        effective_target / effective_stop
    } else {
        0.0
    };
    let target_price = entry_price * (1.0 + effective_target / 100.0);
    let stop_price = entry_price * (1.0 - effective_stop / 100.0);

    ExitGeometry {
        atr_pct,
        base_target_pct: base_target,
        atr_cap_pct,
        horizon_cap_pct,
        effective_target_pct: effective_target,
        effective_stop_pct: effective_stop,
        target_binding,
        target_price,
        stop_price,
        reward_risk,
    }
}

/// Compact one-line summary for watch TUI candidate / position cards.
pub fn format_exit_geometry_brief(g: &ExitGeometry) -> String {
    let atr = g
        .atr_pct
        .map(|p| format!("ATR {p:.1}%"))
        .unwrap_or_else(|| "ATR —".into());
    format!(
        "{atr}  tgt +{:.1}% (${:.2})  stop -{:.1}% (${:.2})  R:R {:.2}  [{}]",
        g.effective_target_pct,
        g.target_price,
        g.effective_stop_pct,
        g.stop_price,
        g.reward_risk,
        g.target_binding
    )
}

/// Cap knobs line for overview / rules summary.
pub fn format_exit_cap_rules(rules: &TraderRules) -> String {
    let exit = &rules.playbook.exit;
    let days = rules.playbook.holding_period.target_days;
    let atr = if exit.profit_target_atr_cap.enabled {
        format!("ATR×{:.1}", exit.profit_target_atr_cap.atr_multiple)
    } else {
        "ATR off".into()
    };
    let horizon = if exit.profit_target_horizon_cap.enabled {
        format!(
            "horizon √{}d×{:.1}",
            days, exit.profit_target_horizon_cap.sqrt_days_multiple
        )
    } else {
        "horizon off".into()
    };
    let stop = if exit.stop_loss_atr_cap.enabled {
        format!("stop ATR×{:.1}", exit.stop_loss_atr_cap.atr_multiple)
    } else {
        "stop ATR off".into()
    };
    format!(
        "target caps: {atr} + {horizon}  ·  {stop}  ·  ceil +{:.1}% / -{:.1}%",
        exit.profit_target_pct, exit.stop_loss_pct
    )
}

/// Effective stop distance % = min(stop_loss_pct, cap multiple × ATR%) when
/// stop_loss_atr_cap is enabled; otherwise the fixed stop_loss_pct.
pub fn effective_stop_loss_pct(entry_price: f64, rules: &TraderRules, atr_14: Option<f64>) -> f64 {
    let base = rules.playbook.exit.stop_loss_pct;
    let cap = &rules.playbook.exit.stop_loss_atr_cap;
    if !cap.enabled {
        return base;
    }
    let Some(atr) = atr_14.filter(|a| *a > 0.0 && entry_price > 0.0) else {
        return base;
    };
    let atr_pct = (atr / entry_price) * 100.0;
    base.min(cap.atr_multiple * atr_pct)
}

pub fn exit_prices(
    entry_price: f64,
    rules: &TraderRules,
    atr_14: Option<f64>,
) -> (f64, f64, f64) {
    let g = exit_geometry(entry_price, rules, atr_14);
    let stop_limit = g.stop_price * 0.995;
    (g.target_price, g.stop_price, stop_limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portfolio_heat_includes_pending_risk() {
        let rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            capital: crate::rules::CapitalConfig {
                fixed_sleeve_cap_usd: 4000.0,
                ..Default::default()
            },
            risk: crate::rules::RiskConfig {
                max_portfolio_heat_pct: 8.0,
                ..Default::default()
            },
            ..TraderRules::default()
        };
        let state = TraderState::default();
        let heat = portfolio_heat_metrics(&rules, &state, 100.0, Some(50.0));
        assert!((heat.portfolio_heat_pct - 3.75).abs() < 0.01);
        assert!((heat.heat_headroom_pct - 4.25).abs() < 0.01);
        assert!((heat.heat_ceiling_pct - 8.0).abs() < 0.01);
    }

    #[test]
    fn exit_prices_from_playbook() {
        let rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        let (profit, stop, _) = exit_prices(100.0, &rules, None);
        assert!((profit - 108.0).abs() < 0.01);
        assert!((stop - 96.0).abs() < 0.01);
    }

    #[test]
    fn exit_prices_stop_atr_cap_tightens_low_vol_stop() {
        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.playbook.exit.stop_loss_pct = 5.0;
        rules.playbook.exit.stop_loss_atr_cap.enabled = true;
        rules.playbook.exit.stop_loss_atr_cap.atr_multiple = 2.0;

        // Low-vol name: ATR 1.5% → stop capped at 3.0% (not the fixed 5%).
        let (_, stop, _) = exit_prices(100.0, &rules, Some(1.5));
        assert!((stop - 97.0).abs() < 0.01);

        // High-vol name: ATR 5% → 2.0×ATR = 10% > fixed 5% → fixed wins.
        let (_, stop, _) = exit_prices(100.0, &rules, Some(5.0));
        assert!((stop - 95.0).abs() < 0.01);

        // Missing ATR → fixed fallback.
        let (_, stop, _) = exit_prices(100.0, &rules, None);
        assert!((stop - 95.0).abs() < 0.01);

        // Disabled → fixed.
        rules.playbook.exit.stop_loss_atr_cap.enabled = false;
        let (_, stop, _) = exit_prices(100.0, &rules, Some(1.5));
        assert!((stop - 95.0).abs() < 0.01);
    }

    #[test]
    fn exit_prices_atr_cap_lowers_target() {
        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.playbook.exit.profit_target_pct = 8.0;
        rules.playbook.exit.profit_target_atr_cap.enabled = true;
        rules.playbook.exit.profit_target_atr_cap.atr_multiple = 2.5;
        // ATR 2 on price 100 → ATR% = 2%, cap = 5% < 8%
        let (profit, _, _) = exit_prices(100.0, &rules, Some(2.0));
        assert!((profit - 105.0).abs() < 0.01);
    }

    #[test]
    fn exit_prices_horizon_cap_lowers_target_when_tighter_than_atr_cap() {
        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.playbook.holding_period.target_days = 4; // √4 = 2
        rules.playbook.exit.profit_target_pct = 8.0;
        rules.playbook.exit.profit_target_atr_cap.enabled = true;
        rules.playbook.exit.profit_target_atr_cap.atr_multiple = 5.0; // 5×2% = 10% > base
        rules.playbook.exit.profit_target_horizon_cap.enabled = true;
        rules.playbook.exit.profit_target_horizon_cap.sqrt_days_multiple = 1.0;
        // ATR 2% → horizon = 1.0 × 2% × 2 = 4% < 8%
        let (profit, _, _) = exit_prices(100.0, &rules, Some(2.0));
        assert!((profit - 104.0).abs() < 0.01);
    }

    #[test]
    fn exit_prices_atr_cap_binds_before_looser_horizon() {
        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.playbook.holding_period.target_days = 10; // √10 ≈ 3.16
        rules.playbook.exit.profit_target_pct = 8.0;
        rules.playbook.exit.profit_target_atr_cap.enabled = true;
        rules.playbook.exit.profit_target_atr_cap.atr_multiple = 2.5; // 3.75%
        rules.playbook.exit.profit_target_horizon_cap.enabled = true;
        rules.playbook.exit.profit_target_horizon_cap.sqrt_days_multiple = 1.0;
        // ATR 1.5% → atr cap 3.75%, horizon ≈ 4.74% → atr wins
        let g = exit_geometry(100.0, &rules, Some(1.5));
        assert!((g.effective_target_pct - 3.75).abs() < 0.01);
        assert_eq!(g.target_binding, "atr");
        assert!(g.atr_pct.is_some_and(|p| (p - 1.5).abs() < 0.01));
    }

    #[test]
    fn format_exit_geometry_brief_includes_binding() {
        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.playbook.holding_period.target_days = 4;
        rules.playbook.exit.profit_target_pct = 8.0;
        rules.playbook.exit.profit_target_horizon_cap.enabled = true;
        rules.playbook.exit.profit_target_horizon_cap.sqrt_days_multiple = 1.0;
        let g = exit_geometry(100.0, &rules, Some(2.0));
        let s = format_exit_geometry_brief(&g);
        assert!(s.contains("ATR 2.0%"), "{s}");
        assert!(s.contains("[horizon]"), "{s}");
        assert!(s.contains("tgt +4.0%"), "{s}");
    }
}
