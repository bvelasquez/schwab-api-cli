//! Rich positions tab: per-spread cards with gauges and payoff chart.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Paragraph, Wrap};
use ratatui::Frame;

use crate::agent::spread_analytics::{price_cushion_rail, spread_win_score};
use crate::ui::spread_live::{
    spread_exit_rail, spread_health, spread_rail_progress_labels, SpreadLiveSnapshot,
    SpreadMonitorView,
};
use crate::ui::spread_payoff::render_payoff_chart;
use crate::ui::theme::{self, gauge_color, pnl_color};

pub const CARD_HEIGHT: u16 = 17;

pub fn positions_content_height(monitors: &[SpreadMonitorView]) -> u16 {
    if monitors.is_empty() {
        1
    } else {
        monitors.len() as u16 * CARD_HEIGHT
    }
}

pub fn render_positions_panel(
    f: &mut Frame,
    area: Rect,
    monitors: &[SpreadMonitorView],
    scroll: u16,
    live: Option<&SpreadLiveSnapshot>,
) {
    if monitors.is_empty() {
        f.render_widget(
            Paragraph::new("No open positions — flat")
                .style(theme::label_style())
                .block(theme::panel_block("Positions")),
            area,
        );
        return;
    }

    let mut y = area.y.saturating_sub(scroll);
    for m in monitors {
        let card = Rect {
            x: area.x,
            y,
            width: area.width,
            height: CARD_HEIGHT,
        };
        if card.y < area.y + area.height && card.y + CARD_HEIGHT > area.y {
            let visible = card.intersection(area);
            render_position_card(f, visible, m);
        }
        y = y.saturating_add(CARD_HEIGHT);
    }

    if let Some(live) = live {
        let footer_y = y.saturating_sub(scroll);
        if footer_y < area.y + area.height {
            let mut note = String::new();
            if let Some(at) = live.last_fetch {
                let ago = (chrono::Utc::now() - at).num_seconds().max(0);
                note.push_str(&format!("chain refresh {ago}s ago"));
            }
            if let Some(err) = &live.last_error {
                if !note.is_empty() {
                    note.push_str("  ·  ");
                }
                note.push_str(&format!("mark feed: {err}"));
            }
            if !note.is_empty() {
                let h = 1u16.min(area.height);
                let footer = Rect {
                    x: area.x,
                    y: footer_y,
                    width: area.width,
                    height: h,
                };
                if footer.y < area.y + area.height {
                    f.render_widget(
                        Paragraph::new(note).style(theme::label_style()),
                        footer.intersection(area),
                    );
                }
            }
        }
    }
}

fn render_position_card(f: &mut Frame, area: Rect, m: &SpreadMonitorView) {
    let health = spread_health(m);
    let type_label = m
        .analytics
        .as_ref()
        .map(|a| {
            if a.is_put_spread {
                "put credit"
            } else {
                "call credit"
            }
        })
        .unwrap_or(m.strategy.as_str());

    let title = format!(
        "{}  ×{}  {}  exp {}  {}d DTE",
        m.underlying, m.contracts, type_label, m.expiry, m.dte
    );
    let border_color = if m.imminent_exit.is_some() {
        theme::LOSS
    } else {
        health.color
    };
    let block = theme::panel_block(&title).border_style(Style::default().fg(border_color));

    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 6 || inner.width < 24 {
        return;
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(inner);

    render_metrics_column(f, cols[0], m, &health);
    render_payoff_chart(f, cols[1], m);
}

fn render_metrics_column(
    f: &mut Frame,
    area: Rect,
    m: &SpreadMonitorView,
    health: &crate::ui::spread_live::SpreadHealth,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

    let win = m
        .analytics
        .as_ref()
        .map(|a| spread_win_score(m.profit_pct, a, m.pct_cushion_from_stop))
        .unwrap_or(50.0);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} {}  ", health.arrow, health.label),
                Style::default()
                    .fg(health.color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:+.1}% P&L  ${:+.0}", m.profit_pct, m.pnl_usd),
                Style::default().fg(pnl_color(m.profit_pct)),
            ),
        ])),
        rows[0],
    );

    f.render_widget(thesis_gauge(win), rows[1]);

    if let Some(a) = &m.analytics {
        let pop = a.spread_pop_pct.unwrap_or(0.0);
        f.render_widget(
            Gauge::default()
                .gauge_style(
                    Style::default()
                        .fg(gauge_color(1.0 - pop / 100.0))
                        .bg(Color::Rgb(40, 44, 56)),
                )
                .ratio((pop / 100.0).clamp(0.0, 1.0))
                .label(format!("POP vs BE {pop:.0}%")),
            rows[2],
        );

        let strike_line = if a.is_put_spread {
            format!(
                "puts ${:.0}/${:.0}  width ${:.0}",
                a.short_strike, a.long_strike, a.width
            )
        } else {
            format!(
                "calls ${:.0}/${:.0}  width ${:.0}",
                a.short_strike, a.long_strike, a.width
            )
        };
        f.render_widget(
            Paragraph::new(strike_line).style(Style::default().fg(theme::ACCENT)),
            rows[3],
        );

        let chg = a
            .underlying_change_pct
            .map(|c| format!(" ({c:+.1}% today)"))
            .unwrap_or_default();
        f.render_widget(
            Paragraph::new(format!("spot ${:.2}{chg}", a.underlying_price))
                .style(theme::value_style()),
            rows[4],
        );

        let delta_s = a
            .short_delta
            .map(|d| format!("{d:+.2}"))
            .unwrap_or_else(|| "—".into());
        let theta = a
            .net_theta_per_day_usd
            .map(|t| format!("{:+.2}/d", t))
            .unwrap_or_else(|| "—".into());
        f.render_widget(
            Paragraph::new(format!("δ {delta_s}  θ {theta}  IV {:.0}%",
                a.chain_iv_pct.unwrap_or(0.0)))
            .style(theme::label_style()),
            rows[5],
        );

        if let Some(be) = a.break_even_price {
            let cushion = a
                .distance_to_be_pct
                .map(|p| format!("{p:+.1}%"))
                .unwrap_or_else(|| "—".into());
            let (rail, _) = price_cushion_rail(
                be,
                a.underlying_price,
                a.short_strike,
                a.is_put_spread,
                area.width.saturating_sub(8).max(16) as usize,
            );
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(format!("BE ${be:.2}  cushion {cushion}  ")),
                    Span::styled(rail, Style::default().fg(Color::Blue)),
                    Span::styled("  B S ●", theme::label_style()),
                ])),
                rows[6],
            );
        }
    } else {
        f.render_widget(
            Paragraph::new("waiting for chain refresh…").style(theme::label_style()),
            rows[3],
        );
    }

    let exit_ratio = if m.debit_to_close > m.entry_credit + f64::EPSILON {
        let stop_span = (m.stop_debit - m.entry_credit).max(0.0001);
        ((m.debit_to_close - m.entry_credit) / stop_span).clamp(0.0, 1.0)
    } else {
        let target_span = (m.entry_credit - m.target_debit).max(0.0001);
        ((m.entry_credit - m.debit_to_close) / target_span).clamp(0.0, 1.0)
    };
    let exit_label = if m.debit_to_close > m.entry_credit + f64::EPSILON {
        "toward stop".to_string()
    } else {
        "toward target".to_string()
    };
    f.render_widget(
        Gauge::default()
            .gauge_style(
                Style::default()
                    .fg(if m.debit_to_close > m.entry_credit {
                        theme::LOSS
                    } else {
                        theme::PROFIT
                    })
                    .bg(Color::Rgb(40, 44, 56)),
            )
            .ratio(exit_ratio)
            .label(format!("exit {exit_label} {:.0}%", exit_ratio * 100.0)),
        rows[7],
    );

    let rail = spread_exit_rail(
        m.stop_debit,
        m.entry_credit,
        m.target_debit,
        m.debit_to_close,
        area.width.saturating_sub(4).max(12) as usize,
    );
    let rail_color = if m.profit_pct >= 0.0 {
        theme::PROFIT
    } else {
        theme::WARN
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("debit ", theme::label_style()),
            Span::styled("S", Style::default().fg(theme::LOSS)),
            Span::styled(rail, Style::default().fg(rail_color)),
            Span::styled("T", Style::default().fg(theme::PROFIT)),
            Span::styled(
                spread_rail_progress_labels(m),
                theme::label_style(),
            ),
        ])),
        rows[8],
    );

    let age = m
        .mark_age_secs
        .map(|s| format!("mark {s}s ago"))
        .unwrap_or_else(|| m.mark_source.clone());
    let mut footer = format!(
        "close ≤{} DTE  ·  stop ${:.2}  entry ${:.2}  target ${:.2}  ·  {age}",
        m.dte_close, m.stop_debit, m.entry_credit, m.target_debit
    );
    if let Some(reason) = &m.imminent_exit {
        footer.push_str(&format!("  ·  EXIT: {reason}"));
    }
    f.render_widget(
        Paragraph::new(footer)
            .style(theme::label_style())
            .wrap(Wrap { trim: true }),
        rows[9],
    );
}

fn thesis_gauge(win: f64) -> Gauge<'static> {
    Gauge::default()
        .gauge_style(
            Style::default()
                .fg(gauge_color(1.0 - win / 100.0))
                .bg(Color::Rgb(40, 44, 56)),
        )
        .ratio((win / 100.0).clamp(0.0, 1.0))
        .label(format!("thesis score {win:.0}%"))
}
