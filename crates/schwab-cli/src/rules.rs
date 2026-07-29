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
    /// Symbols to scan for entries, in priority order. Each entry may be a ticker string or
    /// `{ symbol, role, min_credit, ... }` with per-symbol overrides.
    #[serde(default)]
    pub watchlist: Vec<WatchlistEntry>,
    #[serde(default)]
    pub entry_policy: EntryPolicyConfig,
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

/// `string` or `{ symbol, role, min_credit, ... }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum WatchlistEntry {
    Symbol(String),
    Item(WatchlistItemConfig),
}

impl From<&str> for WatchlistEntry {
    fn from(value: &str) -> Self {
        Self::Symbol(value.to_string())
    }
}

impl From<String> for WatchlistEntry {
    fn from(value: String) -> Self {
        Self::Symbol(value)
    }
}

impl WatchlistEntry {
    pub fn to_item(&self) -> WatchlistItemConfig {
        match self {
            Self::Symbol(s) => WatchlistItemConfig {
                symbol: s.clone(),
                ..Default::default()
            },
            Self::Item(item) => item.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WatchlistItemConfig {
    pub symbol: String,
    #[serde(default)]
    pub role: WatchlistRole,
    #[serde(flatten)]
    pub overrides: VerticalEntryOverrides,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WatchlistRole {
    #[default]
    Primary,
    Fallback,
}

/// Per-symbol vertical entry overrides (merged onto `entry_rules.vertical`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct VerticalEntryOverrides {
    pub min_credit: Option<f64>,
    pub max_width: Option<f64>,
    pub short_delta_min: Option<f64>,
    pub short_delta_max: Option<f64>,
    pub min_pop_pct: Option<f64>,
    pub min_distance_to_be_pct: Option<f64>,
    pub min_credit_to_width_pct: Option<f64>,
}

impl VerticalEntryOverrides {
    pub fn apply_to(&self, mut base: VerticalEntryRules) -> VerticalEntryRules {
        if let Some(v) = self.min_credit {
            base.min_credit = v;
        }
        if let Some(v) = self.max_width {
            base.max_width = v;
        }
        if let Some(v) = self.short_delta_min {
            base.short_delta_min = v;
        }
        if let Some(v) = self.short_delta_max {
            base.short_delta_max = v;
        }
        if let Some(v) = self.min_pop_pct {
            base.min_pop_pct = Some(v);
        }
        if let Some(v) = self.min_distance_to_be_pct {
            base.min_distance_to_be_pct = Some(v);
        }
        if let Some(v) = self.min_credit_to_width_pct {
            base.min_credit_to_width_pct = Some(v);
        }
        base
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryScanMode {
    /// Stop after the first symbol (in watchlist order) that produces a candidate.
    FirstQualifying,
    /// Legacy: queue every symbol that passes mechanical gates.
    AllQualifying,
}

impl Default for EntryScanMode {
    fn default() -> Self {
        Self::FirstQualifying
    }
}

fn default_entry_attempt_cooldown_minutes() -> u32 {
    30
}

fn default_proceed_cache_minutes() -> u32 {
    45
}

/// Config-driven entry scan behavior (watchlist order, LLM gate, retry limits).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EntryPolicyConfig {
    pub mode: EntryScanMode,
    /// When true, `fallback` role symbols are scanned only if no `primary` produced a candidate.
    pub fallback_only_after_primary_exhausted: bool,
    /// Minutes before retrying the same candidate after a non-fill entry attempt.
    pub entry_attempt_cooldown_minutes: u32,
    /// When true, after thesis redeploy cooldown the exited symbol is scanned first.
    pub promote_redeploy_symbol: bool,
    /// Require LLM `proceed` before live entries (uses proceed_cache_minutes between reviews).
    pub require_llm_proceed: bool,
    pub proceed_cache_minutes: u32,
}

impl Default for EntryPolicyConfig {
    fn default() -> Self {
        Self {
            mode: EntryScanMode::FirstQualifying,
            fallback_only_after_primary_exhausted: true,
            entry_attempt_cooldown_minutes: default_entry_attempt_cooldown_minutes(),
            promote_redeploy_symbol: false,
            require_llm_proceed: true,
            proceed_cache_minutes: default_proceed_cache_minutes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ReEntryAfterStopLoss {
    pub enabled: bool,
    pub cooldown_days: u32,
}

impl Default for ReEntryAfterStopLoss {
    fn default() -> Self {
        Self {
            enabled: true,
            cooldown_days: 5,
        }
    }
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
    /// Reject candidates whose short strike sits inside the 1σ expected move.
    /// Entry-only (unlike `exit_rules.thesis.exit_short_inside_1sigma`). Prefer with
    /// farther OTM deltas (~0.10–0.16) so some strikes still clear the gate.
    /// When enabled and chain IV is missing, the gate **fails closed** (rejects).
    #[serde(default)]
    pub reject_short_inside_1sigma: bool,
    /// Minimum chain IV / realized-vol ratio. Omit to skip.
    /// When set, missing IV or RV **fails closed** (rejects). Typical starting value: 1.15.
    #[serde(default)]
    pub min_iv_rv_ratio: Option<f64>,
    /// Minimum short-strike OTM cushion (% of spot). Omit to skip.
    #[serde(default)]
    pub min_short_otm_pct: Option<f64>,
    /// Reject when the underlying has already moved against the credit structure today
    /// by more than this many percent (puts: down day; calls: up day). Omit to skip.
    #[serde(default)]
    pub max_adverse_day_change_pct: Option<f64>,
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
            // Prefer quality shorts; widen only explicitly in rules YAML.
            short_delta_min: 0.10,
            short_delta_max: 0.20,
            min_pop_pct: Some(65.0),
            min_distance_to_be_pct: Some(4.0),
            min_credit_to_width_pct: Some(12.5),
            reject_short_inside_1sigma: false,
            min_iv_rv_ratio: None,
            min_short_otm_pct: None,
            max_adverse_day_change_pct: None,
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
    /// Same meaning as `VerticalEntryRules::min_iv_rv_ratio` (fail-closed when set).
    #[serde(default)]
    pub min_iv_rv_ratio: Option<f64>,
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
            min_iv_rv_ratio: None,
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

/// Defensive roll when a credit vertical hits the mechanical stop.
/// Close then reopen farther OTM / later DTE (two sequential orders). Off by default.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RollConfig {
    pub enabled: bool,
    /// Minimum DTE remaining on the tested position to attempt a roll.
    pub min_dte_remaining: u32,
    /// Short must still be at least this % OTM (do not roll already-ITM / near-ITM shorts).
    pub min_short_otm_pct: f64,
    /// Prefer expiry at least this many calendar days beyond the closed DTE.
    pub min_dte_extension: u32,
    /// Cap |short_delta| on the replacement (also tightened vs original short delta − 0.04).
    pub target_short_delta_max: f64,
    /// Allow net debit up to this % of entry credit: `new_credit - close_debit >= -entry * pct/100`.
    pub max_debit_pct_of_entry_credit: f64,
    /// Max successful rolls for a single lineage (`TrackedPosition.rolls_used`).
    pub max_rolls_per_position: u32,
    /// Soft account-wide cap on successful rolls per calendar day.
    pub max_rolls_per_day: u32,
}

impl Default for RollConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_dte_remaining: 21,
            min_short_otm_pct: 0.5,
            min_dte_extension: 7,
            target_short_delta_max: 0.12,
            max_debit_pct_of_entry_credit: 25.0,
            max_rolls_per_position: 1,
            max_rolls_per_day: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExitRules {
    pub profit_target_pct: f64,
    pub stop_loss_pct: f64,
    /// Arm the credit-multiple stop only when short OTM% is below this.
    /// While the short is farther OTM than this cushion, ignore mark-based stops
    /// (profit target, DTE, and thesis exits still apply). `None` = always arm (legacy).
    #[serde(default)]
    pub stop_loss_require_short_otm_below_pct: Option<f64>,
    pub dte_close: u32,
    pub thesis: ThesisExitRules,
    /// Prefer a managed roll over a hard stop when eligible (verticals only).
    #[serde(default)]
    pub roll: RollConfig,
}

impl Default for ExitRules {
    fn default() -> Self {
        Self {
            profit_target_pct: 50.0,
            stop_loss_pct: 200.0,
            stop_loss_require_short_otm_below_pct: None,
            dte_close: 21,
            thesis: ThesisExitRules::default(),
            roll: RollConfig::default(),
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
    #[serde(default)]
    pub re_entry_after_stop_loss: ReEntryAfterStopLoss,
    /// Manual hard pause: any non-empty list blocks **all** new entries until cleared.
    /// Not a calendar — use `blocked_dates` for FOMC/CPI/NFP blackouts.
    pub blocked_events: Vec<String>,
    /// Date-aware entry blackout (macro/Fed calendar). Blocks when
    /// `event_date - lead_days <= today <= event_date` (inclusive).
    #[serde(default)]
    pub blocked_dates: Vec<BlockedDate>,
    /// Correlated underlyings — cap concurrent open positions per group.
    #[serde(default)]
    pub correlation_groups: Vec<CorrelationGroupConfig>,
    /// Halt new entries when sleeve drawdown from HWM reaches this % (e.g. 15.0).
    /// `null` / omit / `0` = disabled. Exits always continue.
    #[serde(default)]
    pub max_drawdown_halt_pct: Option<f64>,
    /// Sleeve equity base for drawdown HWM. Defaults to `max_portfolio_risk_usd`
    /// (or sim starting budget). Set to the real options cash sleeve (e.g. 4000).
    #[serde(default)]
    pub drawdown_sleeve_usd: Option<f64>,
}

/// One calendar blackout for new option entries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockedDate {
    /// Event date `YYYY-MM-DD` (America/New_York calendar day).
    pub date: String,
    /// Block starting this many calendar days before `date` (0 = event day only).
    #[serde(default)]
    pub lead_days: u32,
    /// Human label for skip messages / tick JSON (e.g. `FOMC`, `CPI`).
    #[serde(default)]
    pub label: String,
}

/// Correlated symbols — cap concurrent open positions per group.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CorrelationGroupConfig {
    pub name: String,
    pub symbols: Vec<String>,
    /// Max open positions from this group at once (`0` = no cap for this group).
    pub max_open: u32,
}

impl Default for CorrelationGroupConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            symbols: vec![],
            max_open: 1,
        }
    }
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_portfolio_risk_usd: 10_000.0,
            max_risk_per_trade_usd: 2_000.0,
            max_trades_per_day: 3,
            allowed_underlyings: vec!["SPY".into(), "QQQ".into(), "IWM".into()],
            max_open_per_underlying: std::collections::HashMap::new(),
            re_entry_after_stop_loss: ReEntryAfterStopLoss::default(),
            blocked_events: vec![],
            blocked_dates: vec![],
            correlation_groups: vec![],
            max_drawdown_halt_pct: None,
            drawdown_sleeve_usd: None,
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

    /// Labels of `blocked_dates` entries active on `today` (inclusive lead window).
    pub fn active_blocked_date_labels(&self, today: chrono::NaiveDate) -> Vec<String> {
        self.blocked_dates
            .iter()
            .filter_map(|b| {
                let event = chrono::NaiveDate::parse_from_str(b.date.trim(), "%Y-%m-%d").ok()?;
                let start = event - chrono::Duration::days(b.lead_days as i64);
                if today >= start && today <= event {
                    let label = if b.label.trim().is_empty() {
                        b.date.clone()
                    } else {
                        format!("{} ({})", b.label.trim(), b.date)
                    };
                    Some(label)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Group containing `underlying`, if any.
    pub fn correlation_group_for(&self, underlying: &str) -> Option<&CorrelationGroupConfig> {
        self.correlation_groups.iter().find(|g| {
            g.symbols
                .iter()
                .any(|s| s.eq_ignore_ascii_case(underlying))
        })
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
    /// Pause all new entries when VIX is at or below this (crushed-vol / cheap insurance).
    /// Omit or null to disable. Independent of `vix_low` (classification only).
    #[serde(default)]
    pub pause_entries_vix_below: Option<f64>,
    /// Fail-closed: pause new entries when the VIX quote is unavailable.
    /// Without this, all VIX-based gates silently pass when the quote misses.
    #[serde(default = "default_pause_on_missing_vix")]
    pub pause_on_missing_vix: bool,
    /// Lookback days for realized-vol used by `min_iv_rv_ratio` entry gates.
    #[serde(default = "default_realized_vol_lookback")]
    pub realized_vol_lookback: usize,
    /// regime class → preferred strategy: `put_credit`, `call_credit`, `iron_condor`, `pause`.
    #[serde(default)]
    pub strategy_map: std::collections::HashMap<String, String>,
}

fn default_realized_vol_lookback() -> usize {
    20
}

fn default_pause_on_missing_vix() -> bool {
    true
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
            pause_entries_vix_below: None,
            pause_on_missing_vix: true,
            realized_vol_lookback: default_realized_vol_lookback(),
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
    /// Broker-resident GTC profit-target close order, placed after each live entry fill.
    /// Schwab rejects stop triggers on multi-leg option orders, so this only covers the
    /// profit-target side of exit protection — stop-loss/DTE-close remain mechanical
    /// (see docs/OPTIONS_RULES.md).
    #[serde(default)]
    pub protective_order: ProtectiveOrderConfig,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            order_type: "limit".into(),
            require_preview: true,
            wait_for_fill: true,
            fill_timeout_seconds: 300,
            protective_order: ProtectiveOrderConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ProtectiveOrderConfig {
    pub enabled: bool,
    /// Placement attempts before giving up (the reconcile pass keeps retrying afterward).
    pub max_attempts: u32,
    pub max_seconds: u64,
}

impl Default for ProtectiveOrderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_attempts: 3,
            max_seconds: 30,
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

    pub fn watchlist_items(&self) -> Vec<WatchlistItemConfig> {
        self.watchlist.iter().map(WatchlistEntry::to_item).collect()
    }

    pub fn watchlist_symbols(&self) -> Vec<String> {
        self.watchlist_items()
            .into_iter()
            .map(|i| i.symbol.to_uppercase())
            .collect()
    }

    pub fn effective_vertical_entry(&self, symbol: &str) -> VerticalEntryRules {
        let base = self.entry_rules.vertical.clone();
        if let Some(item) = self
            .watchlist_items()
            .into_iter()
            .find(|i| i.symbol.eq_ignore_ascii_case(symbol))
        {
            return item.overrides.apply_to(base);
        }
        base
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
            "watchlist": {
                "type": "array",
                "items": {
                    "oneOf": [
                        { "type": "string" },
                        {
                            "type": "object",
                            "required": ["symbol"],
                            "properties": {
                                "symbol": { "type": "string" },
                                "role": { "enum": ["primary", "fallback"] },
                                "min_credit": { "type": "number" },
                                "min_credit_to_width_pct": { "type": "number" }
                            }
                        }
                    ]
                }
            },
            "entry_policy": { "type": "object" },
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
    fn watchlist_item_overrides_merge_into_vertical_entry() {
        let yaml = r#"
version: 1
agent_id: t
accounts:
  - hash: ABC
    enabled: true
watchlist:
  - symbol: SPY
    role: primary
    min_credit: 0.12
  - symbol: IWM
    role: fallback
    min_credit: 0.30
entry_rules:
  vertical:
    min_credit: 0.25
"#;
        let rules: RulesConfig = serde_yaml::from_str(yaml).unwrap();
        assert!((rules.effective_vertical_entry("SPY").min_credit - 0.12).abs() < f64::EPSILON);
        assert!((rules.effective_vertical_entry("IWM").min_credit - 0.30).abs() < f64::EPSILON);
        assert_eq!(rules.watchlist_items()[1].role, WatchlistRole::Fallback);
    }

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
            watchlist: vec![WatchlistEntry::from("SPY")],
            entry_policy: EntryPolicyConfig::default(),
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
    fn blocked_dates_respect_lead_window() {
        let risk = RiskConfig {
            blocked_dates: vec![
                BlockedDate {
                    date: "2026-09-16".into(),
                    lead_days: 1,
                    label: "FOMC".into(),
                },
                BlockedDate {
                    date: "2026-09-11".into(),
                    lead_days: 0,
                    label: "CPI".into(),
                },
            ],
            ..Default::default()
        };
        let sep15 = chrono::NaiveDate::from_ymd_opt(2026, 9, 15).unwrap();
        let sep16 = chrono::NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
        let sep14 = chrono::NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let sep11 = chrono::NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let sep10 = chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();

        assert!(risk
            .active_blocked_date_labels(sep15)
            .iter()
            .any(|l| l.contains("FOMC")));
        assert!(risk
            .active_blocked_date_labels(sep16)
            .iter()
            .any(|l| l.contains("FOMC")));
        assert!(risk.active_blocked_date_labels(sep14).is_empty());
        assert!(risk
            .active_blocked_date_labels(sep11)
            .iter()
            .any(|l| l.contains("CPI")));
        assert!(risk.active_blocked_date_labels(sep10).is_empty());
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
