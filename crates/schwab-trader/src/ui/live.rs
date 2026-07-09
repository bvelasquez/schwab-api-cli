//! Live quote + exit-proximity views for the watch TUI.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::adaptation::effective_rules;
use crate::agent::state::{SwingPosition, TraderState};
use crate::closure::{exit_reason_for_position_at, has_working_broker_oco};
use crate::rules::TraderRules;

#[derive(Debug, Clone, Default)]
pub struct QuoteTick {
    pub symbol: String,
    pub last: f64,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct WatchLiveSnapshot {
    pub quotes: HashMap<String, QuoteTick>,
    pub last_fetch: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PositionMonitorView {
    pub symbol: String,
    pub quantity: f64,
    pub entry_price: f64,
    pub last_price: f64,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub pnl_usd: f64,
    pub pnl_pct: f64,
    pub stop_price: f64,
    pub profit_limit: f64,
    pub pct_toward_target: f64,
    pub pct_above_stop: f64,
    pub imminent_exit: Option<&'static str>,
    pub hold_days: u32,
    pub time_stop_days: u32,
    pub min_hold_days: u32,
    pub oco_label: String,
    pub quote_age_secs: Option<i64>,
}

pub fn collect_quote_symbols(state: &TraderState) -> Vec<String> {
    let mut symbols: Vec<String> = state
        .open_positions
        .values()
        .map(|p| p.symbol.to_uppercase())
        .collect();
    if let Some(scan) = state.last_tick_result.as_ref().and_then(|t| t.get("scan")) {
        if let Some(cands) = scan.get("candidates").and_then(|v| v.as_array()) {
            for c in cands.iter().take(8) {
                if let Some(sym) = c.get("symbol").and_then(|v| v.as_str()) {
                    let u = sym.to_uppercase();
                    if !symbols.contains(&u) {
                        symbols.push(u);
                    }
                }
            }
        }
    }
    symbols.sort();
    symbols.dedup();
    symbols
}

pub fn build_position_monitor(
    rules: &TraderRules,
    state: &TraderState,
    pos: &SwingPosition,
    quote: Option<&QuoteTick>,
    now: DateTime<Utc>,
) -> PositionMonitorView {
    let effective = effective_rules(rules, state);
    let last = quote
        .map(|q| q.last)
        .filter(|p| *p > 0.0)
        .unwrap_or_else(|| {
            if pos.market_value_usd > 0.0 && pos.quantity > 0.0 {
                pos.market_value_usd / pos.quantity
            } else {
                pos.entry_price
            }
        });

    let cost = pos.quantity * pos.entry_price;
    let mkt = pos.quantity * last;
    let pnl_usd = mkt - cost;
    let pnl_pct = if pos.entry_price > 0.0 {
        ((last / pos.entry_price) - 1.0) * 100.0
    } else {
        0.0
    };

    let stop_span = (pos.entry_price - pos.stop_price).max(0.01);
    let target_span = (pos.profit_limit - pos.entry_price).max(0.01);
    let pct_above_stop = ((last - pos.stop_price) / stop_span * 100.0).clamp(0.0, 200.0);
    let pct_toward_target = ((last - pos.entry_price) / target_span * 100.0).clamp(-100.0, 150.0);

    let hold_days = (now - pos.opened_at).num_days().max(0) as u32;
    let imminent_exit = exit_reason_for_position_at(&effective, pos, last, now);

    let oco_label = if has_working_broker_oco(pos) {
        "broker OCO".into()
    } else if pos
        .oco_order_id
        .as_deref()
        .is_some_and(|id| id.eq_ignore_ascii_case("simulated"))
    {
        "sim brackets".into()
    } else if effective.playbook.exit.use_oco_at_entry {
        "agent-monitored (no OCO)".into()
    } else {
        "agent-monitored".into()
    };

    let quote_age_secs = quote.map(|q| (now - q.fetched_at).num_seconds());

    PositionMonitorView {
        symbol: pos.symbol.clone(),
        quantity: pos.quantity,
        entry_price: pos.entry_price,
        last_price: last,
        bid: quote.and_then(|q| q.bid),
        ask: quote.and_then(|q| q.ask),
        pnl_usd,
        pnl_pct,
        stop_price: pos.stop_price,
        profit_limit: pos.profit_limit,
        pct_toward_target,
        pct_above_stop,
        imminent_exit,
        hold_days,
        time_stop_days: effective.playbook.exit.time_stop_days,
        min_hold_days: effective.playbook.holding_period.min_days,
        oco_label,
        quote_age_secs,
    }
}

/// Horizontal rail: stop (left) → target (right), `●` = last price, `│` = entry.
pub fn exit_rail(stop: f64, entry: f64, target: f64, last: f64, width: usize) -> String {
    let width = width.max(12);
    let span = (target - stop).max(0.01);
    let mut chars: Vec<char> = vec!['·'; width];
    let entry_idx = ((entry - stop) / span * (width.saturating_sub(1) as f64)).round() as usize;
    let last_idx = ((last - stop) / span * (width.saturating_sub(1) as f64)).round() as usize;
    if entry_idx < width {
        chars[entry_idx] = '│';
    }
    if last_idx < width {
        chars[last_idx] = '●';
    }
    chars.into_iter().collect()
}

pub fn pnl_style(pnl_pct: f64) -> Style {
    if pnl_pct >= 1.0 {
        Style::default().fg(Color::Green)
    } else if pnl_pct <= -1.0 {
        Style::default().fg(Color::Red)
    } else if pnl_pct >= 0.0 {
        Style::default().fg(Color::LightGreen)
    } else {
        Style::default().fg(Color::Yellow)
    }
}

pub fn position_monitor_lines(
    rules: &TraderRules,
    state: &TraderState,
    live: Option<&WatchLiveSnapshot>,
    now: DateTime<Utc>,
) -> Vec<Line<'static>> {
    if state.open_positions.is_empty() {
        return vec![Line::from("(no open swing positions)")];
    }

    let mut lines = Vec::new();
    let mut positions: Vec<_> = state.open_positions.values().collect();
    positions.sort_by(|a, b| a.symbol.cmp(&b.symbol));

    for pos in positions {
        let quote = live
            .and_then(|l| l.quotes.get(&pos.symbol.to_uppercase()));
        let m = build_position_monitor(rules, state, pos, quote, now);
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", m.symbol),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "x{:.2}  last ${:.2}",
                m.quantity, m.last_price
            )),
        ]));

        let bid_ask = match (m.bid, m.ask) {
            (Some(b), Some(a)) => format!("  bid ${b:.2}  ask ${a:.2}"),
            _ => String::new(),
        };
        let age = m
            .quote_age_secs
            .map(|s| format!("  quote {s}s ago"))
            .unwrap_or_else(|| "  (stale — no live quote)".into());

        lines.push(Line::from(vec![
            Span::styled(
                format!("{:+.2}%  {:+.2}", m.pnl_pct, m.pnl_usd),
                pnl_style(m.pnl_pct),
            ),
            Span::raw(format!(" USD{bid_ask}{age}")),
        ]));

        lines.push(Line::from(format!(
            "stop ${:.2}  entry ${:.2}  target ${:.2}",
            m.stop_price, m.entry_price, m.profit_limit
        )));

        let rail = exit_rail(
            m.stop_price,
            m.entry_price,
            m.profit_limit,
            m.last_price,
            28,
        );
        lines.push(Line::from(vec![
            Span::styled("rail ", Style::default().fg(Color::DarkGray)),
            Span::raw(rail),
            Span::styled(
                format!(
                    "  {:.0}%→target  {:.0}% above stop",
                    m.pct_toward_target, m.pct_above_stop
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]));

        let hold = if rules.is_intraday() {
            format!(
                "hold {}m / {}m time stop",
                (now - pos.opened_at).num_minutes().max(0),
                rules.playbook.exit.time_stop_minutes
            )
        } else {
            format!(
                "hold day {} / {} time stop (min {}d)",
                m.hold_days, m.time_stop_days, m.min_hold_days
            )
        };
        lines.push(Line::from(format!("{hold}  │  {}", m.oco_label)));

        if let Some(reason) = m.imminent_exit {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("⚠ EXIT NOW: {reason}"),
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        } else if m.pct_toward_target >= 80.0 {
            lines.push(Line::from(vec![Span::styled(
                format!("▲ near profit target ({:.0}% of way)", m.pct_toward_target),
                Style::default().fg(Color::Green),
            )]));
        } else if m.pct_above_stop <= 25.0 {
            lines.push(Line::from(vec![Span::styled(
                format!("▼ near stop ({:.0}% above stop floor)", m.pct_above_stop),
                Style::default().fg(Color::Red),
            )]));
        }

        lines.push(Line::from(""));
    }

    if let Some(live) = live {
        if let Some(err) = &live.last_error {
            lines.push(Line::from(vec![Span::styled(
                format!("quote feed: {err}"),
                Style::default().fg(Color::Yellow),
            )]));
        }
    }

    lines
}

pub fn regime_and_rules_lines(ctx: &crate::ui::context::WatchContext) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let tick = ctx.last_tick();

    if let Some(profile) = &ctx.state.active_profile {
        lines.push(Line::from(format!(
            "profile: {profile} ({})",
            ctx.state
                .active_profile_source
                .as_deref()
                .unwrap_or("?")
        )));
        if let Some(reason) = &ctx.state.active_profile_reason {
            let short = if reason.len() > 120 {
                format!("{}…", &reason[..117])
            } else {
                reason.clone()
            };
            lines.push(Line::from(short));
        }
    }

    if let Some(regime) = tick.and_then(|t| t.get("regime")) {
        let class = regime.get("class").and_then(|v| v.as_str()).unwrap_or("?");
        let vix = regime.get("vix").and_then(|v| v.as_f64());
        let vol = regime
            .get("realized_vol_percentile")
            .and_then(|v| v.as_f64());
        lines.push(Line::from(format!(
            "regime: {class}  VIX {}  vol pct {}",
            vix.map(|v| format!("{v:.2}")).unwrap_or_else(|| "?".into()),
            vol.map(|v| format!("{v:.0}")).unwrap_or_else(|| "?".into()),
        )));
    }

    if let Some(ep) = tick.and_then(|t| t.get("effective_playbook")) {
        if let Some(exit) = ep.get("exit") {
            let pt = exit
                .get("profit_target_pct")
                .and_then(|v| v.as_f64())
                .unwrap_or(ctx.rules.playbook.exit.profit_target_pct);
            let sl = exit
                .get("stop_loss_pct")
                .and_then(|v| v.as_f64())
                .unwrap_or(ctx.rules.playbook.exit.stop_loss_pct);
            let ts = exit
                .get("time_stop_days")
                .and_then(|v| v.as_u64())
                .unwrap_or(ctx.rules.playbook.exit.time_stop_days as u64);
            lines.push(Line::from(format!(
                "effective exits: +{pt:.1}% target / -{sl:.1}% stop / {ts}d time stop"
            )));
        }
    }

    if let Some(dd) = tick.and_then(|t| t.get("drawdown")) {
        let pct = dd.get("drawdown_pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let halted = dd.get("halted").and_then(|v| v.as_bool()).unwrap_or(false);
        if halted {
            lines.push(Line::from(vec![Span::styled(
                format!("DRAWDOWN HALT {:.1}%", pct),
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            )]));
        } else {
            lines.push(Line::from(format!("drawdown: {pct:.2}%")));
        }
    }

    if lines.is_empty() {
        lines.push(Line::from("(waiting for first tick…)"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_rail_places_markers() {
        let rail = exit_rail(100.0, 110.0, 120.0, 115.0, 20);
        assert!(rail.contains('●'));
        assert!(rail.contains('│'));
    }

    #[test]
    fn pnl_pct_computed() {
        let rules = TraderRules::default();
        let state = TraderState::default();
        let pos = SwingPosition {
            position_id: "t".into(),
            symbol: "AMD".into(),
            account_hash: "a".into(),
            quantity: 1.0,
            entry_price: 100.0,
            opened_at: Utc::now(),
            stop_price: 95.0,
            profit_limit: 110.0,
            stop_risk_usd: 5.0,
            market_value_usd: 0.0,
            oco_order_id: Some("simulated".into()),
            exit_plan_version: 1,
            ..Default::default()
        };
        let q = QuoteTick {
            symbol: "AMD".into(),
            last: 105.0,
            bid: Some(104.9),
            ask: Some(105.1),
            fetched_at: Utc::now(),
        };
        let m = build_position_monitor(&rules, &state, &pos, Some(&q), Utc::now());
        assert!((m.pnl_pct - 5.0).abs() < 0.01);
        assert!(m.pct_toward_target > 0.0);
    }
}
