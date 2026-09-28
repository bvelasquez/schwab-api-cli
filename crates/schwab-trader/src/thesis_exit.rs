//! Deterministic thesis-deterioration exits for swing positions.

use crate::agent::state::SwingPosition;
use crate::rules::TraderRules;
use crate::technical::TechnicalSnapshot;

/// Tracks both MFE (`peak_profit_pct`) and MAE (`trough_profit_pct`) from the
/// same unrealized-pnl sample so callers only need one update per tick/bar.
pub fn update_peak_and_trough_profit_pct(position: &mut SwingPosition, last: f64) {
    if position.entry_price <= f64::EPSILON || last <= 0.0 {
        return;
    }
    let profit_pct = ((last / position.entry_price) - 1.0) * 100.0;
    let peak = position.peak_profit_pct.unwrap_or(profit_pct);
    position.peak_profit_pct = Some(peak.max(profit_pct));
    let trough = position.trough_profit_pct.unwrap_or(profit_pct);
    position.trough_profit_pct = Some(trough.min(profit_pct));
}

pub fn is_thesis_exit_reason(reason: &str) -> bool {
    reason.starts_with("thesis_")
}

pub fn thesis_exit_reason(
    rules: &TraderRules,
    pos: &SwingPosition,
    last: f64,
    snap: &TechnicalSnapshot,
    regime_class: Option<&str>,
) -> Option<&'static str> {
    let thesis = &rules.playbook.exit.thesis;
    if !thesis.enabled || last <= 0.0 || pos.entry_price <= f64::EPSILON {
        return None;
    }

    let profit_pct = ((last / pos.entry_price) - 1.0) * 100.0;
    let peak = pos.peak_profit_pct.unwrap_or(profit_pct);

    if let Some(gb) = &thesis.profit_giveback {
        // Fraction-of-peak floor (when configured) locks in proportionally
        // more of a bigger run; the fixed pct is the absolute floor.
        let floor = match gb.peak_fraction {
            Some(f) if f > 0.0 => gb.exit_if_below_pct.max(peak * f),
            _ => gb.exit_if_below_pct,
        };
        if peak >= gb.peak_profit_min_pct && profit_pct < floor {
            return Some("thesis_profit_giveback");
        }
    }

    if profit_pct >= thesis.exit_if_below_sma_min_peak_profit_pct {
        for period in &thesis.exit_if_below_sma {
            let below = match period {
                9 => snap.above_sma_9 == Some(false),
                20 => snap.above_sma_20 == Some(false),
                50 => snap.above_sma_50 == Some(false),
                _ => false,
            };
            if below {
                return Some("thesis_below_sma");
            }
        }
    }

    if let Some(threshold) = thesis.exit_on_rs_deterioration_below {
        let rs = snap
            .history_features
            .as_ref()
            .and_then(|h| h.rs_vs_benchmark_30d_pct);
        if let Some(rs) = rs {
            if rs < threshold {
                return Some("thesis_rs_deterioration");
            }
        } else if let Some(entry_rs) = pos.entry_rs_vs_benchmark_30d {
            if entry_rs >= threshold {
                return Some("thesis_rs_deterioration");
            }
        }
    }

    if profit_pct >= thesis.regime_exit_min_profit_pct {
        if let Some(class) = regime_class {
            if thesis
                .regime_exit_profiles
                .iter()
                .any(|p| p.eq_ignore_ascii_case(class))
            {
                return Some("thesis_regime");
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_features::HistoryFeatures;

    fn sample_pos() -> SwingPosition {
        SwingPosition {
            position_id: "AAPL|2026-01-01".into(),
            symbol: "AAPL".into(),
            account_hash: "ACC".into(),
            quantity: 1.0,
            entry_price: 100.0,
            opened_at: chrono::Utc::now(),
            stop_price: 95.0,
            profit_limit: 108.0,
            stop_risk_usd: 5.0,
            market_value_usd: 100.0,
            oco_order_id: Some("123".into()),
            exit_plan_version: 1,
            peak_profit_pct: Some(8.0),
            trough_profit_pct: None,
            entry_rs_vs_benchmark_30d: Some(2.0),
        }
    }

    fn rules_with_thesis() -> TraderRules {
        let mut rules = TraderRules::default();
        rules.playbook.exit.thesis.enabled = true;
        rules.playbook.exit.thesis.profit_giveback = Some(crate::rules::ProfitGivebackExit {
            peak_profit_min_pct: 6.0,
            exit_if_below_pct: 2.0,
            peak_fraction: None,
        });
        rules
    }

    #[test]
    fn peak_and_trough_track_independently() {
        let mut pos = sample_pos();
        pos.peak_profit_pct = None;
        pos.trough_profit_pct = None;

        update_peak_and_trough_profit_pct(&mut pos, 105.0); // +5%
        assert!((pos.peak_profit_pct.unwrap() - 5.0).abs() < 0.01);
        assert!((pos.trough_profit_pct.unwrap() - 5.0).abs() < 0.01);

        update_peak_and_trough_profit_pct(&mut pos, 110.0); // +10% new peak
        assert!((pos.peak_profit_pct.unwrap() - 10.0).abs() < 0.01);
        assert!((pos.trough_profit_pct.unwrap() - 5.0).abs() < 0.01); // trough unmoved

        update_peak_and_trough_profit_pct(&mut pos, 98.0); // -2% new trough, peak unaffected
        assert!((pos.peak_profit_pct.unwrap() - 10.0).abs() < 0.01);
        assert!((pos.trough_profit_pct.unwrap() - (-2.0)).abs() < 0.01);
    }

    #[test]
    fn peak_and_trough_ignore_invalid_inputs() {
        let mut pos = sample_pos();
        pos.peak_profit_pct = Some(3.0);
        pos.trough_profit_pct = Some(-1.0);
        update_peak_and_trough_profit_pct(&mut pos, 0.0);
        assert_eq!(pos.peak_profit_pct, Some(3.0));
        assert_eq!(pos.trough_profit_pct, Some(-1.0));

        pos.entry_price = 0.0;
        update_peak_and_trough_profit_pct(&mut pos, 100.0);
        assert_eq!(pos.peak_profit_pct, Some(3.0));
        assert_eq!(pos.trough_profit_pct, Some(-1.0));
    }

    #[test]
    fn profit_giveback_triggers() {
        let rules = rules_with_thesis();
        let pos = sample_pos();
        let snap = TechnicalSnapshot {
            symbol: "AAPL".into(),
            last: 101.5,
            bid: None,
            ask: None,
            spread_pct: None,
            sma_9: None,
            sma_20: None,
            sma_50: None,
            rsi_14: None,
            atr_14: None,
            volume_sma_20: None,
            relative_volume: None,
            above_sma_9: None,
            above_sma_20: None,
            above_sma_50: None,
            intraday: false,
            history_features: None,
            last_earnings_date: None,
            estimated_next_earnings: None,
            days_until_estimated_earnings: None,
            earnings_estimate_confidence: None,
        };
        assert_eq!(
            thesis_exit_reason(&rules, &pos, 101.5, &snap, None),
            Some("thesis_profit_giveback")
        );
    }

    #[test]
    fn giveback_peak_fraction_locks_more_of_big_runs() {
        let mut rules = rules_with_thesis();
        rules.playbook.exit.thesis.profit_giveback = Some(crate::rules::ProfitGivebackExit {
            peak_profit_min_pct: 4.0,
            exit_if_below_pct: 2.0,
            peak_fraction: Some(0.5),
        });
        let snap = TechnicalSnapshot {
            symbol: "AAPL".into(),
            last: 0.0,
            bid: None,
            ask: None,
            spread_pct: None,
            sma_9: None,
            sma_20: None,
            sma_50: None,
            rsi_14: None,
            atr_14: None,
            volume_sma_20: None,
            relative_volume: None,
            above_sma_9: None,
            above_sma_20: None,
            above_sma_50: None,
            intraday: false,
            history_features: None,
            last_earnings_date: None,
            estimated_next_earnings: None,
            days_until_estimated_earnings: None,
            earnings_estimate_confidence: None,
        };
        let mut pos = sample_pos();
        pos.peak_profit_pct = Some(8.0);
        // Floor = max(2%, 8% × 0.5) = 4% → 3.5% current exits…
        assert_eq!(
            thesis_exit_reason(&rules, &pos, 103.5, &snap, None),
            Some("thesis_profit_giveback")
        );
        // …while 4.5% current holds.
        assert_eq!(thesis_exit_reason(&rules, &pos, 104.5, &snap, None), None);
        // Small peak (5%): floor = max(2%, 2.5%) = 2.5% → 2.2% exits.
        pos.peak_profit_pct = Some(5.0);
        assert_eq!(
            thesis_exit_reason(&rules, &pos, 102.2, &snap, None),
            Some("thesis_profit_giveback")
        );
    }

    #[test]
    fn rs_deterioration_triggers() {
        let mut rules = rules_with_thesis();
        rules.playbook.exit.thesis.profit_giveback = None;
        rules.playbook.exit.thesis.exit_on_rs_deterioration_below = Some(-4.0);
        let pos = sample_pos();
        let snap = TechnicalSnapshot {
            symbol: "AAPL".into(),
            last: 99.0,
            bid: None,
            ask: None,
            spread_pct: None,
            sma_9: None,
            sma_20: None,
            sma_50: None,
            rsi_14: None,
            atr_14: None,
            volume_sma_20: None,
            relative_volume: None,
            above_sma_9: None,
            above_sma_20: Some(true),
            above_sma_50: None,
            intraday: false,
            history_features: Some(HistoryFeatures {
                return_30d_pct: Some(-2.0),
                pct_from_52w_high: Some(-10.0),
                rs_vs_benchmark_30d_pct: Some(-5.0),
                ..Default::default()
            }),
            last_earnings_date: None,
            estimated_next_earnings: None,
            days_until_estimated_earnings: None,
            earnings_estimate_confidence: None,
        };
        assert_eq!(
            thesis_exit_reason(&rules, &pos, 99.0, &snap, None),
            Some("thesis_rs_deterioration")
        );
    }
}
