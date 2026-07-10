//! Rich positions tab: per-stock cards with gauges and P/L chart.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Paragraph, Wrap};
use ratatui::Frame;

use schwab_cli::ui::theme::{self, gauge_color, pnl_color};

use super::live::{
    exit_rail, exit_rail_progress_labels, position_health,
    PositionMonitorView, WatchLiveSnapshot,
};
use super::stock_payoff::render_payoff_chart;

pub const CARD_HEIGHT: u16 = 16;

pub fn positions_content_height(monitors: &[PositionMonitorView]) -> u16 {
    if monitors.is_empty() {
        1
    } else {
        monitors.len() as u16 * CARD_HEIGHT
    }
}

pub fn render_positions_panel(
    f: &mut Frame,
    area: Rect,
    monitors: &[PositionMonitorView],
    scroll: u16,
    live: Option<&WatchLiveSnapshot>,
    intraday: bool,
) {
    if monitors.is_empty() {
        f.render_widget(
            Paragraph::new("No open swing positions")
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
            render_position_card(f, card.intersection(area), m, intraday);
        }
        y = y.saturating_add(CARD_HEIGHT);
    }

    if let Some(live) = live {
        let footer_y = y.saturating_sub(scroll);
        if footer_y < area.y + area.height {
            let mut note = String::new();
            if let Some(at) = live.last_fetch {
                let ago = (chrono::Utc::now() - at).num_seconds().max(0);
                note.push_str(&format!("quotes {ago}s ago"));
            }
            if let Some(err) = &live.last_error {
                if !note.is_empty() {
                    note.push(' ');
                }
                note.push_str(&format!("· quote feed: {err}"));
            }
            if !note.is_empty() {
                let footer = Rect {
                    x: area.x,
                    y: footer_y,
                    width: area.width,
                    height: 1,
                };
                f.render_widget(
                    Paragraph::new(note).style(theme::label_style()),
                    footer.intersection(area),
                );
            }
        }
    }
}

/// Compact text preview for the overview tab (first two positions).
pub fn position_preview_lines(monitors: &[PositionMonitorView]) -> Vec<Line<'static>> {
    if monitors.is_empty() {
        return vec![Line::from("(no open positions)")];
    }
    let mut lines = Vec::new();
    for m in monitors.iter().take(2) {
        let health = position_health(m);
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", m.symbol),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} {}  ", health.arrow, health.label),
                Style::default().fg(health.color),
            ),
            Span::styled(
                format!("{:+.1}%  ${:+.0}", m.pnl_pct, m.pnl_usd),
                Style::default().fg(pnl_color(m.pnl_pct)),
            ),
            Span::raw(format!(
                "  last ${:.2}  target ${:.2}",
                m.last_price, m.profit_limit
            )),
        ]));
    }
    if monitors.len() > 2 {
        lines.push(Line::from(Span::styled(
            format!("… +{} more — see Positions tab", monitors.len() - 2),
            theme::label_style(),
        )));
    }
    lines
}

fn render_position_card(f: &mut Frame, area: Rect, m: &PositionMonitorView, intraday: bool) {
    let health = position_health(m);
    let title = format!(
        "{}  x{:.2}  last ${:.2}",
        m.symbol, m.quantity, m.last_price
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

    render_metrics_column(f, cols[0], m, &health, intraday);
    render_payoff_chart(f, cols[1], m);
}

fn render_metrics_column(
    f: &mut Frame,
    area: Rect,
    m: &PositionMonitorView,
    health: &super::live::PositionHealth,
    intraday: bool,
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
            Constraint::Min(0),
        ])
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} {}  ", health.arrow, health.label),
                Style::default()
                    .fg(health.color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:+.2}%  ${:+.2}", m.pnl_pct, m.pnl_usd),
                Style::default().fg(pnl_color(m.pnl_pct)),
            ),
        ])),
        rows[0],
    );

    let target_ratio = (m.pct_toward_target / 100.0).clamp(0.0, 1.0);
    f.render_widget(
        Gauge::default()
            .gauge_style(
                Style::default()
                    .fg(if m.pnl_pct >= 0.0 {
                        theme::PROFIT
                    } else {
                        theme::WARN
                    })
                    .bg(Color::Rgb(40, 44, 56)),
            )
            .ratio(target_ratio)
            .label(format!(
                "toward target {:.0}%",
                m.pct_toward_target.max(0.0)
            )),
        rows[1],
    );

    f.render_widget(
        Gauge::default()
            .gauge_style(
                Style::default()
                    .fg(gauge_color(1.0 - m.pct_above_stop / 100.0))
                    .bg(Color::Rgb(40, 44, 56)),
            )
            .ratio((m.pct_above_stop / 100.0).clamp(0.0, 1.0))
            .label(format!("above stop {:.0}%", m.pct_above_stop)),
        rows[2],
    );

    let bid_ask = match (m.bid, m.ask) {
        (Some(b), Some(a)) => format!("bid ${b:.2}  ask ${a:.2}"),
        _ => "bid/ask —".into(),
    };
    let age = m
        .quote_age_secs
        .map(|s| format!("quote {s}s ago"))
        .unwrap_or_else(|| "no live quote".into());
    f.render_widget(
        Paragraph::new(format!("{bid_ask}  ·  {age}")).style(theme::label_style()),
        rows[3],
    );

    f.render_widget(
        Paragraph::new(format!(
            "entry ${:.2}  stop ${:.2}  target ${:.2}",
            m.entry_price, m.stop_price, m.profit_limit
        ))
        .style(theme::value_style()),
        rows[4],
    );

    let hold = if intraday {
        format!(
            "hold {}m  ·  time stop {}m  ·  min {}d",
            m.hold_minutes, m.time_stop_minutes, m.min_hold_days
        )
    } else {
        format!(
            "hold {}d  ·  time stop {}d  ·  min hold {}d",
            m.hold_days, m.time_stop_days, m.min_hold_days
        )
    };
    f.render_widget(
        Paragraph::new(hold).style(theme::label_style()),
        rows[5],
    );

    let rail = exit_rail(
        m.stop_price,
        m.entry_price,
        m.profit_limit,
        m.last_price,
        area.width.saturating_sub(4).max(12) as usize,
    );
    let rail_color = if m.pnl_pct >= 0.0 {
        theme::PROFIT
    } else {
        theme::WARN
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("exit ", theme::label_style()),
            Span::styled("S", Style::default().fg(theme::LOSS)),
            Span::styled(rail, Style::default().fg(rail_color)),
            Span::styled("T", Style::default().fg(theme::PROFIT)),
            Span::styled(exit_rail_progress_labels(m), theme::label_style()),
        ])),
        rows[6],
    );

    let mut footer = format!("{}  ·  {}", m.oco_label, age);
    if let Some(reason) = m.imminent_exit {
        footer.push_str(&format!("  ·  EXIT: {reason}"));
    }
    f.render_widget(
        Paragraph::new(footer)
            .style(theme::label_style())
            .wrap(Wrap { trim: true }),
        rows[7],
    );
}
