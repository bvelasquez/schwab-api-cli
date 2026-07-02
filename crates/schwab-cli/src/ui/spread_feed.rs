//! Background Schwab spread mark refresh for options watch TUI.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use schwab_api::TraderApi;
use schwab_market_data::MarketDataApi;

use crate::agent::exits::{
    evaluate_position_monitor, load_live_position_groups, mark_from_net_market_value,
    option_group_from_tracked,
};
use crate::agent::paths::active_state_path;
use crate::agent::state::load_state;
use crate::rules::RulesConfig;
use crate::ui::spread_live::{attach_exit_hint, SpreadLiveSnapshot, SpreadPositionMark};

const MARK_REFRESH_SECS: u64 = 15;

pub fn new_spread_snapshot() -> Arc<RwLock<SpreadLiveSnapshot>> {
    Arc::new(RwLock::new(SpreadLiveSnapshot::default()))
}

pub fn spawn_spread_mark_feed(
    rules_path: std::path::PathBuf,
    simulate: bool,
    live: Arc<RwLock<SpreadLiveSnapshot>>,
    market: Arc<MarketDataApi>,
    trader: Arc<TraderApi>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let rules = match RulesConfig::load(&rules_path) {
            Ok(r) => r,
            Err(err) => {
                if let Ok(mut g) = live.write() {
                    g.last_error = Some(format!("rules load: {err:#}"));
                }
                return;
            }
        };
        let mut interval = tokio::time::interval(Duration::from_secs(MARK_REFRESH_SECS));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(err) =
                refresh_once(&rules_path, &rules, simulate, &market, &trader, &live).await
            {
                if let Ok(mut g) = live.write() {
                    g.last_error = Some(err.to_string());
                }
            }
        }
    })
}

async fn refresh_once(
    rules_path: &Path,
    rules: &RulesConfig,
    simulate: bool,
    market: &MarketDataApi,
    trader: &TraderApi,
    live: &Arc<RwLock<SpreadLiveSnapshot>>,
) -> Result<()> {
    let state_path = active_state_path(rules_path, simulate);
    let state = load_state(&state_path)?;
    if state.open_positions.is_empty() {
        if let Ok(mut g) = live.write() {
            g.marks.clear();
            g.last_fetch = Some(Utc::now());
            g.last_error = None;
        }
        return Ok(());
    }

    let today = Utc::now().date_naive();
    let live_groups = if simulate {
        HashMap::new()
    } else {
        load_live_position_groups(trader, rules).await.unwrap_or_default()
    };

    let mut marks = HashMap::new();
    let fetch_started = Utc::now();

    for (position_id, tracked) in &state.open_positions {
        let entry_credit = tracked.entry_credit.filter(|c| *c > f64::EPSILON);
        let group = if simulate {
            option_group_from_tracked(tracked)
        } else {
            live_groups
                .get(position_id)
                .cloned()
                .or_else(|| option_group_from_tracked(tracked))
        };

        let Some(group) = group else {
            continue;
        };

        let monitor =
            evaluate_position_monitor(market, &group, rules, today, Some(tracked)).await?;

        let spread_mark = monitor
            .exit
            .as_ref()
            .map(|e| e.mark.clone())
            .or(monitor.mark)
            .or_else(|| {
                entry_credit.and_then(|entry| mark_from_net_market_value(&group, entry, today))
            });

        let Some(mut mark) = spread_mark.map(|m| SpreadPositionMark {
            mark: m,
            analytics: monitor.analytics.clone(),
            imminent_exit: None,
            mark_age_secs: None,
        }) else {
            continue;
        };

        if let Some(entry) = entry_credit {
            attach_exit_hint(&mut mark, rules, entry);
        }
        mark.mark_age_secs = Some((Utc::now() - fetch_started).num_seconds().max(0));
        marks.insert(position_id.clone(), mark);
    }

    if let Ok(mut g) = live.write() {
        g.marks = marks;
        g.last_fetch = Some(Utc::now());
        g.last_error = None;
    }
    Ok(())
}
