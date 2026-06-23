use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const RULES_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesConfig {
    pub version: u32,
    pub agent_id: String,
    #[serde(default)]
    pub accounts: Vec<RulesAccount>,
    #[serde(default)]
    pub schedule: ScheduleConfig,
    #[serde(default)]
    pub strategies: StrategiesToggle,
    #[serde(default)]
    pub watchlist: Vec<String>,
    #[serde(default)]
    pub entry_rules: EntryRules,
    #[serde(default)]
    pub exit_rules: ExitRules,
    #[serde(default)]
    pub risk: RiskConfig,
    #[serde(default)]
    pub execution: ExecutionConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub notify: NotifyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulesAccount {
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub r#type: AccountType,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccountType {
    #[default]
    Margin,
    Ira,
    Cash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleConfig {
    pub tick_interval_seconds: u64,
    pub market_hours_only: bool,
    pub timezone: String,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            tick_interval_seconds: 60,
            market_hours_only: true,
            timezone: "America/New_York".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StrategiesToggle {
    #[serde(default)]
    pub vertical: StrategyEnabled,
    #[serde(default)]
    pub iron_condor: StrategyEnabled,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StrategyEnabled {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntryRules {
    #[serde(default)]
    pub vertical: VerticalEntryRules,
    #[serde(default)]
    pub iron_condor: IronCondorEntryRules,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VerticalEntryRules {
    pub r#type: String,
    pub dte_min: u32,
    pub dte_max: u32,
    pub min_credit: f64,
    pub max_width: f64,
    pub short_delta_min: f64,
    pub short_delta_max: f64,
    pub max_open_positions: u32,
    pub max_contracts_per_trade: u32,
}

impl Default for VerticalEntryRules {
    fn default() -> Self {
        Self {
            r#type: "put_credit".into(),
            dte_min: 30,
            dte_max: 45,
            min_credit: 0.50,
            max_width: 5.0,
            short_delta_min: 0.15,
            short_delta_max: 0.30,
            max_open_positions: 3,
            max_contracts_per_trade: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IronCondorEntryRules {
    pub dte_min: u32,
    pub dte_max: u32,
    pub min_credit: f64,
    pub wing_width: f64,
    pub short_delta: f64,
    pub max_open_positions: u32,
    pub max_contracts_per_trade: u32,
}

impl Default for IronCondorEntryRules {
    fn default() -> Self {
        Self {
            dte_min: 30,
            dte_max: 45,
            min_credit: 1.00,
            wing_width: 5.0,
            short_delta: 0.16,
            max_open_positions: 2,
            max_contracts_per_trade: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExitRules {
    pub profit_target_pct: f64,
    pub stop_loss_pct: f64,
    pub dte_close: u32,
}

impl Default for ExitRules {
    fn default() -> Self {
        Self {
            profit_target_pct: 50.0,
            stop_loss_pct: 200.0,
            dte_close: 21,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RiskConfig {
    pub max_portfolio_risk_usd: f64,
    pub max_risk_per_trade_usd: f64,
    pub max_trades_per_day: u32,
    pub allowed_underlyings: Vec<String>,
    pub blocked_events: Vec<String>,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_portfolio_risk_usd: 10_000.0,
            max_risk_per_trade_usd: 2_000.0,
            max_trades_per_day: 3,
            allowed_underlyings: vec!["SPY".into(), "QQQ".into(), "IWM".into()],
            blocked_events: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionConfig {
    pub order_type: String,
    pub require_preview: bool,
    pub wait_for_fill: bool,
    pub fill_timeout_seconds: u64,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            order_type: "limit".into(),
            require_preview: true,
            wait_for_fill: true,
            fill_timeout_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmPhase {
    Selection,
    Monitor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    /// Enable OpenRouter LLM reviews during agent ticks.
    pub enabled: bool,
    /// High-intelligence model for entry veto when rules produce candidate trades.
    pub selection_model: String,
    /// Cost-efficient model for periodic open-position reviews.
    pub monitor_model: String,
    /// Model with web search for macro/event context (selection phase, periodic).
    pub web_model: String,
    /// Legacy fallback if selection_model / monitor_model are empty in old configs.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// Run monitor-phase LLM every N agent ticks (selection always runs when signaled).
    pub review_every_ticks: u64,
    /// Use web_model every N selection/monitor LLM reviews (when applicable).
    pub web_research_every_reviews: u64,
    pub max_tokens: u32,
    /// When true, LLM can veto new entries when it recommends defer/skip.
    pub veto_entries: bool,
    /// When true, high-urgency LLM close recommendations trigger exits.
    pub allow_llm_exits: bool,
    /// Per-phase role, instructions, and strategy context (configurable per rules file).
    #[serde(default)]
    pub prompts: LlmPromptsConfig,
}

/// Configurable LLM instructions per agent strategy. Empty fields use built-in defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmPromptsConfig {
    /// System instructions for entry/selection phase (role, risk posture, what to optimize).
    pub selection: String,
    /// Optional override when web_model is used for selection; falls back to `selection`.
    pub selection_web: String,
    /// Extra strategy context prepended to the user message during selection.
    pub selection_context: String,
    /// System instructions for open-position monitoring phase.
    pub monitor: String,
    /// Extra strategy context prepended to the user message during monitoring.
    pub monitor_context: String,
}

impl Default for LlmPromptsConfig {
    fn default() -> Self {
        Self {
            selection: String::new(),
            selection_web: String::new(),
            selection_context: String::new(),
            monitor: String::new(),
            monitor_context: String::new(),
        }
    }
}

pub fn default_selection_prompt() -> &'static str {
    "You are an expert options income trader specializing in defined-risk credit spreads \
     and iron condors. You are evaluating whether to OPEN new spreads found by deterministic \
     rules. Analyze candidate_entries for credit vs width, delta, timing, portfolio risk, \
     and event risk. Be conservative: recommend defer or skip unless the setup is clearly \
     favorable within the strategy context provided."
}

pub fn default_selection_web_prompt() -> &'static str {
    "You are an expert options income trader evaluating whether to OPEN new spreads. \
     Research current market conditions, upcoming events (FOMC, CPI, earnings), IV regime, \
     and macro risk via web context. Be conservative: defer or skip if event or volatility \
     risk outweighs the premium collected."
}

pub fn default_monitor_prompt() -> &'static str {
    "You are monitoring existing open option spreads. Mechanical exits (profit target, \
     stop loss, DTE) run every tick without you — do not duplicate those rules. Review \
     progress, flag watch items, and note any developing risks. For new_entries, recommend \
     proceed unless adding risk would violate the strategy context."
}

impl LlmPromptsConfig {
    pub fn effective_selection_instructions(&self, use_web: bool) -> &str {
        if use_web {
            if !self.selection_web.is_empty() {
                return &self.selection_web;
            }
            if !self.selection.is_empty() {
                return &self.selection;
            }
            return default_selection_web_prompt();
        }
        if !self.selection.is_empty() {
            return &self.selection;
        }
        default_selection_prompt()
    }

    pub fn effective_monitor_instructions(&self) -> &str {
        if !self.monitor.is_empty() {
            return &self.monitor;
        }
        default_monitor_prompt()
    }

    pub fn effective_context(&self, phase: LlmPhase) -> &str {
        match phase {
            LlmPhase::Selection => &self.selection_context,
            LlmPhase::Monitor => &self.monitor_context,
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            selection_model: "anthropic/claude-3.5-sonnet".into(),
            monitor_model: "google/gemini-2.5-flash".into(),
            web_model: "perplexity/sonar".into(),
            model: String::new(),
            review_every_ticks: 5,
            web_research_every_reviews: 3,
            max_tokens: 2000,
            veto_entries: true,
            allow_llm_exits: false,
            prompts: LlmPromptsConfig::default(),
        }
    }
}

impl LlmConfig {
    pub fn effective_selection_model(&self) -> &str {
        if !self.selection_model.is_empty() {
            &self.selection_model
        } else if !self.model.is_empty() {
            &self.model
        } else {
            "anthropic/claude-3.5-sonnet"
        }
    }

    pub fn effective_monitor_model(&self) -> &str {
        if !self.monitor_model.is_empty() {
            &self.monitor_model
        } else if !self.model.is_empty() {
            &self.model
        } else {
            "google/gemini-2.5-flash"
        }
    }

    /// Resolve which OpenRouter model to call for this phase.
    pub fn resolve_model(&self, phase: LlmPhase, use_web: bool) -> &str {
        if use_web {
            return &self.web_model;
        }
        match phase {
            LlmPhase::Selection => self.effective_selection_model(),
            LlmPhase::Monitor => self.effective_monitor_model(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    pub telegram: TelegramNotifyConfig,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            telegram: TelegramNotifyConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramNotifyConfig {
    pub enabled: bool,
    /// Notify on every tick summary (can be noisy).
    pub notify_every_tick: bool,
    /// Always notify on entries, exits, and LLM alerts.
    pub notify_on_actions: bool,
}

impl Default for TelegramNotifyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            notify_every_tick: false,
            notify_on_actions: true,
        }
    }
}

impl RulesConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("read rules file {}", path.display()))?;
        let rules: RulesConfig = if path.extension().is_some_and(|e| e == "json") {
            serde_json::from_str(&content)?
        } else {
            serde_yaml::from_str(&content)?
        };
        rules.validate()?;
        Ok(rules)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != RULES_VERSION {
            anyhow::bail!(
                "unsupported rules version {} (expected {})",
                self.version,
                RULES_VERSION
            );
        }
        if self.agent_id.trim().is_empty() {
            anyhow::bail!("agent_id is required");
        }
        if self.accounts.is_empty() {
            anyhow::bail!("at least one account is required");
        }
        for acct in &self.accounts {
            if acct.hash.trim().is_empty() {
                anyhow::bail!("account hash is required");
            }
        }
        if self.watchlist.is_empty() {
            anyhow::bail!("watchlist must not be empty");
        }
        Ok(())
    }

    pub fn enabled_accounts(&self) -> impl Iterator<Item = &RulesAccount> {
        self.accounts.iter().filter(|a| a.enabled)
    }
}

pub fn rules_json_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Schwab options agent rules",
        "type": "object",
        "required": ["version", "agent_id", "accounts", "watchlist"],
        "properties": {
            "version": { "const": 1 },
            "agent_id": { "type": "string" },
            "accounts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["hash"],
                    "properties": {
                        "hash": { "type": "string" },
                        "label": { "type": "string" },
                        "type": { "enum": ["margin", "ira", "cash"] },
                        "enabled": { "type": "boolean" }
                    }
                }
            },
            "schedule": {
                "type": "object",
                "properties": {
                    "tick_interval_seconds": { "type": "integer", "minimum": 5 },
                    "market_hours_only": { "type": "boolean" },
                    "timezone": { "type": "string" }
                }
            },
            "strategies": {
                "type": "object",
                "properties": {
                    "vertical": { "type": "object", "properties": { "enabled": { "type": "boolean" } } },
                    "iron_condor": { "type": "object", "properties": { "enabled": { "type": "boolean" } } }
                }
            },
            "watchlist": { "type": "array", "items": { "type": "string" } },
            "entry_rules": { "type": "object" },
            "exit_rules": { "type": "object" },
            "risk": { "type": "object" },
            "execution": { "type": "object" }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_minimal_rules() {
        let rules = RulesConfig {
            version: 1,
            agent_id: "test".into(),
            accounts: vec![RulesAccount {
                hash: "ABC".into(),
                label: None,
                r#type: AccountType::Margin,
                enabled: true,
            }],
            schedule: ScheduleConfig::default(),
            strategies: StrategiesToggle::default(),
            watchlist: vec!["SPY".into()],
            entry_rules: EntryRules::default(),
            exit_rules: ExitRules::default(),
            risk: RiskConfig::default(),
            execution: ExecutionConfig::default(),
            llm: LlmConfig::default(),
            notify: NotifyConfig::default(),
        };
        rules.validate().unwrap();
    }

    #[test]
    fn loads_example_rules_yaml() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../rules/options-rules.example.yaml");
        if path.exists() {
            let rules = RulesConfig::load(&path).unwrap();
            assert_eq!(rules.agent_id, "spy-income-v1");
        }
    }

    #[test]
    fn llm_config_resolves_phase_models() {
        let cfg = LlmConfig::default();
        assert_eq!(
            cfg.resolve_model(LlmPhase::Selection, false),
            "anthropic/claude-3.5-sonnet"
        );
        assert_eq!(
            cfg.resolve_model(LlmPhase::Monitor, false),
            "google/gemini-2.5-flash"
        );
        assert_eq!(
            cfg.resolve_model(LlmPhase::Selection, true),
            "perplexity/sonar"
        );
    }

    #[test]
    fn custom_selection_prompt_overrides_default() {
        let mut prompts = LlmPromptsConfig::default();
        prompts.selection = "Aggressive premium seller.".into();
        assert!(prompts.effective_selection_instructions(false).contains("Aggressive"));
    }

    #[test]
    fn selection_web_prompt_used_when_set() {
        let prompts = LlmPromptsConfig {
            selection: "conservative".into(),
            selection_web: "web aggressive".into(),
            ..Default::default()
        };
        assert_eq!(
            prompts.effective_selection_instructions(true),
            "web aggressive"
        );
    }
}
