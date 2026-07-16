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
    /// Regime-aware strategy selection (put credit / call credit / iron condor / pause).
    #[serde(default)]
    pub regime: OptionsRegimeConfig,
    #[serde(default)]
    pub execution: ExecutionConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub notify: NotifyConfig,
    /// Paper trading (--simulate): virtual budget separate from live agent state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulation: Option<SimulationConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationConfig {
    /// Virtual risk budget for paper P&L (defaults to risk.max_portfolio_risk_usd).
    pub starting_budget_usd: f64,
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
    #[serde(default)]
    pub overnight: OvernightConfig,
}

/// Low-frequency overnight / pre-market behavior when the option market is closed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OvernightConfig {
    /// When true, the agent keeps running after the close with a slower tick.
    pub enabled: bool,
    /// Seconds between overnight wakes (default 1 hour). LLM digest respects this interval.
    pub tick_interval_seconds: u64,
    /// Run web-model digest to build an open playbook (no chain calls, no entries).
    pub web_digest: bool,
    /// Skip overnight LLM when flat (no open positions).
    pub skip_llm_when_flat: bool,
    /// Telegram only when risk_alerts is non-empty (digest still saved to state).
    pub alert_on_risk_only: bool,
}

impl Default for OvernightConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tick_interval_seconds: 3600,
            web_digest: true,
            skip_llm_when_flat: true,
            alert_on_risk_only: true,
        }
    }
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            tick_interval_seconds: 60,
            market_hours_only: true,
            timezone: "America/New_York".into(),
            overnight: OvernightConfig::default(),
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
    /// Minimum modeled POP vs break-even (percent). Omit to skip.
    #[serde(default)]
    pub min_pop_pct: Option<f64>,
    /// Minimum spot cushion above (puts) or below (calls) break-even as % of spot.
    #[serde(default)]
    pub min_distance_to_be_pct: Option<f64>,
    /// Minimum credit / width ratio (percent). Defaults to 12.5 when omitted.
    #[serde(default)]
    pub min_credit_to_width_pct: Option<f64>,
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
            min_pop_pct: Some(60.0),
            min_distance_to_be_pct: Some(3.0),
            min_credit_to_width_pct: Some(12.5),
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
pub struct ProfitGivebackExit {
    /// Exit when peak unrealized profit reached at least this %.
    pub peak_profit_min_pct: f64,
    /// Exit when current profit falls below this % after the peak threshold was met.
    pub exit_if_below_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThesisExitRules {
    pub enabled: bool,
    /// Skip thesis exits (not profit/stop/DTE) until the position has been open this long.
    pub min_hold_minutes: Option<u32>,
    /// Close when modeled spread POP falls below this (success probability deteriorated).
    pub min_pop_pct_exit: Option<f64>,
    /// Close when |short_delta| reaches this (strike no longer comfortably OTM).
    pub max_short_delta_exit: Option<f64>,
    /// Close when short leg is within this % OTM of spot (pin / chop risk).
    pub min_short_otm_pct: Option<f64>,
    /// Close when short strike sits inside the 1σ expected move toward ITM.
    /// Prefer false for ~5% OTM credit spreads — distance < 1σ is normal at entry.
    pub exit_short_inside_1sigma: bool,
    /// After a thesis exit, skip same-underlying entry scan for this many minutes.
    pub redeploy_cooldown_minutes: Option<u32>,
    pub profit_giveback: Option<ProfitGivebackExit>,
}

impl Default for ThesisExitRules {
    fn default() -> Self {
        Self {
            enabled: false,
            min_hold_minutes: None,
            min_pop_pct_exit: None,
            max_short_delta_exit: None,
            min_short_otm_pct: None,
            exit_short_inside_1sigma: false,
            redeploy_cooldown_minutes: None,
            profit_giveback: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExitRules {
    pub profit_target_pct: f64,
    pub stop_loss_pct: f64,
    pub dte_close: u32,
    pub thesis: ThesisExitRules,
}

impl Default for ExitRules {
    fn default() -> Self {
        Self {
            profit_target_pct: 50.0,
            stop_loss_pct: 200.0,
            dte_close: 21,
            thesis: ThesisExitRules::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RiskConfig {
    pub max_portfolio_risk_usd: f64,
    pub max_risk_per_trade_usd: f64,
    /// Soft churn cap. Prefer `max_open_positions` + portfolio/trade risk as hard gates.
    /// `0` = unlimited (no daily trade-count pause).
    pub max_trades_per_day: u32,
    pub allowed_underlyings: Vec<String>,
    /// Optional per-symbol open-slot caps (e.g. SPY: 2, IWM: 1). Keys are case-insensitive.
    #[serde(default)]
    pub max_open_per_underlying: std::collections::HashMap<String, u32>,
    pub blocked_events: Vec<String>,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_portfolio_risk_usd: 10_000.0,
            max_risk_per_trade_usd: 2_000.0,
            max_trades_per_day: 3,
            allowed_underlyings: vec!["SPY".into(), "QQQ".into(), "IWM".into()],
            max_open_per_underlying: std::collections::HashMap::new(),
            blocked_events: vec![],
        }
    }
}

impl RiskConfig {
    pub fn max_open_for_underlying(&self, underlying: &str) -> Option<u32> {
        self.max_open_per_underlying
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(underlying))
            .map(|(_, v)| *v)
    }
}

/// Maps market regime → preferred options structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OptionsRegimeConfig {
    pub enabled: bool,
    pub benchmark_symbol: String,
    pub vix_symbol: String,
    pub vix_low: f64,
    pub vix_high: f64,
    /// Pause all new entries when VIX is at or above this (hostile / crash regime).
    pub pause_entries_vix_above: f64,
    /// regime class → preferred strategy: `put_credit`, `call_credit`, `iron_condor`, `pause`.
    #[serde(default)]
    pub strategy_map: std::collections::HashMap<String, String>,
}

impl Default for OptionsRegimeConfig {
    fn default() -> Self {
        let mut strategy_map = std::collections::HashMap::new();
        strategy_map.insert("low_vol_trend".into(), "put_credit".into());
        strategy_map.insert("elevated_vol".into(), "put_credit".into());
        strategy_map.insert("high_vol_chop".into(), "iron_condor".into());
        strategy_map.insert("bearish_trend".into(), "call_credit".into());
        strategy_map.insert("hostile".into(), "pause".into());
        strategy_map.insert("neutral".into(), "put_credit".into());
        Self {
            enabled: false,
            benchmark_symbol: "SPY".into(),
            vix_symbol: "$VIX".into(),
            vix_low: 14.0,
            vix_high: 25.0,
            pause_entries_vix_above: 30.0,
            strategy_map,
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
    OvernightDigest,
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
    /// Run LLM review every N agent ticks when flat (no open positions).
    /// With open positions, `effective_monitor_review_ticks` applies to both selection and monitor.
    pub review_every_ticks: u64,
    /// Monitor-phase interval when open spreads are above `dte_close` (long-dated / low gamma).
    /// Falls back to `review_every_ticks` when unset or when any position is in the gamma window.
    #[serde(default)]
    pub monitor_review_every_ticks: Option<u64>,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// System instructions for overnight web digest (market closed).
    pub overnight: String,
    /// Extra context for overnight digest user message.
    pub overnight_context: String,
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
     and macro risk via web context. When candidate_entries is non-empty, ground strike and \
     greek analysis in each candidate's market_context — web research supplements but does \
     not replace chain fields. Be conservative: defer or skip if event or volatility \
     risk outweighs the premium collected."
}

/// Appended to every selection-phase system prompt (custom YAML included).
pub fn selection_market_context_guardrails() -> &'static str {
    "CHAIN DATA GUARDRAILS (binding):\n\
     - candidate_entries[] are built only after a successful Schwab chain fetch. Each item \
     includes market_context (underlying_price, short_delta, chain_iv, spread_pop_pct, \
     break_even_price, expected_move_1sigma, credit_to_width_pct, DTE, etc.).\n\
     - ivr_available: false means IV Rank is not provided — NOT missing chain data. Use chain_iv.\n\
     - FORBIDDEN defer/skip reasons: \"lack of live chain data\", \"missing greeks\", \
     \"no IV data\", or similar vague claims when market_context has underlying_price AND \
     short_delta.\n\
     - If you believe data is incomplete, cite the exact null/missing JSON field (e.g. \
     \"short_theta is null\") and do not veto solely for absent IV Rank, theta, or gamma.\n\
     - Defer or skip based on macro, event risk, poor premium/delta/strike placement, or \
     extreme chain_iv — not invented data gaps.\n\
     - If candidate_entries is empty, recommend skip with \"no mechanical candidates\" — do \
     not claim chain API failure."
}

pub fn default_monitor_prompt() -> &'static str {
    "You are monitoring existing open option spreads. Mechanical exits (profit target, \
     stop loss, DTE) run every tick without you — do not duplicate those rules.\n\
     Each open_positions[] item includes mechanical_rules (stop_debit_threshold_per_share, \
     current_debit_to_close, stop_triggered) and market_context (greeks, OTM distance).\n\
     CRITICAL: Never use net_market_value for stop-loss or profit-target decisions — it is \
     Schwab leg market value in dollars, not per-share debit_to_close. Only cite a stop hit \
     in risk_alerts when mechanical_rules.stop_triggered is true. If status is holding and \
     stop_triggered is false, the position has NOT hit the mechanical stop.\n\
     Early in a 30-45 DTE trade, mark-to-market swings are normal; theta needs time.\n\
     Use market_context for recommendations:\n\
     - hold: thesis intact, short leg comfortably OTM (typically |short_delta| < 0.30, \
     short_otm_pct > 3% for put credits)\n\
     - watch: elevated delta (|short_delta| >= 0.30), price within ~2% of short strike, \
     or developing macro/event risk\n\
     - close: thesis broken (recommendation only; mechanical stop handles P/L) — use \
     urgency high only for imminent assignment/gap risk through short strike\n\
     For 30-45 DTE income trades: keep market_commentary to 1-2 sentences unless \
     delta, POP, P/L, or event risk changed materially since the last review. Do not \
     repeat overnight playbook themes when mechanical_rules show no triggered exit.\n\
     If market_context is missing but market_context_error is set, rely on mechanical_rules \
     and recommend hold unless mechanical_rules indicate a triggered exit.\n\
     For new_entries during monitor phase: recommend proceed only when candidate_entries is \
     non-empty; otherwise use skip with brief reasoning.\n\
     Do not recommend close for routine profit — mechanics handle 50% target."
}

pub fn default_overnight_prompt() -> &'static str {
    "The US options market is CLOSED. Research overnight and pre-market news (futures, \
     macro, geopolitical, scheduled data) affecting the watchlist and open positions. \
     Build a concise OPEN PLAYBOOK for the next session: what to watch at the bell, \
     whether any open spread thesis is broken, and suggested actions at the open \
     (hold, close at market, or wait). Do NOT recommend opening new trades overnight. \
     For new_entries always recommend skip. Only flag high-urgency risk_alerts for \
     thesis-breaking developments."
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

    pub fn effective_overnight_instructions(&self) -> &str {
        if !self.overnight.is_empty() {
            return &self.overnight;
        }
        default_overnight_prompt()
    }

    pub fn effective_context(&self, phase: LlmPhase) -> &str {
        match phase {
            LlmPhase::Selection => &self.selection_context,
            LlmPhase::Monitor => &self.monitor_context,
            LlmPhase::OvernightDigest => &self.overnight_context,
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            selection_model: "anthropic/claude-sonnet-4".into(),
            monitor_model: "google/gemini-2.5-flash".into(),
            web_model: "perplexity/sonar".into(),
            model: String::new(),
            review_every_ticks: 5,
            monitor_review_every_ticks: None,
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
            "anthropic/claude-sonnet-4"
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
            LlmPhase::OvernightDigest => &self.web_model,
        }
    }

    /// Monitor LLM cadence: slower above gamma window (DTE > dte_close), faster inside it.
    pub fn effective_monitor_review_ticks(
        &self,
        min_open_dte: Option<i64>,
        dte_close: u32,
    ) -> u64 {
        let fast = self.review_every_ticks.max(1);
        let slow = self.monitor_review_every_ticks.unwrap_or(30).max(1);
        match min_open_dte {
            Some(dte) if dte > dte_close as i64 => slow,
            _ => fast,
        }
    }

    /// Shared cadence for selection and monitor LLM reviews.
    pub fn effective_llm_review_ticks(
        &self,
        has_open_positions: bool,
        min_open_dte: Option<i64>,
        dte_close: u32,
    ) -> u64 {
        if has_open_positions {
            self.effective_monitor_review_ticks(min_open_dte, dte_close)
        } else {
            self.review_every_ticks.max(1)
        }
    }

    pub fn monitor_interval_minutes(
        &self,
        tick_interval_seconds: u64,
        min_open_dte: Option<i64>,
        dte_close: u32,
    ) -> u64 {
        let secs = self
            .effective_monitor_review_ticks(min_open_dte, dte_close)
            .saturating_mul(tick_interval_seconds.max(1));
        (secs / 60).max(1)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    pub telegram: TelegramNotifyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramNotifyConfig {
    pub enabled: bool,
    /// Notify on every tick summary (can be noisy).
    pub notify_every_tick: bool,
    /// Notify on fills, exits, and LLM updates (when urgency/digest rules allow).
    pub notify_on_actions: bool,
    /// Send routine LLM status digests (same recommendation is not repeated).
    pub llm_notify_digest: bool,
    /// Minimum minutes between routine LLM digests (0 = urgent-only).
    pub llm_digest_interval_minutes: u64,
    /// Send immediately when LLM says proceed, high urgency, or urgent close.
    pub llm_notify_urgent: bool,
    /// Do not repeat the same urgent LLM message within this many minutes.
    pub llm_urgent_cooldown_minutes: u64,
}

impl Default for TelegramNotifyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            notify_every_tick: false,
            notify_on_actions: true,
            llm_notify_digest: true,
            llm_digest_interval_minutes: 60,
            llm_notify_urgent: true,
            llm_urgent_cooldown_minutes: 30,
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
        if !self.schedule.market_hours_only {
            anyhow::bail!("options agent requires schedule.market_hours_only=true");
        }
        if !self.execution.order_type.eq_ignore_ascii_case("limit") {
            anyhow::bail!("options agent requires execution.order_type=limit");
        }
        if !self.execution.require_preview {
            anyhow::bail!("options agent requires execution.require_preview=true");
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
                    "timezone": { "type": "string" },
                    "overnight": {
                        "type": "object",
                        "properties": {
                            "enabled": { "type": "boolean" },
                            "tick_interval_seconds": { "type": "integer", "minimum": 300 },
                            "web_digest": { "type": "boolean" },
                            "skip_llm_when_flat": { "type": "boolean" },
                            "alert_on_risk_only": { "type": "boolean" }
                        }
                    }
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
            regime: OptionsRegimeConfig::default(),
            execution: ExecutionConfig::default(),
            llm: LlmConfig::default(),
            notify: NotifyConfig::default(),
            simulation: None,
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
            "anthropic/claude-sonnet-4"
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
        let prompts = LlmPromptsConfig {
            selection: "Aggressive premium seller.".into(),
            ..Default::default()
        };
        assert!(prompts
            .effective_selection_instructions(false)
            .contains("Aggressive"));
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

    #[test]
    fn selection_guardrails_are_documented() {
        let g = selection_market_context_guardrails();
        assert!(g.contains("FORBIDDEN"));
        assert!(g.contains("underlying_price"));
        assert!(g.contains("ivr_available"));
    }

    #[test]
    fn monitor_review_slower_above_gamma_window() {
        let llm = LlmConfig {
            review_every_ticks: 5,
            monitor_review_every_ticks: Some(45),
            ..Default::default()
        };
        assert_eq!(llm.effective_monitor_review_ticks(Some(30), 21), 45);
        assert_eq!(llm.effective_monitor_review_ticks(Some(18), 21), 5);
    }

    #[test]
    fn llm_review_ticks_use_monitor_cadence_with_open_positions() {
        let llm = LlmConfig {
            review_every_ticks: 5,
            monitor_review_every_ticks: Some(45),
            ..Default::default()
        };
        assert_eq!(llm.effective_llm_review_ticks(false, None, 21), 5);
        assert_eq!(llm.effective_llm_review_ticks(true, Some(30), 21), 45);
        assert_eq!(llm.effective_llm_review_ticks(true, Some(18), 21), 5);
    }
}
