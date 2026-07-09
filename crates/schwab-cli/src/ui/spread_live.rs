//! Live spread marks + exit-proximity views for the options watch TUI.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::agent::exits::{
    evaluate_exit_from_mark, spread_exit_thresholds, SpreadMark,
};
use crate::agent::spread_analytics::{price_cushion_rail, spread_win_score, SpreadAnalytics};
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

/// Credit spread rail: stop debit (left, high) → target debit (right, low).
pub fn spread_exit_rail(
    stop_debit: f64,
    entry_debit: f64,
    target_debit: f64,
    current_debit: f64,
    width: usize,
) -> String {
    let width = width.max(12);
    let lo = target_debit.min(stop_debit);
    let hi = stop_debit.max(target_debit);
    let span = (hi - lo).max(0.0001);
    let mut chars: Vec<char> = vec!['·'; width];
    let entry_idx =
        ((entry_debit.clamp(lo, hi) - lo) / span * (width.saturating_sub(1) as f64)).round() as usize;
    let current_idx =
        ((current_debit.clamp(lo, hi) - lo) / span * (width.saturating_sub(1) as f64)).round() as usize;
    if entry_idx < width {
        chars[entry_idx] = '│';
    }
    if current_idx < width {
        chars[current_idx] = '●';
    }
    chars.into_iter().collect()
}

pub fn pnl_style(profit_pct: f64) -> Style {
    if profit_pct >= 25.0 {
        Style::default().fg(Color::Green)
    } else if profit_pct <= -25.0 {
        Style::default().fg(Color::Red)
    } else if profit_pct >= 0.0 {
        Style::default().fg(Color::LightGreen)
    } else {
        Style::default().fg(Color::Yellow)
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

fn meter_spans(ratio: f64, width: usize, fill: Color) -> Vec<Span<'static>> {
    let ratio = ratio.clamp(0.0, 1.0);
    let filled = (ratio * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);
    vec![
        Span::styled("█".repeat(filled), Style::default().fg(fill)),
        Span::styled("░".repeat(empty), Style::default().fg(Color::DarkGray)),
    ]
}

fn spread_type_label(a: &SpreadAnalytics) -> &'static str {
    if a.is_put_spread {
        "put credit"
    } else {
        "call credit"
    }
}

fn strike_line(a: &SpreadAnalytics) -> String {
    let leg = if a.is_put_spread { "puts" } else { "calls" };
    format!(
        "{leg} ${:.0}/${:.0}  width ${:.0}",
        a.short_strike, a.long_strike, a.width
    )
}

fn spot_line(a: &SpreadAnalytics) -> String {
    let chg = a
        .underlying_change_pct
        .map(|c| format!(" ({c:+.1}% today)"))
        .unwrap_or_default();
    let otm = a
        .short_otm_pct
        .map(|p| format!("  short {p:.1}% OTM"))
        .unwrap_or_default();
    let dist = a.distance_to_short_strike_usd.map(|d| {
        if d >= 0.0 {
            format!(" (${d:.0} above short)")
        } else {
            format!(" (${:.0} below short)", d.abs())
        }
    });
    format!(
        "spot ${:.2}{chg}{otm}{}",
        a.underlying_price,
        dist.unwrap_or_default()
    )
}

fn health_banner_line(m: &SpreadMonitorView) -> Line<'static> {
    let health = spread_health(m);
    let win = m
        .analytics
        .as_ref()
        .map(|a| spread_win_score(m.profit_pct, a, m.pct_cushion_from_stop))
        .unwrap_or(50.0);
    let win_color = if win >= 70.0 {
        Color::Green
    } else if win >= 45.0 {
        Color::Yellow
    } else {
        Color::Red
    };
    let mut spans = vec![
        Span::styled(
            format!("{} {}  ", health.arrow, health.label),
            Style::default()
                .fg(health.color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("win ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{win:.0}%"),
            Style::default()
                .fg(win_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    spans.extend(meter_spans(win / 100.0, 10, win_color));
    spans.push(Span::styled(
        format!("  {:+.1}% P&L", m.profit_pct),
        pnl_style(m.profit_pct),
    ));
    Line::from(spans)
}

fn probability_line(_m: &SpreadMonitorView, a: &SpreadAnalytics) -> Line<'static> {
    let pop = a.spread_pop_pct.unwrap_or(0.0);
    let pop_color = if pop >= 70.0 {
        Color::Green
    } else if pop >= 50.0 {
        Color::Yellow
    } else {
        Color::Red
    };
    let otm_expire = a
        .approx_short_otm_prob_pct
        .map(|p| format!("{p:.0}%"))
        .unwrap_or_else(|| "—".into());
    let touch_short = a
        .short_delta
        .map(|d| format!("{:.0}%", d.abs() * 100.0))
        .unwrap_or_else(|| "—".into());
    let mut spans = vec![
        Span::styled("POP vs BE ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{pop:.0}%"),
            Style::default().fg(pop_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    spans.extend(meter_spans(pop / 100.0, 8, pop_color));
    spans.push(Span::raw(format!(
        "  short ~{otm_expire} expire OTM  ·  ~{touch_short} touch short"
    )));
    Line::from(spans)
}

fn fmt_opt_f(v: Option<f64>, decimals: usize) -> String {
    v.map(|x| format!("{x:.prec$}", prec = decimals))
        .unwrap_or_else(|| "—".into())
}

fn analytics_lines(m: &SpreadMonitorView) -> Vec<Line<'static>> {
    let Some(a) = &m.analytics else {
        return vec![Line::from(Span::styled(
            "greeks: (waiting for chain refresh…)",
            Style::default().fg(Color::DarkGray),
        ))];
    };

    let mut lines = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(strike_line(a), Style::default().fg(Color::Cyan)),
        Span::raw(format!("  exp {}", m.expiry)),
    ]));

    lines.push(Line::from(Span::styled(
        spot_line(a),
        Style::default().fg(Color::White),
    )));

    lines.push(probability_line(m, a));

    let delta_s = a
        .short_delta
        .map(|d| format!("{d:+.2}"))
        .unwrap_or_else(|| "—".into());
    let delta_l = a
        .long_delta
        .map(|d| format!("{d:+.2}"))
        .unwrap_or_else(|| "—".into());
    let iv = fmt_opt_f(a.chain_iv_pct, 1);
    let theta = a
        .net_theta_per_day_usd
        .map(|t| format!("{:+.2}/d", t))
        .unwrap_or_else(|| "—".into());

    lines.push(Line::from(format!(
        "δ short {delta_s}  long {delta_l}  IV {iv}%  θ {theta}"
    )));

    let be = fmt_opt_f(a.break_even_price, 2);
    let be_cushion = a
        .distance_to_be_pct
        .map(|p| format!("{p:+.1}%"))
        .unwrap_or_else(|| "—".into());
    let ctw = a
        .credit_to_width_pct
        .map(|p| format!("{p:.0}%"))
        .unwrap_or_else(|| "—".into());

    lines.push(Line::from(format!(
        "BE ${be}  cushion {be_cushion}  cr/width {ctw}"
    )));

    if let (Some(em), Some(em_pct)) = (a.expected_move_1sigma_usd, a.expected_move_1sigma_pct) {
        let inside_style = if a.short_strike_inside_1sigma == Some(true) {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let inside = a
            .short_strike_inside_1sigma
            .map(|b| if b { "short inside 1σ" } else { "short outside 1σ" })
            .unwrap_or("");
        lines.push(Line::from(vec![
            Span::raw(format!("1σ move ±${em:.2} ({em_pct:.1}%)  ")),
            Span::styled(inside, inside_style),
        ]));
    }

    if let Some(be_px) = a.break_even_price {
        let (rail, _) = price_cushion_rail(
            be_px,
            a.underlying_price,
            a.short_strike,
            a.is_put_spread,
            28,
        );
        lines.push(Line::from(vec![
            Span::styled("spot ", Style::default().fg(Color::DarkGray)),
            Span::styled(rail, Style::default().fg(Color::Blue)),
            Span::raw("  B=BE  S=short  ●=spot"),
        ]));
    }

    lines
}

pub fn spread_monitor_lines(
    rules: &RulesConfig,
    state: &AgentState,
    live: Option<&SpreadLiveSnapshot>,
) -> Vec<Line<'static>> {
    if state.open_positions.is_empty() {
        return vec![Line::from(Span::styled(
            "(flat — no open positions)",
            Style::default().fg(Color::DarkGray),
        ))];
    }

    let mut positions: Vec<_> = state.open_positions.values().collect();
    positions.sort_by(|a, b| a.underlying.cmp(&b.underlying));

    let mut lines = Vec::new();
    for (i, pos) in positions.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        let live_mark = live.and_then(|l| l.marks.get(&pos.position_id));
        let m = build_spread_monitor(pos, live_mark, &rules.exit_rules);

        let type_label = m
            .analytics
            .as_ref()
            .map(spread_type_label)
            .unwrap_or(m.strategy.as_str());

        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", m.underlying),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "×{}  {type_label}  {}d DTE",
                m.contracts, m.dte
            )),
        ]));

        lines.push(health_banner_line(&m));

        let age = m
            .mark_age_secs
            .map(|s| format!("  mark {s}s ago"))
            .unwrap_or_else(|| "  (no live mark)".into());

        lines.push(Line::from(vec![
            Span::styled(
                format!("{:+.1}%  ${:+.2}", m.profit_pct, m.pnl_usd),
                pnl_style(m.profit_pct),
            ),
            Span::raw(format!(
                "  debit ${:.2}  cr ${:.2}  tgt ≤${:.2}{age}",
                m.debit_to_close, m.entry_credit, m.target_debit
            )),
        ]));

        lines.extend(analytics_lines(&m));

        lines.push(Line::from(vec![
            Span::styled("stop ", Style::default().fg(Color::Red)),
            Span::raw(format!("${:.2}", m.stop_debit)),
            Span::styled("  entry ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("${:.2}", m.entry_credit)),
            Span::styled("  target ", Style::default().fg(Color::Green)),
            Span::raw(format!("${:.2}", m.target_debit)),
        ]));

        let rail = spread_exit_rail(
            m.stop_debit,
            m.entry_credit,
            m.target_debit,
            m.debit_to_close,
            28,
        );
        let rail_style = if m.profit_pct >= 0.0 {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::Yellow)
        };
        lines.push(Line::from(vec![
            Span::styled("P/L  ", Style::default().fg(Color::DarkGray)),
            Span::styled(rail, rail_style),
            Span::raw(format!(
                "  {:.0}%→target  {:.0}% from stop",
                m.pct_toward_target, m.pct_cushion_from_stop
            )),
        ]));

        let mut footer_spans = vec![
            Span::styled(
                format!("close ≤{} DTE", m.dte_close),
                Style::default().fg(Color::DarkGray),
            ),
        ];
        if let Some(reason) = &m.imminent_exit {
            footer_spans.push(Span::raw("  │  "));
            footer_spans.push(Span::styled(
                format!("EXIT: {reason}"),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));
        } else if m.mark_source != "chain" && m.mark_source != "portfolio" {
            footer_spans.push(Span::raw(format!("  │  mark: {}", m.mark_source)));
        }
        lines.push(Line::from(footer_spans));
    }

    if let Some(live) = live {
        if let Some(at) = live.last_fetch {
            let ago = (Utc::now() - at).num_seconds().max(0);
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("chain refresh {ago}s ago"),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    if let Some(err) = live.and_then(|l| l.last_error.as_ref()) {
        lines.push(Line::from(vec![
            Span::styled("mark feed: ", Style::default().fg(Color::Red)),
            Span::raw(err.clone()),
        ]));
    }

    lines
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
    use crate::agent::spread_analytics::compute_vertical_analytics;
    use crate::agent::spread_analytics::VerticalAnalyticsInput;
    use crate::rules::ExitRules;

    #[test]
    fn spread_rail_places_markers() {
        let rail = spread_exit_rail(0.58, 0.29, 0.145, 0.20, 20);
        assert!(rail.contains('│'));
        assert!(rail.contains('●'));
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
