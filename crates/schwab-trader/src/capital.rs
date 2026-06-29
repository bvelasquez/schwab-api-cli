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

pub fn exit_prices(entry_price: f64, rules: &TraderRules) -> (f64, f64, f64) {
    let profit = entry_price * (1.0 + rules.playbook.exit.profit_target_pct / 100.0);
    let stop = entry_price * (1.0 - rules.playbook.exit.stop_loss_pct / 100.0);
    let stop_limit = stop * 0.995;
    (profit, stop, stop_limit)
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
        let (profit, stop, _) = exit_prices(100.0, &rules);
        assert!((profit - 108.0).abs() < 0.01);
        assert!((stop - 96.0).abs() < 0.01);
    }
}
