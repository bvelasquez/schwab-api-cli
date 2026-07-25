//! Black–Scholes helpers for synthetic option marks.

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

/// Credit vertical mid: short premium − long premium (both same right).
pub fn vertical_credit(
    is_put: bool,
    spot: f64,
    short_strike: f64,
    long_strike: f64,
    dte: i64,
    iv_pct: f64,
) -> f64 {
    let t = years_from_dte(dte);
    let sigma = (iv_pct / 100.0).max(0.01);
    let short = bs_price(is_put, spot, short_strike, t, 0.0, sigma);
    let long = bs_price(is_put, spot, long_strike, t, 0.0, sigma);
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
) -> f64 {
    vertical_credit(is_put, spot, short_strike, long_strike, dte, iv_pct)
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
        let c = vertical_credit(true, 500.0, 480.0, 475.0, 35, 18.0);
        assert!(c > 0.05, "credit={c}");
    }
}
