//! Black–Scholes helpers for synthetic option marks.

use crate::rules::RulesConfig;

/// Backtest pricing calibration: symbol IV level scaling + put OTM skew add-on.
/// `FLAT` reproduces the legacy flat-VIX-IV, no-skew model exactly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PricingCalibration {
    pub iv_multiplier: f64,
    pub put_skew_per_10_delta_pts: f64,
}

impl PricingCalibration {
    pub const FLAT: Self = Self {
        iv_multiplier: 1.0,
        put_skew_per_10_delta_pts: 0.0,
    };

    /// Resolve from `simulation.backtest_pricing` for `symbol`; missing config or a
    /// symbol absent from `iv_multiplier_by_symbol` fall back to `FLAT`'s multiplier.
    pub fn for_symbol(rules: &RulesConfig, symbol: &str) -> Self {
        let Some(cfg) = rules
            .simulation
            .as_ref()
            .and_then(|s| s.backtest_pricing.as_ref())
        else {
            return Self::FLAT;
        };
        Self {
            iv_multiplier: cfg
                .iv_multiplier_by_symbol
                .get(&symbol.trim().to_uppercase())
                .copied()
                .unwrap_or(1.0),
            put_skew_per_10_delta_pts: cfg.put_skew_per_10_delta_pts,
        }
    }
}

/// Standard normal CDF (Abramowitz & Stegun approximation).
pub fn norm_cdf(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs() / (2.0_f64).sqrt();
    let t = 1.0 / (1.0 + p * x);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-x * x).exp();
    0.5 * (1.0 + sign * y)
}

fn d1_d2(spot: f64, strike: f64, t: f64, r: f64, sigma: f64) -> Option<(f64, f64)> {
    if spot <= 0.0 || strike <= 0.0 || t <= 0.0 || sigma <= 0.0 {
        return None;
    }
    let vol_sqrt_t = sigma * t.sqrt();
    if vol_sqrt_t <= f64::EPSILON {
        return None;
    }
    let d1 = ((spot / strike).ln() + (r + 0.5 * sigma * sigma) * t) / vol_sqrt_t;
    let d2 = d1 - vol_sqrt_t;
    Some((d1, d2))
}

/// European option mid price. `is_put` selects put vs call. `sigma` is decimal vol (0.20 = 20%).
pub fn bs_price(is_put: bool, spot: f64, strike: f64, t_years: f64, r: f64, sigma: f64) -> f64 {
    if t_years <= 0.0 {
        return intrinsic(is_put, spot, strike);
    }
    let Some((d1, d2)) = d1_d2(spot, strike, t_years, r, sigma) else {
        return intrinsic(is_put, spot, strike);
    };
    let disc = (-r * t_years).exp();
    if is_put {
        (strike * disc * norm_cdf(-d2) - spot * norm_cdf(-d1)).max(0.0)
    } else {
        (spot * norm_cdf(d1) - strike * disc * norm_cdf(d2)).max(0.0)
    }
}

pub fn bs_delta(is_put: bool, spot: f64, strike: f64, t_years: f64, r: f64, sigma: f64) -> f64 {
    if t_years <= 0.0 {
        return if is_put {
            if spot < strike {
                -1.0
            } else {
                0.0
            }
        } else if spot > strike {
            1.0
        } else {
            0.0
        };
    }
    let Some((d1, _)) = d1_d2(spot, strike, t_years, r, sigma) else {
        return 0.0;
    };
    if is_put {
        norm_cdf(d1) - 1.0
    } else {
        norm_cdf(d1)
    }
}

fn intrinsic(is_put: bool, spot: f64, strike: f64) -> f64 {
    if is_put {
        (strike - spot).max(0.0)
    } else {
        (spot - strike).max(0.0)
    }
}

pub fn years_from_dte(dte: i64) -> f64 {
    (dte.max(0) as f64) / 365.0
}

/// Symbol-scaled IV (decimal) from a base IV% (e.g. VIX close) and a `PricingCalibration`
/// multiplier. Delta targeting and gates use this (unskewed) level — skew is a
/// per-leg pricing add-on only, not a redefinition of "16 delta".
pub fn scaled_sigma(iv_pct: f64, iv_multiplier: f64) -> f64 {
    (iv_pct / 100.0 * iv_multiplier).max(0.01)
}

/// Per-leg vol after the (puts-only) OTM skew add-on: `put_skew_per_10_delta_pts` IV
/// points for every 10 delta this strike sits further OTM than 50-delta, anchored on
/// the unskewed `base_sigma` delta (skew is a small add-on, so the unskewed delta is
/// an accurate enough anchor — this is a calibration knob, not a real vol surface).
/// Calls, or a zero skew knob, return `base_sigma` unchanged.
fn leg_sigma_with_skew(
    is_put: bool,
    spot: f64,
    strike: f64,
    t_years: f64,
    base_sigma: f64,
    put_skew_per_10_delta_pts: f64,
) -> f64 {
    if !is_put || put_skew_per_10_delta_pts.abs() < f64::EPSILON {
        return base_sigma;
    }
    let delta_pct = bs_delta(is_put, spot, strike, t_years, 0.0, base_sigma).abs() * 100.0;
    let otm_10s = (50.0 - delta_pct).max(0.0) / 10.0;
    (base_sigma + put_skew_per_10_delta_pts * otm_10s / 100.0).max(0.01)
}

/// Credit vertical mid: short premium − long premium, each leg priced with its own IV
/// per `calib` (symbol multiplier + put skew). `PricingCalibration::FLAT` reproduces
/// the legacy single flat-IV price exactly.
pub fn vertical_credit(
    is_put: bool,
    spot: f64,
    short_strike: f64,
    long_strike: f64,
    dte: i64,
    iv_pct: f64,
    calib: PricingCalibration,
) -> f64 {
    let t = years_from_dte(dte);
    let base_sigma = scaled_sigma(iv_pct, calib.iv_multiplier);
    let short_sigma = leg_sigma_with_skew(
        is_put,
        spot,
        short_strike,
        t,
        base_sigma,
        calib.put_skew_per_10_delta_pts,
    );
    let long_sigma = leg_sigma_with_skew(
        is_put,
        spot,
        long_strike,
        t,
        base_sigma,
        calib.put_skew_per_10_delta_pts,
    );
    let short = bs_price(is_put, spot, short_strike, t, 0.0, short_sigma);
    let long = bs_price(is_put, spot, long_strike, t, 0.0, long_sigma);
    (short - long).max(0.01)
}

/// Debit to close a short credit vertical ≈ same formula as credit at current marks.
pub fn vertical_debit_to_close(
    is_put: bool,
    spot: f64,
    short_strike: f64,
    long_strike: f64,
    dte: i64,
    iv_pct: f64,
    calib: PricingCalibration,
) -> f64 {
    vertical_credit(is_put, spot, short_strike, long_strike, dte, iv_pct, calib)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atm_call_has_near_half_delta() {
        let d = bs_delta(false, 100.0, 100.0, 30.0 / 365.0, 0.0, 0.20);
        assert!((d - 0.5).abs() < 0.05, "delta={d}");
    }

    #[test]
    fn put_credit_positive() {
        let c = vertical_credit(true, 500.0, 480.0, 475.0, 35, 18.0, PricingCalibration::FLAT);
        assert!(c > 0.05, "credit={c}");
    }

    #[test]
    fn flat_calibration_reproduces_legacy_single_iv_price() {
        // Old behavior: both legs priced at the same flat sigma from `iv_pct`.
        let (spot, short_strike, long_strike, dte, iv_pct) = (500.0, 474.0, 469.0, 35, 18.0);
        let t = years_from_dte(dte);
        let sigma = (iv_pct / 100.0_f64).max(0.01);
        let legacy = (bs_price(true, spot, short_strike, t, 0.0, sigma)
            - bs_price(true, spot, long_strike, t, 0.0, sigma))
        .max(0.01);
        let calibrated = vertical_credit(
            true,
            spot,
            short_strike,
            long_strike,
            dte,
            iv_pct,
            PricingCalibration::FLAT,
        );
        assert!((legacy - calibrated).abs() < 1e-12, "legacy={legacy} calibrated={calibrated}");
    }

    #[test]
    fn put_skew_raises_credit_vs_flat_iv_at_same_strikes() {
        // 16-delta-ish short / 5-wide long on SPY-scale strikes; realistic skew knob.
        let (spot, short_strike, long_strike, dte, iv_pct) = (500.0, 474.0, 469.0, 35, 18.0);
        let flat = vertical_credit(true, spot, short_strike, long_strike, dte, iv_pct, PricingCalibration::FLAT);
        let skewed = vertical_credit(
            true,
            spot,
            short_strike,
            long_strike,
            dte,
            iv_pct,
            PricingCalibration {
                iv_multiplier: 1.0,
                put_skew_per_10_delta_pts: 1.5,
            },
        );
        assert!(skewed > flat, "flat={flat} skewed={skewed}");
    }

    #[test]
    fn iv_multiplier_scales_credit_up() {
        let (spot, short_strike, long_strike, dte, iv_pct) = (500.0, 474.0, 469.0, 35, 18.0);
        let base = vertical_credit(true, spot, short_strike, long_strike, dte, iv_pct, PricingCalibration::FLAT);
        let scaled = vertical_credit(
            true,
            spot,
            short_strike,
            long_strike,
            dte,
            iv_pct,
            PricingCalibration {
                iv_multiplier: 1.25,
                put_skew_per_10_delta_pts: 0.0,
            },
        );
        assert!(scaled > base, "base={base} scaled={scaled}");
    }
}
