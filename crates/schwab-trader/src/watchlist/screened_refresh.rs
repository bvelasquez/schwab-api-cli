//! In-agent refresh: screen the candidate pool with playbook filters and promote
//! qualifiers into `dynamic_watchlist` (priority over raw FMP movers).

use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use serde_json::{json, Value};

use crate::agent::state::TraderState;
use crate::market_ctx::MarketCtx;
use crate::market_session;
use crate::rules::TraderRules;
use crate::watchlist::build::{build_watchlist, BuildOptions};
use crate::watchlist::dynamic_merge::merge_dynamic_symbols;
use crate::watchlist::patch::write_rules_watchlists;
use crate::watchlist::build::WriteTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenedRefreshTrigger {
    Premarket,
    AtOpen,
    Periodic,
}

impl ScreenedRefreshTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Premarket => "premarket",
            Self::AtOpen => "at_open",
            Self::Periodic => "periodic",
        }
    }
}

pub fn screened_refresh_due(
    state: &TraderState,
    rules: &TraderRules,
    trigger: ScreenedRefreshTrigger,
) -> bool {
    let cfg = &rules.watchlists.screened;
    if !cfg.enabled || !rules.watchlists.dynamic {
        return false;
    }
    if rules.watchlists.candidate_pool_file.is_none() && rules.watchlists.candidate_pool.is_empty() {
        return false;
    }

    let today = market_session::trading_day(&rules.schedule.timezone);

    match trigger {
        ScreenedRefreshTrigger::Premarket => {
            if !cfg.run_premarket {
                return false;
            }
            state.last_screened_premarket_day != Some(today)
        }
        ScreenedRefreshTrigger::AtOpen => {
            if !cfg.run_at_open {
                return false;
            }
            state.last_screened_open_day != Some(today)
        }
        ScreenedRefreshTrigger::Periodic => {
            let every = cfg.refresh_every_minutes.max(15) as i64;
            match state.last_screened_refresh_at {
                None => true,
                Some(at) => Utc::now().signed_duration_since(at).num_minutes() >= every,
            }
        }
    }
}

pub async fn apply_screened_refresh_if_due(
    state: &mut TraderState,
    rules: &TraderRules,
    rules_path: &Path,
    market: &MarketCtx,
    trigger: ScreenedRefreshTrigger,
) -> Result<Option<Value>> {
    if !screened_refresh_due(state, rules, trigger) {
        return Ok(None);
    }

    let cfg = &rules.watchlists.screened;
    let options = BuildOptions {
        top_n: Some(cfg.top_n),
        min_score: Some(cfg.min_score),
    };
    let built = build_watchlist(market, rules, rules_path, &options).await?;

    let qualified: Vec<String> = built.qualified.iter().map(|q| q.symbol.clone()).collect();
    let top_n = cfg.top_n.max(1) as usize;
    let previous_screened = state.screened_dynamic_symbols.clone();
    let added = merge_dynamic_symbols(
        state,
        rules,
        &previous_screened,
        &qualified,
        top_n,
    );
    state.screened_dynamic_symbols = added.clone();
    state.last_screened_refresh_at = Some(Utc::now());
    let today = market_session::trading_day(&rules.schedule.timezone);
    match trigger {
        ScreenedRefreshTrigger::Premarket => state.last_screened_premarket_day = Some(today),
        ScreenedRefreshTrigger::AtOpen => state.last_screened_open_day = Some(today),
        ScreenedRefreshTrigger::Periodic => {}
    }

    let mut write_note = None;
    if cfg.write_thematic && !built.proposed_thematic.is_empty() {
        match write_rules_watchlists(
            rules_path,
            &built.proposed_thematic,
            &built.proposed_core_append,
            WriteTarget::Thematic,
        ) {
            Ok(_) => write_note = Some(rules_path.display().to_string()),
            Err(err) => write_note = Some(format!("write failed: {err:#}")),
        }
    }

    let summary = json!({
        "trigger": trigger.as_str(),
        "qualified_count": built.qualified.len(),
        "qualified": built.qualified.iter().map(|q| json!({
            "symbol": q.symbol,
            "score": q.score,
        })).collect::<Vec<_>>(),
        "added": added,
        "pool_size": built.pool_size,
        "rejected_count": built.rejected.len(),
        "dynamic_watchlist": state.dynamic_watchlist,
        "write_thematic": write_note,
        "refresh_every_minutes": cfg.refresh_every_minutes,
    });
    state.last_screened_refresh = Some(summary.clone());
    Ok(Some(summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::agent::state::TraderState;
    use crate::rules::{TraderRules, WatchlistScreenedConfig};

    fn rules_with_pool() -> TraderRules {
        let mut rules = TraderRules {
            version: 1,
            trader_id: "t".into(),
            accounts: vec![],
            ..TraderRules::default()
        };
        rules.watchlists.dynamic = true;
        rules.watchlists.candidate_pool_file = Some("universe/test.yaml".into());
        rules.watchlists.screened = WatchlistScreenedConfig {
            enabled: true,
            run_premarket: true,
            run_at_open: true,
            refresh_every_minutes: 60,
            ..WatchlistScreenedConfig::default()
        };
        rules
    }

    #[test]
    fn due_premarket_open_and_periodic() {
        let rules = rules_with_pool();
        let mut state = TraderState::default();
        assert!(screened_refresh_due(
            &state,
            &rules,
            ScreenedRefreshTrigger::Premarket
        ));
        assert!(screened_refresh_due(
            &state,
            &rules,
            ScreenedRefreshTrigger::AtOpen
        ));
        assert!(screened_refresh_due(
            &state,
            &rules,
            ScreenedRefreshTrigger::Periodic
        ));

        let today = market_session::trading_day(&rules.schedule.timezone);
        state.last_screened_premarket_day = Some(today);
        assert!(!screened_refresh_due(
            &state,
            &rules,
            ScreenedRefreshTrigger::Premarket
        ));

        state.last_screened_refresh_at = Some(Utc::now());
        assert!(!screened_refresh_due(
            &state,
            &rules,
            ScreenedRefreshTrigger::Periodic
        ));
        state.last_screened_refresh_at = Some(Utc::now() - chrono::Duration::minutes(90));
        assert!(screened_refresh_due(
            &state,
            &rules,
            ScreenedRefreshTrigger::Periodic
        ));
    }
}
