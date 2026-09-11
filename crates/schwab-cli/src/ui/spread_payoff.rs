//! Expiry payoff (risk graph) for vertical credit spreads and iron condors.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::symbols::Marker;
use ratatui::widgets::canvas::{Canvas, Line};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use super::chart_markers::{draw_chart_marker, marker_spans};
use super::spread_live::SpreadMonitorView;
use super::theme::{self, label_style};
use crate::agent::spread_analytics::SpreadAnalytics;

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

/// P/L in dollars at expiry for a short iron condor (put credit + call credit).
pub fn iron_condor_payoff_usd(
    spot: f64,
    put_short: f64,
    put_long: f64,
    call_short: f64,
    call_long: f64,
    credit: f64,
    contracts: u32,
) -> f64 {
    let put_intrinsic = (put_short - spot).max(0.0) - (put_long - spot).max(0.0);
    let call_intrinsic = (spot - call_short).max(0.0) - (spot - call_long).max(0.0);
    (credit - put_intrinsic - call_intrinsic) * 100.0 * contracts.max(1) as f64
}

/// Strategy-aware expiry P/L from live analytics (or vertical fallback).
pub fn expiry_payoff_usd(spot: f64, m: &SpreadMonitorView) -> Option<f64> {
    let a = m.analytics.as_ref()?;
    let credit = m.entry_credit;
    let contracts = m.contracts;
    if a.is_iron_condor || m.strategy.eq_ignore_ascii_case("iron_condor") {
        let (put_short, put_long, call_short, call_long) = iron_condor_strikes(a)?;
        Some(iron_condor_payoff_usd(
            spot, put_short, put_long, call_short, call_long, credit, contracts,
        ))
    } else {
        Some(vertical_credit_payoff_usd(
            spot,
            a.is_put_spread,
            a.short_strike,
            a.long_strike,
            credit,
            contracts,
        ))
    }
}

fn iron_condor_strikes(a: &SpreadAnalytics) -> Option<(f64, f64, f64, f64)> {
    Some((
        a.put_short?,
        a.put_long?,
        a.call_short?,
        a.call_long?,
    ))
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

    let (lo_strike, hi_strike) = if a.is_iron_condor || m.strategy.eq_ignore_ascii_case("iron_condor")
    {
        let (put_short, put_long, call_short, call_long) = iron_condor_strikes(a)?;
        (
            put_long.min(put_short),
            call_long.max(call_short),
        )
    } else if a.is_put_spread {
        (a.long_strike, a.short_strike)
    } else {
        (a.short_strike, a.long_strike)
    };

    let pad = (a.underlying_price * 0.04).max(width * 1.5);
    let x_min = lo_strike.min(a.underlying_price) - pad;
    let x_max = hi_strike.max(a.underlying_price) + pad;
    let expiry_at_spot = expiry_payoff_usd(a.underlying_price, m)?;
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

    let spot = a.underlying_price;
    let sample_count = 64usize;
    let x_step = (bounds.x_max - bounds.x_min) / sample_count as f64;
    let mut payoff_coords: Vec<(f64, f64)> = Vec::with_capacity(sample_count + 1);
    let mut x = bounds.x_min;
    for _ in 0..=sample_count {
        let Some(y) = expiry_payoff_usd(x, m) else {
            return;
        };
        payoff_coords.push((x, y));
        x += x_step;
    }
    let Some(spot_y) = expiry_payoff_usd(spot, m) else {
        return;
    };
    let now_y = m.pnl_usd;
    let show_expiry_ref = (now_y - spot_y).abs() > 1.0;

    let x_min = bounds.x_min;
    let x_max = bounds.x_max;
    let y_min = bounds.y_min;
    let y_max = bounds.y_max;
    let (hx, hy) = marker_spans(x_min, x_max, y_min, y_max);

    let kind = if a.is_iron_condor || m.strategy.eq_ignore_ascii_case("iron_condor") {
        "IC"
    } else if a.is_put_spread {
        "put"
    } else {
        "call"
    };
    let chart_title = format!(
        " {kind}  +${:.0}/-${:.0}  spot ${:.0}  @expiry ${:+.0}  mtm ${:+.0}",
        bounds.max_profit, bounds.max_loss, spot, spot_y, now_y
    );
    let legend = if show_expiry_ref {
        " ● path@expiry   ○ mtm if closed now "
    } else {
        " ● path@expiry "
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
                    now_y,
                    hx * 0.55,
                    hy * 0.55,
                    Color::Gray,
                    "○",
                );
                ctx.draw(&Line::new(spot, now_y, spot, spot_y, Color::Magenta));
            }

            draw_chart_marker(ctx, spot, spot_y, hx, hy, Color::LightYellow, "●");
        });

    f.render_widget(canvas, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::exits::SpreadMark;
    use crate::agent::spread_analytics::{
        compute_iron_condor_analytics, compute_vertical_analytics, IronCondorAnalyticsInput,
        VerticalAnalyticsInput,
    };
    use crate::agent::state::TrackedPosition;
    use crate::rules::ExitRules;
    use crate::ui::spread_live::{build_spread_monitor, SpreadPositionMark};
    use chrono::Utc;

    fn sample_vertical_monitor() -> SpreadMonitorView {
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
            realized_vol_pct: None,
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
                ..Default::default()
            },
            analytics: Some(analytics),
            imminent_exit: None,
            mark_age_secs: Some(0),
        };
        build_spread_monitor(&tracked, Some(&live), &exit_rules)
    }

    fn sample_condor_monitor() -> SpreadMonitorView {
        let exit_rules = ExitRules::default();
        let tracked = TrackedPosition {
            position_id: "GLD|2026-09-04".into(),
            account_hash: "h".into(),
            underlying: "GLD".into(),
            expiry: "2026-09-04".into(),
            strategy: "iron_condor".into(),
            opened_at: Utc::now(),
            entry_credit: Some(0.62),
            max_loss_usd: 438.0,
            contracts: 1,
            entry_params: None,
            ..Default::default()
        };
        let analytics = compute_iron_condor_analytics(IronCondorAnalyticsInput {
            underlying_price: 370.0,
            put_short: 340.0,
            put_long: 335.0,
            call_short: 403.0,
            call_long: 408.0,
            credit: 0.62,
            dte: 35,
            chain_iv_pct: Some(18.0),
            realized_vol_pct: None,
            put_short_delta: Some(-0.13),
            put_long_delta: Some(-0.10),
            call_short_delta: Some(0.13),
            call_long_delta: Some(0.10),
            put_short_theta: Some(-0.05),
            put_long_theta: Some(-0.04),
            call_short_theta: Some(-0.05),
            call_long_theta: Some(-0.04),
            contracts: 1,
            underlying_change_pct: Some(-2.0),
        });
        let live = SpreadPositionMark {
            mark: SpreadMark {
                entry_credit: 0.62,
                debit_to_close: 0.50,
                profit_pct: 19.4,
                dte: 35,
                source: "test".into(),
                ..Default::default()
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
    fn iron_condor_payoff_plateau_and_wings() {
        // Between shorts: max profit = credit
        let mid = iron_condor_payoff_usd(370.0, 340.0, 335.0, 403.0, 408.0, 0.62, 1);
        assert!((mid - 62.0).abs() < 0.01);

        // Through put wing: max loss
        let put_breach = iron_condor_payoff_usd(330.0, 340.0, 335.0, 403.0, 408.0, 0.62, 1);
        assert!((put_breach + 438.0).abs() < 0.01);

        // Through call wing: max loss
        let call_breach = iron_condor_payoff_usd(410.0, 340.0, 335.0, 403.0, 408.0, 0.62, 1);
        assert!((call_breach + 438.0).abs() < 0.01);

        // Inside put wing: partial loss
        let put_mid = iron_condor_payoff_usd(337.5, 340.0, 335.0, 403.0, 408.0, 0.62, 1);
        assert!(put_mid < 62.0 && put_mid > -438.0);
    }

    #[test]
    fn payoff_bounds_available_with_analytics() {
        assert!(payoff_bounds(&sample_vertical_monitor()).is_some());
        assert!(payoff_bounds(&sample_condor_monitor()).is_some());
    }

    #[test]
    fn condor_bounds_span_both_wings() {
        let m = sample_condor_monitor();
        let bounds = payoff_bounds(&m).unwrap();
        assert!(bounds.x_min < 335.0);
        assert!(bounds.x_max > 408.0);
        let mid = expiry_payoff_usd(370.0, &m).unwrap();
        assert!((mid - 62.0).abs() < 0.01);
    }

    #[test]
    fn mark_pnl_can_differ_from_expiry_at_spot() {
        let m = sample_vertical_monitor();
        let a = m.analytics.as_ref().unwrap();
        let expiry_at_spot = expiry_payoff_usd(a.underlying_price, &m).unwrap();
        assert!(expiry_at_spot > 0.0);
        assert!(m.pnl_usd < 0.0);
        let bounds = payoff_bounds(&m).unwrap();
        assert!(bounds.y_min < m.pnl_usd);
        assert!(bounds.y_max > expiry_at_spot);
    }
}
