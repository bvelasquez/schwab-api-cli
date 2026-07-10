//! Live spread marks + exit-proximity views for the options watch TUI.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ratatui::style::Color;

use crate::agent::exits::{
    evaluate_exit_from_mark, spread_exit_thresholds, SpreadMark,
};
use crate::agent::spread_analytics::SpreadAnalytics;
use crate::agent::state::{AgentState, TrackedPosition};
use crate::rules::RulesConfig;

#[derive(Debug, Clone, Default)]
pub struct SpreadLiveSnapshot {
    pub marks: HashMap<String, SpreadPositionMark>,
    pub last_fetch: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SpreadPositionMark {
    pub mark: SpreadMark,
    pub analytics: Option<SpreadAnalytics>,
    pub imminent_exit: Option<String>,
    pub mark_age_secs: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct SpreadMonitorView {
    pub underlying: String,
    pub expiry: String,
    pub strategy: String,
    pub contracts: u32,
    pub entry_credit: f64,
    pub debit_to_close: f64,
    pub target_debit: f64,
    pub stop_debit: f64,
    pub profit_pct: f64,
    pub pnl_usd: f64,
    pub pct_toward_target: f64,
    pub pct_cushion_from_stop: f64,
    pub dte: i64,
    pub dte_close: u32,
    pub imminent_exit: Option<String>,
    pub mark_source: String,
    pub mark_age_secs: Option<i64>,
    pub analytics: Option<SpreadAnalytics>,
}

pub fn build_spread_monitor(
    tracked: &TrackedPosition,
    live: Option<&SpreadPositionMark>,
    exit_rules: &crate::rules::ExitRules,
) -> SpreadMonitorView {
    let entry_credit = tracked
        .entry_credit
        .filter(|c| *c > f64::EPSILON)
        .unwrap_or(0.0);
    let contracts = tracked.contracts.max(1);
    let (target_debit, stop_debit) = spread_exit_thresholds(entry_credit, exit_rules);

    let (debit_to_close, profit_pct, dte, mark_source, mark_age_secs, imminent_exit, analytics) =
        if let Some(live) = live {
            (
                live.mark.debit_to_close,
                live.mark.profit_pct,
                live.mark.dte,
                live.mark.source.clone(),
                live.mark_age_secs,
                live.imminent_exit.clone(),
                live.analytics.clone(),
            )
        } else {
            (
                entry_credit,
                0.0,
                0,
                "stale".into(),
                None,
                None,
                None,
            )
        };

    let pnl_usd = (entry_credit - debit_to_close) * 100.0 * contracts as f64;

    let target_span = (entry_credit - target_debit).max(0.0001);
    let pct_toward_target =
        ((entry_credit - debit_to_close) / target_span * 100.0).clamp(-100.0, 150.0);

    let stop_span = (stop_debit - entry_credit).max(0.0001);
    let pct_cushion_from_stop =
        ((stop_debit - debit_to_close) / stop_span * 100.0).clamp(0.0, 200.0);

    SpreadMonitorView {
        underlying: tracked.underlying.clone(),
        expiry: tracked.expiry.clone(),
        strategy: tracked.strategy.clone(),
        contracts,
        entry_credit,
        debit_to_close,
        target_debit,
        stop_debit,
        profit_pct,
        pnl_usd,
        pct_toward_target,
        pct_cushion_from_stop,
        dte,
        dte_close: exit_rules.dte_close,
        imminent_exit,
        mark_source,
        mark_age_secs,
        analytics,
    }
}

/// Credit spread rail: stop debit (left, bad) → target debit (right, good).
/// Matches the equity monitor: moving right is winning.
pub fn spread_exit_rail(
    stop_debit: f64,
    entry_debit: f64,
    target_debit: f64,
    current_debit: f64,
    width: usize,
) -> String {
    let width = width.max(12);
    let span = (stop_debit - target_debit).max(0.0001);
    let max_idx = width.saturating_sub(1) as f64;
    let debit_idx = |debit: f64| {
        ((stop_debit - debit.clamp(target_debit, stop_debit)) / span * max_idx).round() as usize
    };
    let mut chars: Vec<char> = vec!['·'; width];
    let entry_idx = debit_idx(entry_debit);
    let current_idx = debit_idx(current_debit);
    if entry_idx < width {
        chars[entry_idx] = '│';
    }
    if current_idx < width {
        chars[current_idx] = '●';
    }
    chars.into_iter().collect()
}

pub(crate) fn spread_rail_progress_labels(m: &SpreadMonitorView) -> String {
    let above_stop = m.pct_cushion_from_stop;
    if m.debit_to_close > m.entry_credit + f64::EPSILON {
        let stop_span = (m.stop_debit - m.entry_credit).max(0.0001);
        let toward_stop =
            ((m.debit_to_close - m.entry_credit) / stop_span * 100.0).clamp(0.0, 200.0);
        format!("  {toward_stop:.0}% toward stop  {above_stop:.0}% above stop")
    } else {
        format!(
            "  {:.0}%→target  {above_stop:.0}% above stop",
            m.pct_toward_target.max(0.0)
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SpreadHealth {
    pub label: &'static str,
    pub arrow: &'static str,
    pub color: Color,
}

pub fn spread_health(m: &SpreadMonitorView) -> SpreadHealth {
    let pop = m.analytics.as_ref().and_then(|a| a.spread_pop_pct);
    let delta = m
        .analytics
        .as_ref()
        .and_then(|a| a.short_delta.map(|d| d.abs()));
    let near_stop = m.pct_cushion_from_stop < 30.0;

    if m.imminent_exit.is_some() {
        return SpreadHealth {
            label: "EXIT SOON",
            arrow: "!",
            color: Color::Red,
        };
    }
    if m.profit_pct <= -25.0 || (near_stop && m.profit_pct < -10.0) {
        return SpreadHealth {
            label: "LOSING",
            arrow: "▼",
            color: Color::Red,
        };
    }
    if m.profit_pct < 0.0 || pop.is_some_and(|p| p < 50.0) || delta.is_some_and(|d| d >= 0.35) {
        return SpreadHealth {
            label: "AT RISK",
            arrow: "▼",
            color: Color::Yellow,
        };
    }
    if delta.is_some_and(|d| d >= 0.28) || pop.is_some_and(|p| p < 60.0) {
        return SpreadHealth {
            label: "WATCH",
            arrow: "◆",
            color: Color::Magenta,
        };
    }
    if m.profit_pct >= 40.0 || m.pct_toward_target >= 90.0 {
        return SpreadHealth {
            label: "STRONG WIN",
            arrow: "▲▲",
            color: Color::LightGreen,
        };
    }
    if m.profit_pct > 0.0 {
        return SpreadHealth {
            label: "WINNING",
            arrow: "▲",
            color: Color::Green,
        };
    }
    SpreadHealth {
        label: "HOLDING",
        arrow: "═",
        color: Color::Cyan,
    }
}

pub fn list_spread_monitors(
    rules: &RulesConfig,
    state: &AgentState,
    live: Option<&SpreadLiveSnapshot>,
) -> Vec<SpreadMonitorView> {
    let mut positions: Vec<_> = state.open_positions.values().collect();
    positions.sort_by(|a, b| a.underlying.cmp(&b.underlying));
    positions
        .into_iter()
        .map(|pos| {
            let live_mark = live.and_then(|l| l.marks.get(&pos.position_id));
            build_spread_monitor(pos, live_mark, &rules.exit_rules)
        })
        .collect()
}

pub fn attach_exit_hint(mark: &mut SpreadPositionMark, rules: &RulesConfig, entry_credit: f64) {
    if entry_credit <= f64::EPSILON {
        return;
    }
    if let Some(eval) = evaluate_exit_from_mark(rules, Some(entry_credit), &mark.mark) {
        mark.imminent_exit = Some(eval.reason);
    } else if mark.mark.dte <= rules.exit_rules.dte_close as i64 {
        mark.imminent_exit = Some("dte_close".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::spread_analytics::{compute_vertical_analytics, spread_win_score, VerticalAnalyticsInput};
    use crate::rules::ExitRules;

    #[test]
    fn spread_rail_places_markers() {
        let rail = spread_exit_rail(0.58, 0.29, 0.145, 0.20, 20);
        assert!(rail.contains('│'));
        assert!(rail.contains('●'));
    }

    #[test]
    fn spread_rail_right_is_toward_target() {
        let winning = spread_exit_rail(0.58, 0.29, 0.145, 0.20, 28);
        let entry_win = winning.find('│').unwrap();
        let mark_win = winning.find('●').unwrap();
        assert!(
            mark_win > entry_win,
            "lower debit (winning) should sit right of entry"
        );

        let losing = spread_exit_rail(0.56, 0.28, 0.14, 0.46, 28);
        let entry_lose = losing.find('│').unwrap();
        let mark_lose = losing.find('●').unwrap();
        assert!(
            mark_lose < entry_lose,
            "higher debit (losing) should sit left of entry toward stop"
        );
    }

    #[test]
    fn spread_rail_labels_show_toward_stop_when_losing() {
        let exit_rules = ExitRules::default();
        let tracked = TrackedPosition {
            position_id: "IWM|2026-08-14".into(),
            account_hash: "h".into(),
            underlying: "IWM".into(),
            expiry: "2026-08-14".into(),
            strategy: "vertical".into(),
            opened_at: Utc::now(),
            entry_credit: Some(0.28),
            max_loss_usd: 144.0,
            contracts: 1,
            entry_params: None,
            ..Default::default()
        };
        let live = SpreadPositionMark {
            mark: SpreadMark {
                entry_credit: 0.28,
                debit_to_close: 0.46,
                profit_pct: -64.3,
                dte: 35,
                source: "test".into(),
            },
            analytics: None,
            imminent_exit: None,
            mark_age_secs: Some(0),
        };
        let m = build_spread_monitor(&tracked, Some(&live), &exit_rules);
        let labels = spread_rail_progress_labels(&m);
        assert!(labels.contains("toward stop"));
        assert!(labels.contains("above stop"));
        assert!(!labels.contains("→target"));
    }

    #[test]
    fn profit_pct_maps_to_target_progress() {
        let exit_rules = ExitRules::default();
        let tracked = TrackedPosition {
            position_id: "SPY|2026-07-18".into(),
            account_hash: "h".into(),
            underlying: "SPY".into(),
            expiry: "2026-07-18".into(),
            strategy: "vertical".into(),
            opened_at: Utc::now(),
            entry_credit: Some(0.40),
            max_loss_usd: 200.0,
            contracts: 2,
            entry_params: None,
            ..Default::default()
        };
        let analytics = compute_vertical_analytics(VerticalAnalyticsInput {
            is_put_spread: true,
            underlying_price: 520.0,
            short_strike: 500.0,
            long_strike: 498.0,
            credit: 0.40,
            dte: 25,
            chain_iv_pct: Some(18.0),
            short_delta: Some(-0.20),
            long_delta: Some(-0.12),
            short_theta: Some(-0.10),
            long_theta: Some(-0.06),
            contracts: 2,
            underlying_change_pct: Some(-0.3),
        });
        let live = SpreadPositionMark {
            mark: SpreadMark {
                entry_credit: 0.40,
                debit_to_close: 0.20,
                profit_pct: 50.0,
                dte: 25,
                source: "test".into(),
            },
            analytics: Some(analytics),
            imminent_exit: Some("profit_target".into()),
            mark_age_secs: Some(5),
        };
        let m = build_spread_monitor(&tracked, Some(&live), &exit_rules);
        assert!((m.pct_toward_target - 100.0).abs() < 0.1);
        assert!((m.pnl_usd - 40.0).abs() < 0.01);
        assert!(m.analytics.is_some());
    }

    #[test]
    fn winning_position_gets_winning_health() {
        let exit_rules = ExitRules::default();
        let tracked = TrackedPosition {
            position_id: "IWM|2026-07-31".into(),
            account_hash: "h".into(),
            underlying: "IWM".into(),
            expiry: "2026-07-31".into(),
            strategy: "vertical".into(),
            opened_at: Utc::now(),
            entry_credit: Some(0.32),
            max_loss_usd: 136.0,
            contracts: 2,
            entry_params: None,
            ..Default::default()
        };
        let analytics = compute_vertical_analytics(VerticalAnalyticsInput {
            is_put_spread: true,
            underlying_price: 301.0,
            short_strike: 282.0,
            long_strike: 280.0,
            credit: 0.32,
            dte: 30,
            chain_iv_pct: Some(29.0),
            short_delta: Some(-0.16),
            long_delta: Some(-0.14),
            short_theta: Some(-0.06),
            long_theta: Some(-0.04),
            contracts: 2,
            underlying_change_pct: Some(0.4),
        });
        let live = SpreadPositionMark {
            mark: SpreadMark {
                entry_credit: 0.32,
                debit_to_close: 0.27,
                profit_pct: 14.7,
                dte: 30,
                source: "test".into(),
            },
            analytics: Some(analytics),
            imminent_exit: None,
            mark_age_secs: Some(1),
        };
        let m = build_spread_monitor(&tracked, Some(&live), &exit_rules);
        let h = spread_health(&m);
        assert!(matches!(h.label, "WINNING" | "STRONG WIN" | "WATCH"));
        assert!(spread_win_score(m.profit_pct, m.analytics.as_ref().unwrap(), m.pct_cushion_from_stop)
            > 60.0);
    }
}
