//! The research arm rules file: `rules/research/<arm>.yaml`.
//!
//! See `docs/RESEARCH_ARM_PLAN.md` §5 and Appendix C. The arm is paper-only;
//! `validate()` fails closed on anything that could open a live path.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// The only forecast target this arm accepts (plan §0.1).
pub const EXPECTED_TARGET: &str = "forward_5d_excess_return_sign";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ArmConfig {
    pub version: u32,
    pub mode: String,
    pub arm_id: String,
    pub broker: Option<String>,
    pub account: Option<String>,
    pub account_hash: Option<String>,
    pub interval: IntervalConfig,
    pub forecast: ForecastConfig,
    pub research: ResearchConfig,
    pub universe: Vec<String>,
    pub paper: PaperConfig,
    pub adaptation: AdaptationConfig,
}

impl Default for ArmConfig {
    fn default() -> Self {
        Self {
            version: 1,
            mode: "paper".into(),
            arm_id: String::new(),
            broker: Some("public".into()),
            account: None,
            account_hash: None,
            interval: IntervalConfig::default(),
            forecast: ForecastConfig::default(),
            research: ResearchConfig::default(),
            universe: Vec::new(),
            paper: PaperConfig::default(),
            adaptation: AdaptationConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IntervalConfig {
    pub cron: String,
    pub timezone: String,
    pub active_between: Option<Vec<String>>,
}

impl Default for IntervalConfig {
    fn default() -> Self {
        Self {
            cron: "0 10-15 * * 1-5".into(),
            timezone: "America/New_York".into(),
            active_between: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ForecastConfig {
    pub target: String,
    pub benchmark: String,
    pub horizon_days: u32,
    pub min_confidence: f64,
}

impl Default for ForecastConfig {
    fn default() -> Self {
        Self {
            target: EXPECTED_TARGET.into(),
            benchmark: "SPY".into(),
            horizon_days: 5,
            min_confidence: 0.55,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ResearchConfig {
    pub feeds: Vec<FeedConfig>,
    pub search: SearchConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FeedConfig {
    pub id: String,
    pub kind: String,
    pub url: String,
    pub phases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    pub enabled: bool,
    pub provider: String,
    pub engine: String,
    pub model: String,
    pub max_results: u32,
    pub max_total_results: u32,
    pub search_context_size: String,
    pub max_queries_per_tick: u32,
    pub allowed_domains: Vec<String>,
    pub excluded_domains: Vec<String>,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: "openrouter_web_search".into(),
            engine: "auto".into(),
            model: "google/gemini-2.5-flash".into(),
            max_results: 5,
            max_total_results: 20,
            search_context_size: "medium".into(),
            max_queries_per_tick: 4,
            allowed_domains: Vec::new(),
            excluded_domains: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PaperConfig {
    pub starting_equity_usd: f64,
    pub slippage_bps: f64,
    pub max_quote_age_seconds: u64,
    pub max_daily_loss_usd: f64,
}

impl Default for PaperConfig {
    fn default() -> Self {
        Self {
            starting_equity_usd: 10_000.0,
            slippage_bps: 5.0,
            max_quote_age_seconds: 60,
            max_daily_loss_usd: 300.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AdaptationConfig {
    /// Must stay `false` in the research arm. There is no live path here.
    pub live_auto_apply: bool,
    pub allowlist: Vec<String>,
}

impl ArmConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let raw = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("read {}", path.as_ref().display()))?;
        let config: Self = serde_yaml::from_str(&raw)
            .with_context(|| format!("parse {}", path.as_ref().display()))?;
        config.validate()?;
        Ok(config)
    }

    /// The account address. Bare ids are rejected: they default to Schwab in
    /// `broker_core::parse_account_address`, which this arm must never reach.
    pub fn account_address(&self) -> Result<String> {
        if let Some(account) = self.account.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            return Ok(account.to_string());
        }
        if let Some(hash) = self.account_hash.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            bail!(
                "research arm requires `account: public:<id>`; bare hash `{hash}` maps to Schwab and is not permitted"
            );
        }
        bail!("research arm needs `account: public:<id>`")
    }

    /// Fail closed. Every check here is a hard gate, not a warning.
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("unsupported research arm version {}", self.version);
        }
        if !self.mode.eq_ignore_ascii_case("paper") {
            bail!(
                "research arm mode must be `paper` (got `{}`); live is a separate artifact",
                self.mode
            );
        }
        let address = self.account_address()?;
        if !address
            .to_ascii_lowercase()
            .starts_with("public:")
        {
            bail!("research arm account must be `public:<id>` (got `{address}`)");
        }
        if self.arm_id.trim().is_empty() {
            bail!("research arm needs a non-empty `arm_id`");
        }
        if self.forecast.target != EXPECTED_TARGET {
            bail!(
                "research arm forecast target must be `{EXPECTED_TARGET}` (got `{}`)",
                self.forecast.target
            );
        }
        if self.forecast.horizon_days == 0 {
            bail!("research arm forecast.horizon_days must be > 0");
        }
        if self.adaptation.live_auto_apply {
            bail!("research arm refuses `adaptation.live_auto_apply: true`; this arm is paper-only");
        }
        if self.universe.is_empty() {
            bail!("research arm needs a non-empty `universe`");
        }
        if self.interval.cron.split_whitespace().count() != 5 {
            bail!(
                "research arm interval.cron must have 5 fields (got `{}`)",
                self.interval.cron
            );
        }
        if self.research.search.enabled {
            let search = &self.research.search;
            if search.max_results == 0 || search.max_results > 25 {
                bail!("search.max_results must be 1..=25");
            }
            if search.max_total_results < search.max_results {
                bail!("search.max_total_results must be >= max_results");
            }
            if search.max_queries_per_tick == 0 {
                bail!("search.max_queries_per_tick must be >= 1");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> ArmConfig {
        ArmConfig {
            arm_id: "research-megacaps".into(),
            account: Some("public:ACC123".into()),
            universe: vec!["MSFT".into(), "AAPL".into()],
            ..Default::default()
        }
    }

    #[test]
    fn valid_config_passes() {
        valid().validate().unwrap();
    }

    #[test]
    fn live_mode_is_rejected() {
        let mut config = valid();
        config.mode = "live".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn bare_hash_is_rejected() {
        let mut config = valid();
        config.account = None;
        config.account_hash = Some("HASH123".into());
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("bare hash"), "unexpected: {err}");
    }

    #[test]
    fn schwab_account_is_rejected() {
        let mut config = valid();
        config.account = Some("schwab:HASH".into());
        assert!(config.validate().is_err());
    }

    #[test]
    fn live_auto_apply_is_rejected() {
        let mut config = valid();
        config.adaptation.live_auto_apply = true;
        assert!(config.validate().is_err());
    }

    #[test]
    fn wrong_forecast_target_is_rejected() {
        let mut config = valid();
        config.forecast.target = "next_tick_up".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn search_caps_are_enforced() {
        let mut config = valid();
        config.research.search.max_results = 30;
        assert!(config.validate().is_err());
        let mut config = valid();
        config.research.search.max_total_results = 1;
        config.research.search.max_results = 5;
        assert!(config.validate().is_err());
    }
}
