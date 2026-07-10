//! Long-stock P/L diagram (linear risk graph) for the watch TUI.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::symbols::Marker;
use ratatui::widgets::canvas::{Canvas, Line};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use schwab_cli::ui::chart_markers::{draw_chart_marker, marker_spans};
use schwab_cli::ui::theme::{self, label_style};

use super::live::PositionMonitorView;

/// Unrealized P/L in USD at a given price for a long position.
pub fn long_stock_pnl_usd(price: f64, entry: f64, quantity: f64) -> f64 {
    (price - entry) * quantity
}

#[derive(Debug, Clone)]
struct PayoffBounds {
    x_min: f64,
    x_max: f64,
    y_min: f64,
    y_max: f64,
    max_profit: f64,
    max_loss: f64,
}

fn payoff_bounds(m: &PositionMonitorView) -> PayoffBounds {
    let qty = m.quantity.max(0.0);
    let max_profit = long_stock_pnl_usd(m.profit_limit, m.entry_price, qty);
    let max_loss = long_stock_pnl_usd(m.stop_price, m.entry_price, qty);
    let pad = (m.entry_price * 0.04).max(1.0);
    let x_min = m.stop_price.min(m.last_price) - pad;
    let x_max = m.profit_limit.max(m.last_price) + pad;
    let y_lo = long_stock_pnl_usd(x_min, m.entry_price, qty);
    let y_hi = long_stock_pnl_usd(x_max, m.entry_price, qty);
    let y_pad = y_hi.abs().max(y_lo.abs()) * 0.12;
    PayoffBounds {
        x_min,
        x_max,
        y_min: y_lo.min(0.0) - y_pad,
        y_max: y_hi.max(0.0) + y_pad,
        max_profit,
        max_loss,
    }
}

pub fn render_payoff_chart(f: &mut Frame, area: Rect, m: &PositionMonitorView) {
    let bounds = payoff_bounds(m);
    let qty = m.quantity;
    let entry = m.entry_price;
    let last = m.last_price;
    let x_min = bounds.x_min;
    let x_max = bounds.x_max;
    let y_min = bounds.y_min;
    let y_max = bounds.y_max;
    let spot_y = long_stock_pnl_usd(last, entry, qty);

    let chart_title = format!(
        " +${:.0} / -${:.0} ",
        bounds.max_profit.max(0.0),
        bounds.max_loss.abs()
    );

    let stop_price = m.stop_price;
    let profit_limit = m.profit_limit;

    let (hx, hy) = marker_spans(x_min, x_max, y_min, y_max);

    let canvas = Canvas::default()
        .block(
            Block::default()
                .title(format!(" P/L vs price{chart_title} "))
                .title_bottom(" ● now ")
                .title_style(label_style().add_modifier(Modifier::ITALIC)),
        )
        .marker(Marker::Braille)
        .x_bounds([x_min, x_max])
        .y_bounds([y_min, y_max])
        .paint(move |ctx| {
            ctx.draw(&Line::new(x_min, 0.0, x_max, 0.0, Color::DarkGray));
            ctx.draw(&Line::new(
                x_min,
                long_stock_pnl_usd(x_min, entry, qty),
                x_max,
                long_stock_pnl_usd(x_max, entry, qty),
                theme::ACCENT,
            ));
            ctx.draw(&Line::new(
                stop_price,
                y_min,
                stop_price,
                y_max,
                Color::Red,
            ));
            ctx.draw(&Line::new(
                profit_limit,
                y_min,
                profit_limit,
                y_max,
                theme::PROFIT,
            ));
            ctx.draw(&Line::new(last, y_min, last, y_max, Color::Rgb(50, 110, 130)));
            draw_chart_marker(ctx, last, spot_y, hx, hy, Color::LightYellow, "●");
        });

    if area.height < 4 {
        f.render_widget(
            Paragraph::new("chart (expand terminal)").style(label_style()),
            area,
        );
    } else {
        f.render_widget(canvas, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::agent::state::SwingPosition;
    use crate::rules::TraderRules;
    use crate::ui::live::{build_position_monitor, QuoteTick};

    #[test]
    fn long_stock_pnl_is_linear() {
        assert!((long_stock_pnl_usd(110.0, 100.0, 10.0) - 100.0).abs() < 0.01);
        assert!((long_stock_pnl_usd(95.0, 100.0, 10.0) + 50.0).abs() < 0.01);
    }

    #[test]
    fn payoff_bounds_for_monitor() {
        let rules = TraderRules::default();
        let state = crate::agent::state::TraderState::default();
        let pos = SwingPosition {
            position_id: "t".into(),
            symbol: "AMD".into(),
            account_hash: "a".into(),
            quantity: 10.0,
            entry_price: 100.0,
            opened_at: Utc::now(),
            stop_price: 95.0,
            profit_limit: 110.0,
            stop_risk_usd: 50.0,
            market_value_usd: 0.0,
            oco_order_id: None,
            exit_plan_version: 1,
            ..Default::default()
        };
        let q = QuoteTick {
            symbol: "AMD".into(),
            last: 105.0,
            bid: None,
            ask: None,
            fetched_at: Utc::now(),
        };
        let m = build_position_monitor(&rules, &state, &pos, Some(&q), Utc::now());
        let b = payoff_bounds(&m);
        assert!(b.max_profit > 0.0);
        assert!(b.max_loss < 0.0);
    }
}
