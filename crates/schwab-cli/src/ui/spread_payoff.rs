//! Expiry payoff (risk graph) for vertical credit spreads.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::symbols::Marker;
use ratatui::widgets::canvas::{Canvas, Line};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use super::chart_markers::{draw_chart_marker, marker_spans};
use super::spread_live::SpreadMonitorView;
use super::theme::{self, label_style};

/// P/L in dollars at expiry for one vertical credit spread.
pub fn vertical_credit_payoff_usd(
    spot: f64,
    is_put_spread: bool,
    short_strike: f64,
    long_strike: f64,
    credit: f64,
    contracts: u32,
) -> f64 {
    let intrinsic = if is_put_spread {
        (short_strike - spot).max(0.0) - (long_strike - spot).max(0.0)
    } else {
        (spot - short_strike).max(0.0) - (spot - long_strike).max(0.0)
    };
    (credit - intrinsic) * 100.0 * contracts.max(1) as f64
}

#[derive(Debug, Clone)]
pub struct PayoffBounds {
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
    pub max_profit: f64,
    pub max_loss: f64,
}

pub fn payoff_bounds(m: &SpreadMonitorView) -> Option<PayoffBounds> {
    let a = m.analytics.as_ref()?;
    let width = a.width.max(0.01);
    let credit = m.entry_credit.max(0.0);
    let contracts = m.contracts.max(1);
    let max_profit = credit * 100.0 * contracts as f64;
    let max_loss = a
        .max_loss_per_spread_usd
        .map(|l| l * contracts as f64)
        .unwrap_or((width - credit) * 100.0 * contracts as f64);

    let pad = (a.underlying_price * 0.04).max(width * 1.5);
    let (lo_strike, hi_strike) = if a.is_put_spread {
        (a.long_strike, a.short_strike)
    } else {
        (a.short_strike, a.long_strike)
    };
    let x_min = lo_strike.min(a.underlying_price) - pad;
    let x_max = hi_strike.max(a.underlying_price) + pad;
    let expiry_at_spot = vertical_credit_payoff_usd(
        a.underlying_price,
        a.is_put_spread,
        a.short_strike,
        a.long_strike,
        credit,
        contracts,
    );
    let y_lo = expiry_at_spot.min(-max_loss).min(m.pnl_usd);
    let y_hi = expiry_at_spot.max(max_profit).max(m.pnl_usd);
    let y_pad = y_hi.abs().max(y_lo.abs()) * 0.15 + 1.0;
    Some(PayoffBounds {
        x_min,
        x_max,
        y_min: y_lo - y_pad,
        y_max: y_hi + y_pad,
        max_profit,
        max_loss,
    })
}

pub fn render_payoff_chart(f: &mut Frame, area: Rect, m: &SpreadMonitorView) {
    let Some(a) = m.analytics.as_ref() else {
        f.render_widget(
            Paragraph::new("payoff chart\n(waiting for chain)")
                .style(label_style())
                .block(
                    Block::default()
                        .title(" Payoff @ expiry ")
                        .title_style(label_style().add_modifier(Modifier::ITALIC)),
                ),
            area,
        );
        return;
    };
    let Some(bounds) = payoff_bounds(m) else {
        return;
    };

    let is_put = a.is_put_spread;
    let short = a.short_strike;
    let long = a.long_strike;
    let credit = m.entry_credit;
    let contracts = m.contracts;
    let spot = a.underlying_price;

    let sample_count = 48usize;
    let x_step = (bounds.x_max - bounds.x_min) / sample_count as f64;
    let mut payoff_coords: Vec<(f64, f64)> = Vec::with_capacity(sample_count + 1);
    let mut x = bounds.x_min;
    for _ in 0..=sample_count {
        let y = vertical_credit_payoff_usd(x, is_put, short, long, credit, contracts);
        payoff_coords.push((x, y));
        x += x_step;
    }
    let spot_y =
        vertical_credit_payoff_usd(spot, is_put, short, long, credit, contracts);
    let now_y = m.pnl_usd;
    let show_expiry_ref = (now_y - spot_y).abs() > 1.0;

    let x_min = bounds.x_min;
    let x_max = bounds.x_max;
    let y_min = bounds.y_min;
    let y_max = bounds.y_max;
    let (hx, hy) = marker_spans(x_min, x_max, y_min, y_max);

    let chart_title = format!(
        " +${:.0}/-${:.0}  spot ${:.0}  now ${:+.0}",
        bounds.max_profit, bounds.max_loss, spot, now_y
    );
    let legend = if show_expiry_ref {
        " ● now   ○ expiry@spot "
    } else {
        " ● now "
    };

    let canvas = Canvas::default()
        .block(
            Block::default()
                .title(format!(" Payoff @ expiry{chart_title} "))
                .title_bottom(legend)
                .title_style(label_style().add_modifier(Modifier::ITALIC)),
        )
        .marker(Marker::Braille)
        .x_bounds([x_min, x_max])
        .y_bounds([y_min, y_max])
        .paint(move |ctx| {
            ctx.draw(&Line::new(x_min, 0.0, x_max, 0.0, Color::DarkGray));

            for window in payoff_coords.windows(2) {
                let (x1, y1) = window[0];
                let (x2, y2) = window[1];
                let color = if y1 >= 0.0 && y2 >= 0.0 {
                    theme::PROFIT
                } else if y1 <= 0.0 && y2 <= 0.0 {
                    theme::LOSS
                } else {
                    theme::WARN
                };
                ctx.draw(&Line::new(x1, y1, x2, y2, color));
            }

            // Spot price — dim full-height guide.
            ctx.draw(&Line::new(spot, y_min, spot, y_max, Color::Rgb(50, 110, 130)));

            if show_expiry_ref {
                draw_chart_marker(
                    ctx,
                    spot,
                    spot_y,
                    hx * 0.55,
                    hy * 0.55,
                    Color::Gray,
                    "○",
                );
                ctx.draw(&Line::new(
                    spot,
                    now_y,
                    spot,
                    spot_y,
                    Color::Magenta,
                ));
            }

            // Live mark — bright crosshair + bold ● (primary focal point).
            draw_chart_marker(
                ctx,
                spot,
                now_y,
                hx,
                hy,
                Color::LightYellow,
                "●",
            );
        });

    f.render_widget(canvas, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::exits::SpreadMark;
    use crate::agent::spread_analytics::{compute_vertical_analytics, VerticalAnalyticsInput};
    use crate::agent::state::TrackedPosition;
    use crate::rules::ExitRules;
    use crate::ui::spread_live::{build_spread_monitor, SpreadPositionMark};
    use chrono::Utc;

    fn sample_monitor() -> SpreadMonitorView {
        let exit_rules = ExitRules::default();
        let tracked = TrackedPosition {
            position_id: "IWM|2026-08-14".into(),
            account_hash: "h".into(),
            underlying: "IWM".into(),
            expiry: "2026-08-14".into(),
            strategy: "vertical".into(),
            opened_at: Utc::now(),
            entry_credit: Some(0.28),
            max_loss_usd: 172.0,
            contracts: 1,
            entry_params: None,
            ..Default::default()
        };
        let analytics = compute_vertical_analytics(VerticalAnalyticsInput {
            is_put_spread: true,
            underlying_price: 294.81,
            short_strike: 283.0,
            long_strike: 281.0,
            credit: 0.28,
            dte: 35,
            chain_iv_pct: Some(29.0),
            short_delta: Some(-0.26),
            long_delta: Some(-0.23),
            short_theta: Some(-0.15),
            long_theta: Some(-0.12),
            contracts: 1,
            underlying_change_pct: Some(-0.8),
        });
        let live = SpreadPositionMark {
            mark: SpreadMark {
                entry_credit: 0.28,
                debit_to_close: 0.46,
                profit_pct: -64.3,
                dte: 35,
                source: "test".into(),
            },
            analytics: Some(analytics),
            imminent_exit: None,
            mark_age_secs: Some(0),
        };
        build_spread_monitor(&tracked, Some(&live), &exit_rules)
    }

    #[test]
    fn put_spread_payoff_plateau_and_max_loss() {
        let p_win = vertical_credit_payoff_usd(295.0, true, 283.0, 281.0, 0.28, 1);
        assert!((p_win - 28.0).abs() < 0.01);

        let p_max_loss = vertical_credit_payoff_usd(270.0, true, 283.0, 281.0, 0.28, 1);
        assert!((p_max_loss + 172.0).abs() < 0.01);

        let p_mid = vertical_credit_payoff_usd(282.0, true, 283.0, 281.0, 0.28, 1);
        assert!(p_mid < 0.0 && p_mid > -172.0);
    }

    #[test]
    fn payoff_bounds_available_with_analytics() {
        let m = sample_monitor();
        assert!(payoff_bounds(&m).is_some());
    }

    #[test]
    fn mark_pnl_can_differ_from_expiry_at_spot() {
        let m = sample_monitor();
        let a = m.analytics.as_ref().unwrap();
        let expiry_at_spot = vertical_credit_payoff_usd(
            a.underlying_price,
            a.is_put_spread,
            a.short_strike,
            a.long_strike,
            m.entry_credit,
            m.contracts,
        );
        assert!(expiry_at_spot > 0.0);
        assert!(m.pnl_usd < 0.0);
        let bounds = payoff_bounds(&m).unwrap();
        assert!(bounds.y_min < m.pnl_usd);
        assert!(bounds.y_max > expiry_at_spot);
    }
}
