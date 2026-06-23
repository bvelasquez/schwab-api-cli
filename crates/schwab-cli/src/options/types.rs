use serde::{Deserialize, Serialize};

/// Vertical spread parameters (LLM / rules facing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerticalParams {
    pub underlying: String,
    pub expiry: String,
    /// put_credit | put_debit | call_credit | call_debit
    #[serde(rename = "type")]
    pub spread_type: String,
    pub short_strike: f64,
    pub long_strike: f64,
    pub contracts: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_credit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_debit: Option<f64>,
    #[serde(default)]
    pub duration: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
}

/// Iron condor parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IronCondorParams {
    pub underlying: String,
    pub expiry: String,
    pub put_short: f64,
    pub put_long: f64,
    pub call_short: f64,
    pub call_long: f64,
    pub contracts: f64,
    pub limit_credit: f64,
    #[serde(default)]
    pub duration: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyKind {
    Vertical,
    IronCondor,
}

impl StrategyKind {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "vertical" | "vert" => Ok(Self::Vertical),
            "iron_condor" | "iron-condor" | "condor" => Ok(Self::IronCondor),
            other => anyhow::bail!("unknown strategy `{other}` (use vertical or iron_condor)"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vertical => "vertical",
            Self::IronCondor => "iron_condor",
        }
    }
}
