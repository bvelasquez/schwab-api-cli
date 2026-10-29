//! The forecast contract (plan §0.1, Appendix D).
//!
//! Target: `forward_5d_excess_return_sign` vs the configured benchmark.
//! `expected_return_pct` and `confidence` are emitted for ranking/sizing but are
//! not the pass/fail target — cross-sectional rank IC is.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::patch::Patch;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Forecast {
    pub arm_id: String,
    pub as_of: String,
    pub target: String,
    #[serde(default = "default_benchmark")]
    pub benchmark: String,
    pub horizon_days: u32,
    #[serde(default)]
    pub signals: Vec<Signal>,
    #[serde(default)]
    pub regime: String,
    #[serde(default)]
    pub plan_patch: Vec<Patch>,
    #[serde(default)]
    pub risk_alerts: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

fn default_benchmark() -> String {
    "SPY".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub symbol: String,
    pub direction: String,
    #[serde(default)]
    pub expected_return_pct: f64,
    pub confidence: f64,
    #[serde(default)]
    pub rank: u32,
    #[serde(default)]
    pub citations: Vec<String>,
}

impl Forecast {
    /// Structural validation. Semantic/cost gates live elsewhere.
    pub fn validate(&self) -> Result<()> {
        if self.arm_id.trim().is_empty() {
            bail!("forecast.arm_id is empty");
        }
        if self.target != crate::config::EXPECTED_TARGET {
            bail!(
                "forecast.target must be `{}` (got `{}`)",
                crate::config::EXPECTED_TARGET,
                self.target
            );
        }
        if self.horizon_days == 0 {
            bail!("forecast.horizon_days must be > 0");
        }
        if self.signals.is_empty() {
            bail!("forecast has no signals");
        }
        let mut seen = std::collections::BTreeSet::new();
        for signal in &self.signals {
            if signal.symbol.trim().is_empty() {
                bail!("forecast signal has an empty symbol");
            }
            if !seen.insert(signal.symbol.to_ascii_uppercase()) {
                bail!("forecast signal `{}` is duplicated", signal.symbol);
            }
            match signal.direction.as_str() {
                "up" | "down" | "flat" => {}
                other => bail!("signal `{}` has unknown direction `{other}`", signal.symbol),
            }
            if !(0.0..=1.0).contains(&signal.confidence) {
                bail!(
                    "signal `{}` confidence {} is outside 0..=1",
                    signal.symbol,
                    signal.confidence
                );
            }
        }
        for patch in &self.plan_patch {
            if patch.path.trim().is_empty() {
                bail!("forecast plan_patch has an empty path");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Forecast {
        Forecast {
            arm_id: "research-megacaps".into(),
            as_of: "2026-10-28T14:00:00Z".into(),
            target: crate::config::EXPECTED_TARGET.into(),
            benchmark: "SPY".into(),
            horizon_days: 5,
            signals: vec![Signal {
                symbol: "MSFT".into(),
                direction: "up".into(),
                expected_return_pct: 0.8,
                confidence: 0.6,
                rank: 1,
                citations: vec!["search:1".into()],
            }],
            regime: "risk_on".into(),
            plan_patch: vec![],
            risk_alerts: vec![],
            notes: String::new(),
        }
    }

    #[test]
    fn valid_forecast_passes() {
        sample().validate().unwrap();
    }

    #[test]
    fn duplicate_symbol_is_rejected() {
        let mut f = sample();
        f.signals.push(f.signals[0].clone());
        assert!(f.validate().is_err());
    }

    #[test]
    fn bad_confidence_is_rejected() {
        let mut f = sample();
        f.signals[0].confidence = 1.5;
        assert!(f.validate().is_err());
    }

    #[test]
    fn bad_direction_is_rejected() {
        let mut f = sample();
        f.signals[0].direction = "moon".into();
        assert!(f.validate().is_err());
    }

    #[test]
    fn wrong_target_is_rejected() {
        let mut f = sample();
        f.target = "next_tick_up".into();
        assert!(f.validate().is_err());
    }
}
