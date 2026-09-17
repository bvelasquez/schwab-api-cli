use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::adaptation::PlaybookProfileOverrides;
use crate::market_cache::MarketCacheConfig;

pub const RULES_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraderRules {
    pub version: u32,
    pub trader_id: String,
    #[serde(default)]
    pub accounts: Vec<TraderAccount>,
    #[serde(default)]
    pub capital: CapitalConfig,
    #[serde(default)]
    pub schedule: ScheduleConfig,
    #[serde(default)]
    pub playbook: PlaybookConfig,
    #[serde(default)]
    pub watchlists: WatchlistsConfig,
    #[serde(default)]
    pub sources: SourcesConfig,
    #[serde(default)]
    pub technical: TechnicalConfig,
    #[serde(default)]
    pub risk: RiskConfig,
    #[serde(default)]
    pub execution: ExecutionConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub notify: NotifyConfig,
    #[serde(default)]
    pub simulation: Option<SimulationConfig>,
    #[serde(default)]
    pub adaptation: AdaptationConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdaptationConfig {
    /// Master switch for regime profiles, monitor adjustments, and profile selection.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// When false (default), live ticks journal proposed patches but do not write YAML.
    pub live_auto_apply: bool,
    /// Pick profile from mechanical regime each tick.
    pub regime_auto_select: bool,
    /// Allow LLM to override active profile via profile_selection in reviews.
    pub llm_profile_select: bool,
    pub regime: RegimeConfig,
    pub monitor_adjustments: MonitorAdjustmentsConfig,
    /// Named playbook override sets (baseline uses playbook section as-is).
    pub profiles: HashMap<String, TradingProfile>,
    /// Regime class → profile name.
    pub profile_map: HashMap<String, String>,
    pub default_profile: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingProfile {
    pub description: String,
    #[serde(default)]
    pub overrides: PlaybookProfileOverrides,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RegimeConfig {
    pub enabled: bool,
    pub benchmark_symbol: String,
    pub vix_symbol: String,
    pub vix_low: f64,
    pub vix_high: f64,
    pub realized_vol_lookback: usize,
    pub realized_vol_history: usize,
    pub realized_vol_high_percentile: f64,
    /// Consecutive regime detections that must agree before the active profile
    /// switches. Hysteresis against VIX hovering near a threshold (e.g. VIX
    /// oscillating around vix_low flipping low_vol_trend ↔ elevated_vol several
    /// times a day, which also flips stop/target geometry mid-trade).
    /// 20 ticks ≈ 30 min at a 90s tick interval. 0 = switch immediately.
    pub profile_switch_min_dwell_ticks: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorAdjustmentsConfig {
    pub enabled: bool,
    /// Max stop raise per tighten_exits (percent of entry→stop distance).
    pub max_tighten_pct: f64,
    /// Max stop lower per widen_exits (percent of entry→stop distance).
    pub max_widen_pct: f64,
    /// Do not tighten stop closer than this % below last price.
    pub min_stop_distance_from_price_pct: f64,
    /// Do not widen stop further than this % below last price.
    pub max_stop_distance_from_price_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationConfig {
    /// Paper portfolio starting cash (defaults to capital.fixed_sleeve_cap_usd)
    pub starting_cash_usd: f64,
    /// Allow LLM rule adaptation from simulated trade outcomes
    #[serde(default = "default_true")]
    pub allow_rule_adaptation: bool,
    /// Allow fractional share sizing in simulation/backtest (Schwab supports fractional)
    #[serde(default)]
    pub allow_fractional_shares: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraderAccount {
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
pub struct CapitalConfig {
    pub fixed_sleeve_cap_usd: f64,
    pub max_pct_of_free_cash: f64,
    pub min_cash_floor_usd: f64,
    pub options_risk: OptionsRiskConfig,
    pub core_holdings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OptionsRiskConfig {
    /// Options agent rules files whose open/pending risk is reserved from free cash.
    /// Prefer this over the legacy singular `rules_file`.
    #[serde(default)]
    pub rules_files: Vec<String>,
    /// Legacy singular path (still accepted). Merged into `rules_files` at read time.
    #[serde(default)]
    pub rules_file: String,
    pub fallback_reserve_usd: f64,
    pub buffer_pct: f64,
}

impl OptionsRiskConfig {
    /// Deduped list of options rules paths (plural + legacy singular).
    pub fn all_rules_files(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .rules_files
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let singular = self.rules_file.trim();
        if !singular.is_empty()
            && !out
                .iter()
                .any(|p| p.eq_ignore_ascii_case(singular))
        {
            out.push(singular.to_string());
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleConfig {
    pub tick_interval_seconds: u64,
    pub market_hours_only: bool,
    pub timezone: String,
    pub premarket_scan: bool,
    /// US/Eastern start of premarket window (HH:MM), e.g. 08:00.
    pub premarket_start_et: String,
    /// Wake interval during premarket (seconds).
    pub premarket_tick_interval_seconds: u64,
    /// Min seconds between premarket LLM digests in the last 30 min before 9:30 open.
    pub premarket_open_grounding_interval_seconds: u64,
    pub overnight: OvernightConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OvernightConfig {
    pub enabled: bool,
    pub tick_interval_seconds: u64,
    pub web_digest: bool,
    pub skip_llm_when_flat: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybookConfig {
    pub style: String,
    pub direction: String,
    pub holding_period: HoldingPeriodConfig,
    pub entry: EntryConfig,
    pub exit: ExitConfig,
    pub closure: ClosureConfig,
    pub intraday: IntradayConfig,
    pub filters: FilterConfig,
    pub short: ShortConfig,
}

/// End-of-day / overnight hold rules (required for intraday).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClosureConfig {
    pub no_overnight_holds: bool,
    /// Flatten all positions by this US/Eastern time (HH:MM).
    pub flatten_by_et: String,
    /// Block new entries after this US/Eastern time (HH:MM).
    pub block_entries_after_et: String,
}

/// Aggressive intraday entry analytics (used when `playbook.style: intraday`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IntradayConfig {
    pub min_relative_volume: f64,
    pub momentum_rsi_min: f64,
    pub require_above_sma: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HoldingPeriodConfig {
    pub min_days: u32,
    pub max_days: u32,
    pub target_days: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EntryConfig {
    pub min_price_usd: f64,
    pub min_avg_volume_20d: f64,
    pub max_spread_pct: f64,
    pub require_above_sma: Vec<u32>,
    /// SMA periods the price must be BELOW to qualify (shallow-pullback / not-extended gate).
    /// Empty (default) disables the check, so this is additive for existing rules files.
    pub require_below_sma: Vec<u32>,
    pub rsi_14_range: [f64; 2],
    pub max_positions: u32,
    pub max_new_entries_per_day: u32,
    pub position_size: PositionSizeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PositionSizeConfig {
    /// risk_pct | atr_normalized
    pub method: String,
    pub risk_per_trade_pct: f64,
    pub max_position_pct: f64,
    /// Reference ATR% for vol scaling when method=atr_normalized.
    pub atr_baseline_pct: f64,
    pub atr_vol_scalar_min: f64,
    pub atr_vol_scalar_max: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfitGivebackExit {
    pub peak_profit_min_pct: f64,
    pub exit_if_below_pct: f64,
    /// Optional fraction-of-peak floor: exit when profit falls below
    /// max(exit_if_below_pct, peak × peak_fraction). Locks in more of a big
    /// run while keeping the fixed floor for small ones. None = fixed floor.
    #[serde(default)]
    pub peak_fraction: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThesisExitConfig {
    pub enabled: bool,
    pub profit_giveback: Option<ProfitGivebackExit>,
    pub exit_if_below_sma: Vec<u32>,
    /// Require peak unrealized profit before SMA-based thesis exit.
    pub exit_if_below_sma_min_peak_profit_pct: f64,
    /// Exit when RS vs benchmark falls below this (same scale as entry filter).
    pub exit_on_rs_deterioration_below: Option<f64>,
    /// Exit open winners when regime profile is in this list.
    pub regime_exit_profiles: Vec<String>,
    pub regime_exit_min_profit_pct: f64,
}

impl Default for ThesisExitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            profit_giveback: None,
            exit_if_below_sma: Vec::new(),
            exit_if_below_sma_min_peak_profit_pct: 5.0,
            exit_on_rs_deterioration_below: None,
            regime_exit_profiles: Vec::new(),
            regime_exit_min_profit_pct: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExitConfig {
    pub profit_target_pct: f64,
    /// Cap profit target at `atr_multiple × ATR%` when enabled (uses min with profit_target_pct).
    pub profit_target_atr_cap: ProfitTargetAtrCapConfig,
    /// Cap profit target at hold-horizon expected move:
    /// `sqrt_days_multiple × ATR% × √target_days` (uses min with other caps).
    #[serde(default)]
    pub profit_target_horizon_cap: ProfitTargetHorizonCapConfig,
    /// Cap profit target using the lookback high/low range: min(room to the
    /// recent N-day high + extension, realized (high−low)/entry width).
    /// Rejects fantasy targets that ATR/horizon still allow when price has not
    /// traded that far recently. Open positions are re-tightened on each tick.
    #[serde(default)]
    pub profit_target_recent_range_cap: ProfitTargetRecentRangeCapConfig,
    pub stop_loss_pct: f64,
    /// Cap stop distance at `atr_multiple × ATR%` when enabled (uses min with
    /// stop_loss_pct). Lets low-vol names (utilities/staples) trade with a
    /// noise-scaled stop instead of a tech-calibrated fixed %, which keeps the
    /// min_reward_risk gate self-consistent across vol regimes.
    #[serde(default)]
    pub stop_loss_atr_cap: StopLossAtrCapConfig,
    pub use_oco_at_entry: bool,
    pub trailing: TrailingConfig,
    pub time_stop_days: u32,
    /// Intraday max hold in minutes (0 = use closure rules only).
    pub time_stop_minutes: u32,
    pub tighten_on_earnings_within_days: u32,
    pub thesis: ThesisExitConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfitTargetAtrCapConfig {
    pub enabled: bool,
    /// Effective target % = min(profit_target_pct, atr_multiple × daily ATR%).
    pub atr_multiple: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfitTargetHorizonCapConfig {
    pub enabled: bool,
    /// Horizon expected move % = sqrt_days_multiple × ATR% × √target_days.
    /// `1.0` ≈ one random-walk expected move over the hold window.
    pub sqrt_days_multiple: f64,
}

/// Cap the profit target using recent swing highs so brackets stay inside
/// historically printed range (plus a small extension buffer).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfitTargetRecentRangeCapConfig {
    pub enabled: bool,
    /// Lookback sessions for recent high/low (mapped to 20 / 60 / 90d history features).
    pub lookback_days: u32,
    /// Allow target this far above the recent high (e.g. 1.0 = 1% breakout room).
    pub max_extension_above_high_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StopLossAtrCapConfig {
    pub enabled: bool,
    /// Effective stop % = min(stop_loss_pct, atr_multiple × daily ATR%).
    /// Keep atr_multiple ≥ filters.min_stop_atr_multiple so the noise floor
    /// and the cap cannot contradict each other.
    pub atr_multiple: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TrailingConfig {
    pub enabled: bool,
    pub activate_after_profit_pct: f64,
    pub trail_atr_multiple: f64,
    /// Once peak profit reaches this %, ratchet the stop to at least
    /// breakeven (entry × (1 + breakeven_buffer_pct/100)). Closes the gap
    /// where a +2–5% winner could round-trip to a full stop-loss because the
    /// ATR trail still sat below entry. None = disabled.
    #[serde(default)]
    pub breakeven_after_profit_pct: Option<f64>,
    /// Buffer above entry for the breakeven floor (covers fees/slippage).
    #[serde(default)]
    pub breakeven_buffer_pct: f64,
}

/// Correlated symbols — cap concurrent open positions per group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SymbolGroupConfig {
    pub name: String,
    pub symbols: Vec<String>,
    /// Max open positions from this group at once (0 = no cap).
    pub max_open: u32,
}

/// Portfolio-aware candidate ranking: cooldown after stops, group diversification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EntryShuffleConfig {
    pub enabled: bool,
    /// Block re-entry after stop_loss within this many calendar days (unless overwhelming).
    pub re_entry_cooldown_days: u32,
    /// Score penalty after a recent stop on the same symbol.
    pub stop_loss_penalty: f64,
    /// Extra penalty per additional stop on the symbol within lookback.
    pub repeat_stop_penalty: f64,
    /// Penalty when another symbol in the same group is already open.
    pub same_group_open_penalty: f64,
    /// Bonus when this group has no open positions but other groups do.
    pub underrepresented_group_bonus: f64,
    pub lookback_days: u32,
    /// Adjusted score must exceed second place by this margin to bypass cooldown.
    pub overwhelming_margin: f64,
    /// Optional RS floor for overwhelming bypass (30d vs benchmark %).
    #[serde(default)]
    pub overwhelming_min_rs_vs_benchmark_30d: Option<f64>,
    pub bypass_cooldown_on_overwhelming: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FilterConfig {
    pub blocked_symbols: Vec<String>,
    pub no_trade_before_earnings_days: u32,
    /// Min 30d return vs benchmark (symbol % − benchmark %). None = disabled.
    #[serde(default)]
    pub min_rs_vs_benchmark_30d: Option<f64>,
    /// Reject when pct_from_52w_high > -N (e.g. 3.0 → must be ≥3% below 52w high).
    #[serde(default)]
    pub min_distance_from_52w_high_pct: Option<f64>,
    /// Between min_distance and this zone below the 52w high, shrink entry size.
    #[serde(default)]
    pub near_52w_high_soft_zone_pct: Option<f64>,
    /// Position size multiplier in the soft zone (e.g. 0.5).
    #[serde(default)]
    pub near_52w_high_size_scalar: Option<f64>,
    /// Require effective_profit_target_pct / stop_loss_pct >= this (catches ATR-capped targets).
    #[serde(default)]
    pub min_reward_risk: Option<f64>,
    /// Require stop_loss_pct / ATR% >= this so the stop clears daily noise.
    #[serde(default)]
    pub min_stop_atr_multiple: Option<f64>,
    #[serde(default)]
    pub symbol_groups: Vec<SymbolGroupConfig>,
    #[serde(default)]
    pub shuffle: EntryShuffleConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortConfig {
    pub enabled: bool,
    pub min_avg_volume_20d: f64,
    pub max_spread_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchlistsConfig {
    pub core: Vec<String>,
    pub thematic: Vec<WatchlistThematic>,
    /// Large seed universe for `watchlist build` (path relative to rules file dir).
    pub candidate_pool_file: Option<String>,
    /// Inline candidate symbols (merged with pool file when set).
    pub candidate_pool: Vec<String>,
    pub screened: WatchlistScreenedConfig,
    pub dynamic: bool,
    pub max_dynamic_symbols: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchlistScreenedConfig {
    pub enabled: bool,
    pub top_n: u32,
    pub min_score: f64,
    /// Minimum days between full pool screens on periodic trigger only (open/premarket still run daily).
    pub refresh_days: u32,
    /// Screen candidate pool once per premarket session.
    #[serde(default = "default_true")]
    pub run_premarket: bool,
    /// Screen at the first regular tick after the open.
    #[serde(default = "default_true")]
    pub run_at_open: bool,
    /// Re-screen during regular hours (playbook-qualified names can rotate intraday).
    #[serde(default = "default_screened_refresh_minutes")]
    pub refresh_every_minutes: u32,
    /// Persist screened symbols into rules YAML `watchlists.thematic` on each refresh.
    #[serde(default)]
    pub write_thematic: bool,
}

fn default_screened_refresh_minutes() -> u32 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchlistThematic {
    pub symbol: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SourcesConfig {
    pub web: WebSourcesConfig,
    /// Financial Modeling Prep — periodic swing candidate discovery (not every tick).
    #[serde(default)]
    pub fmp: FmpSourcesConfig,
    /// User-configured URLs/APIs/RSS feeds prefetched for LLM context.
    pub feeds: Vec<DataFeedSource>,
}

/// FMP discovery during the trading day: premarket, at open, then periodic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FmpSourcesConfig {
    pub enabled: bool,
    /// Minutes between refreshes during regular hours (default 90).
    pub discover_every_minutes: u32,
    /// Run once in premarket (requires `schedule.premarket_scan: true`).
    pub run_premarket: bool,
    /// Run on the first regular tick after the open.
    pub run_at_open: bool,
    /// Max new FMP symbols kept in `dynamic_watchlist` per refresh.
    pub max_add_per_refresh: u32,
    /// Skip paid company-screener; use free movers lists only.
    pub movers_only: bool,
    /// Also write `rules/universe/fmp-discovered.yaml` on each refresh.
    pub write_pool: bool,
    /// Only rotate movers into `dynamic_watchlist` when they pass playbook entry filters.
    #[serde(default = "default_true")]
    pub require_playbook_pass: bool,
    /// Deprecated: ignored. Kept so older YAML still parses.
    #[serde(default)]
    pub discover_every_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebSourcesConfig {
    pub enabled: bool,
    pub pick_budget_per_day: u32,
    pub require_corroboration: u32,
    pub providers: Vec<WebProvider>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebProvider {
    pub id: String,
    pub r#type: String,
    #[serde(default = "default_one_f")]
    pub weight: f64,
}

/// External data source prefetched and injected into LLM prompts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataFeedSource {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// `url` (HTML/text), `api` (JSON), or `rss` (XML feed).
    pub kind: String,
    pub url: String,
    /// LLM phases that receive this feed: selection, monitor, web, learn,
    /// premarket_digest, overnight_digest, or `all`.
    #[serde(default)]
    pub phases: Vec<String>,
    #[serde(default)]
    pub auth: Option<FeedAuth>,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    #[serde(default = "default_feed_max_bytes")]
    pub max_bytes: usize,
    #[serde(default = "default_feed_timeout")]
    pub timeout_seconds: u64,
}

fn default_feed_max_bytes() -> usize {
    12_000
}

fn default_feed_timeout() -> u64 {
    15
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedAuth {
    /// `bearer` (Authorization: Bearer) or `header` (custom header from env).
    pub kind: String,
    /// Environment variable holding the secret (never put tokens in YAML).
    pub token_env: String,
    /// Required when kind=header (e.g. X-API-Key).
    #[serde(default)]
    pub header_name: Option<String>,
}

fn default_one_f() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TechnicalConfig {
    pub history: HistoryConfig,
    pub intraday_history: HistoryConfig,
    pub indicators: Vec<String>,
    #[serde(default)]
    pub market_cache: MarketCacheConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    pub period_type: String,
    pub period: u32,
    pub frequency_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RiskConfig {
    pub max_portfolio_heat_pct: f64,
    pub max_drawdown_halt_pct: f64,
    pub max_trades_per_day: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionConfig {
    pub entry_order_type: String,
    pub entry_limit_basis: String,
    pub bracket_mode: String,
    pub place_bracket_within_seconds: u64,
    pub oco_duration: String,
    pub require_preview: bool,
    pub wait_for_fill: bool,
    pub fill_timeout_seconds: u64,
    /// Block new entries while an unbracketed position exists (default true).
    pub require_bracket_before_entry_resume: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub enabled: bool,
    pub selection_model: String,
    pub monitor_model: String,
    pub web_model: String,
    pub learn_model: String,
    pub review_every_ticks: u64,
    pub web_research_every_reviews: u64,
    pub learn_every_ticks: u64,
    pub learn_min_closed_trades: u32,
    /// Minimum ticks between learn runs after thresholds are met (live + sim).
    pub learn_cooldown_ticks: u64,
    /// Stricter closed-trade threshold for backtest learn (API cost control).
    pub backtest_learn_min_closed_trades: u32,
    /// Minimum ticks between backtest learn runs.
    pub backtest_learn_cooldown_ticks: u64,
    pub max_tokens: u32,
    pub veto_entries: bool,
    pub allow_llm_exits: bool,
    pub allow_rule_adaptation: bool,
    pub adaptation_bounds: serde_json::Value,
    pub immutable_fields: Vec<String>,
    pub prompts: LlmPrompts,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmPrompts {
    #[serde(default)]
    pub selection: String,
    #[serde(default)]
    pub selection_web: String,
    #[serde(default)]
    pub monitor: String,
    #[serde(default)]
    pub learn: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    pub telegram: TelegramNotify,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramNotify {
    pub enabled: bool,
    pub notify_every_tick: bool,
    pub notify_on_actions: bool,
    pub notify_on_rule_adaptation: bool,
}

impl TelegramNotify {
    pub fn to_cli_config(&self) -> schwab_cli::rules::TelegramNotifyConfig {
        schwab_cli::rules::TelegramNotifyConfig {
            enabled: self.enabled,
            notify_every_tick: self.notify_every_tick,
            notify_on_actions: self.notify_on_actions,
            ..Default::default()
        }
    }
}

impl Default for CapitalConfig {
    fn default() -> Self {
        Self {
            fixed_sleeve_cap_usd: 3000.0,
            max_pct_of_free_cash: 80.0,
            min_cash_floor_usd: 500.0,
            options_risk: OptionsRiskConfig::default(),
            core_holdings: vec![],
        }
    }
}

impl Default for OptionsRiskConfig {
    fn default() -> Self {
        Self {
            rules_files: vec![],
            rules_file: String::new(),
            fallback_reserve_usd: 500.0,
            buffer_pct: 10.0,
        }
    }
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            tick_interval_seconds: 90,
            market_hours_only: true,
            timezone: "America/New_York".into(),
            premarket_scan: false,
            premarket_start_et: "08:00".into(),
            premarket_tick_interval_seconds: 1800,
            premarket_open_grounding_interval_seconds: 900,
            overnight: OvernightConfig::default(),
        }
    }
}

impl Default for OvernightConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tick_interval_seconds: 3600,
            web_digest: true,
            skip_llm_when_flat: true,
        }
    }
}

impl Default for PlaybookConfig {
    fn default() -> Self {
        Self {
            style: "swing".into(),
            direction: "long".into(),
            holding_period: HoldingPeriodConfig::default(),
            entry: EntryConfig::default(),
            exit: ExitConfig::default(),
            closure: ClosureConfig::default(),
            intraday: IntradayConfig::default(),
            filters: FilterConfig::default(),
            short: ShortConfig::default(),
        }
    }
}

impl Default for ClosureConfig {
    fn default() -> Self {
        Self {
            no_overnight_holds: false,
            flatten_by_et: "15:55".into(),
            block_entries_after_et: "15:30".into(),
        }
    }
}

impl Default for IntradayConfig {
    fn default() -> Self {
        Self {
            min_relative_volume: 1.2,
            momentum_rsi_min: 52.0,
            require_above_sma: vec![9, 20],
        }
    }
}

impl Default for HoldingPeriodConfig {
    fn default() -> Self {
        Self {
            min_days: 2,
            max_days: 30,
            target_days: 10,
        }
    }
}

impl Default for EntryConfig {
    fn default() -> Self {
        Self {
            min_price_usd: 5.0,
            min_avg_volume_20d: 500_000.0,
            max_spread_pct: 0.5,
            require_above_sma: vec![20, 50],
            require_below_sma: vec![],
            rsi_14_range: [45.0, 65.0],
            max_positions: 4,
            max_new_entries_per_day: 1,
            position_size: PositionSizeConfig::default(),
        }
    }
}

impl Default for PositionSizeConfig {
    fn default() -> Self {
        Self {
            method: "risk_pct".into(),
            risk_per_trade_pct: 0.75,
            max_position_pct: 8.0,
            atr_baseline_pct: 2.0,
            atr_vol_scalar_min: 0.5,
            atr_vol_scalar_max: 1.5,
        }
    }
}

impl Default for ProfitTargetAtrCapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            atr_multiple: 2.5,
        }
    }
}

impl Default for ProfitTargetHorizonCapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sqrt_days_multiple: 1.0,
        }
    }
}

impl Default for ProfitTargetRecentRangeCapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            lookback_days: 60,
            max_extension_above_high_pct: 1.0,
        }
    }
}

impl Default for StopLossAtrCapConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            atr_multiple: 2.0,
        }
    }
}

impl Default for ExitConfig {
    fn default() -> Self {
        Self {
            profit_target_pct: 8.0,
            profit_target_atr_cap: ProfitTargetAtrCapConfig::default(),
            profit_target_horizon_cap: ProfitTargetHorizonCapConfig::default(),
            profit_target_recent_range_cap: ProfitTargetRecentRangeCapConfig::default(),
            stop_loss_pct: 4.0,
            stop_loss_atr_cap: StopLossAtrCapConfig::default(),
            use_oco_at_entry: true,
            trailing: TrailingConfig::default(),
            time_stop_days: 30,
            time_stop_minutes: 0,
            tighten_on_earnings_within_days: 3,
            thesis: ThesisExitConfig::default(),
        }
    }
}

impl Default for TrailingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            activate_after_profit_pct: 5.0,
            trail_atr_multiple: 2.0,
            breakeven_after_profit_pct: None,
            breakeven_buffer_pct: 0.1,
        }
    }
}

impl Default for SymbolGroupConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            symbols: vec![],
            max_open: 1,
        }
    }
}

impl Default for EntryShuffleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            re_entry_cooldown_days: 2,
            stop_loss_penalty: 0.25,
            repeat_stop_penalty: 0.1,
            same_group_open_penalty: 0.2,
            underrepresented_group_bonus: 0.08,
            lookback_days: 7,
            overwhelming_margin: 0.12,
            overwhelming_min_rs_vs_benchmark_30d: Some(5.0),
            bypass_cooldown_on_overwhelming: true,
        }
    }
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            blocked_symbols: vec![],
            no_trade_before_earnings_days: 2,
            min_rs_vs_benchmark_30d: None,
            min_distance_from_52w_high_pct: None,
            near_52w_high_soft_zone_pct: None,
            near_52w_high_size_scalar: None,
            min_reward_risk: None,
            min_stop_atr_multiple: None,
            symbol_groups: vec![],
            shuffle: EntryShuffleConfig::default(),
        }
    }
}

impl Default for ShortConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_avg_volume_20d: 2_000_000.0,
            max_spread_pct: 0.4,
        }
    }
}

impl Default for WatchlistScreenedConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            top_n: 12,
            min_score: 0.0,
            refresh_days: 7,
            run_premarket: true,
            run_at_open: true,
            refresh_every_minutes: default_screened_refresh_minutes(),
            write_thematic: false,
        }
    }
}

impl Default for WatchlistsConfig {
    fn default() -> Self {
        Self {
            core: vec![],
            thematic: vec![],
            candidate_pool_file: None,
            candidate_pool: vec![],
            screened: WatchlistScreenedConfig::default(),
            dynamic: false,
            max_dynamic_symbols: 5,
        }
    }
}

impl Default for SourcesConfig {
    fn default() -> Self {
        Self {
            web: WebSourcesConfig::default(),
            fmp: FmpSourcesConfig::default(),
            feeds: vec![],
        }
    }
}

impl Default for FmpSourcesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            discover_every_minutes: 90,
            run_premarket: true,
            run_at_open: true,
            max_add_per_refresh: 5,
            movers_only: false,
            write_pool: true,
            require_playbook_pass: true,
            discover_every_days: None,
        }
    }
}

impl Default for WebSourcesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            pick_budget_per_day: 5,
            require_corroboration: 1,
            providers: vec![],
        }
    }
}

impl Default for TechnicalConfig {
    fn default() -> Self {
        Self {
            history: HistoryConfig::default(),
            intraday_history: HistoryConfig::intraday_default(),
            indicators: vec![
                "sma_20".into(),
                "sma_50".into(),
                "rsi_14".into(),
                "atr_14".into(),
            ],
            market_cache: MarketCacheConfig::default(),
        }
    }
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            period_type: "month".into(),
            period: 3,
            frequency_type: "daily".into(),
        }
    }
}

impl HistoryConfig {
    pub fn intraday_default() -> Self {
        Self {
            period_type: "day".into(),
            period: 5,
            frequency_type: "minute".into(),
        }
    }
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_portfolio_heat_pct: 8.0,
            max_drawdown_halt_pct: 10.0,
            max_trades_per_day: 1,
        }
    }
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            entry_order_type: "limit".into(),
            entry_limit_basis: "ask".into(),
            bracket_mode: "post_fill_oco".into(),
            place_bracket_within_seconds: 30,
            oco_duration: "GTC".into(),
            require_preview: true,
            wait_for_fill: true,
            fill_timeout_seconds: 600,
            require_bracket_before_entry_resume: true,
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
            learn_model: "anthropic/claude-sonnet-4".into(),
            review_every_ticks: 3,
            web_research_every_reviews: 2,
            learn_every_ticks: 6,
            learn_min_closed_trades: 3,
            learn_cooldown_ticks: 20,
            backtest_learn_min_closed_trades: 5,
            backtest_learn_cooldown_ticks: 30,
            max_tokens: 2000,
            veto_entries: true,
            allow_llm_exits: false,
            allow_rule_adaptation: true,
            adaptation_bounds: serde_json::json!({}),
            immutable_fields: vec![],
            prompts: LlmPrompts::default(),
        }
    }
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            telegram: TelegramNotify::default(),
        }
    }
}

impl Default for TelegramNotify {
    fn default() -> Self {
        Self {
            enabled: false,
            notify_every_tick: false,
            notify_on_actions: true,
            notify_on_rule_adaptation: true,
        }
    }
}

impl Default for RegimeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            benchmark_symbol: "SPY".into(),
            vix_symbol: "$VIX".into(),
            vix_low: 15.0,
            vix_high: 25.0,
            realized_vol_lookback: 20,
            realized_vol_history: 60,
            realized_vol_high_percentile: 70.0,
            profile_switch_min_dwell_ticks: 20,
        }
    }
}

impl Default for MonitorAdjustmentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_tighten_pct: 2.0,
            max_widen_pct: 1.5,
            min_stop_distance_from_price_pct: 1.5,
            max_stop_distance_from_price_pct: 8.0,
        }
    }
}

impl Default for AdaptationConfig {
    fn default() -> Self {
        Self::default_swing()
    }
}

impl AdaptationConfig {
    pub fn default_swing() -> Self {
        let mut profile_map = HashMap::new();
        profile_map.insert("low_vol_trend".into(), "low_vol_trend".into());
        profile_map.insert("high_vol_chop".into(), "high_vol_chop".into());
        profile_map.insert("elevated_vol".into(), "elevated_vol".into());
        profile_map.insert("neutral".into(), "baseline".into());

        let mut profiles = HashMap::new();
        profiles.insert(
            "baseline".into(),
            TradingProfile {
                description: "Default playbook from rules YAML".into(),
                overrides: PlaybookProfileOverrides::default(),
            },
        );
        profiles.insert(
            "low_vol_trend".into(),
            TradingProfile {
                description: "Calm uptrend — let winners run, normal participation".into(),
                overrides: PlaybookProfileOverrides {
                    exit: Some(crate::adaptation::ProfileExitOverrides {
                        profit_target_pct: Some(10.0),
                        stop_loss_pct: Some(3.5),
                        trailing: Some(crate::adaptation::ProfileTrailingOverrides {
                            activate_after_profit_pct: Some(4.0),
                            trail_atr_multiple: Some(1.75),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    entry: Some(crate::adaptation::ProfileEntryOverrides {
                        max_new_entries_per_day: Some(1),
                        position_size: Some(crate::adaptation::ProfilePositionSizeOverrides {
                            risk_per_trade_pct: Some(0.9),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            },
        );
        profiles.insert(
            "high_vol_chop".into(),
            TradingProfile {
                description: "High vol chop — defensive sizing, wider stops, no new entries".into(),
                overrides: PlaybookProfileOverrides {
                    exit: Some(crate::adaptation::ProfileExitOverrides {
                        profit_target_pct: Some(6.0),
                        stop_loss_pct: Some(5.0),
                        trailing: Some(crate::adaptation::ProfileTrailingOverrides {
                            trail_atr_multiple: Some(2.5),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    entry: Some(crate::adaptation::ProfileEntryOverrides {
                        max_new_entries_per_day: Some(0),
                        position_size: Some(crate::adaptation::ProfilePositionSizeOverrides {
                            risk_per_trade_pct: Some(0.4),
                            method: Some("atr_normalized".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            },
        );
        profiles.insert(
            "elevated_vol".into(),
            TradingProfile {
                description: "Elevated vol — reduced size, moderate targets".into(),
                overrides: PlaybookProfileOverrides {
                    exit: Some(crate::adaptation::ProfileExitOverrides {
                        profit_target_pct: Some(7.0),
                        stop_loss_pct: Some(4.5),
                        trailing: Some(crate::adaptation::ProfileTrailingOverrides {
                            trail_atr_multiple: Some(2.25),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    entry: Some(crate::adaptation::ProfileEntryOverrides {
                        max_new_entries_per_day: Some(1),
                        position_size: Some(crate::adaptation::ProfilePositionSizeOverrides {
                            risk_per_trade_pct: Some(0.55),
                            method: Some("atr_normalized".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            },
        );

        Self {
            enabled: true,
            live_auto_apply: false,
            regime_auto_select: true,
            llm_profile_select: true,
            regime: RegimeConfig::default(),
            monitor_adjustments: MonitorAdjustmentsConfig::default(),
            profiles,
            profile_map,
            default_profile: "baseline".into(),
        }
    }
}

impl DataFeedSource {
    pub fn applies_to_phase(&self, phase: &str) -> bool {
        if !self.enabled {
            return false;
        }
        if self.phases.is_empty() {
            return matches!(
                phase,
                "selection"
                    | "web"
                    | "premarket_digest"
                    | "overnight_digest"
                    | "monitor"
            );
        }
        self.phases.iter().any(|p| {
            let p = p.trim().to_ascii_lowercase();
            p == "all" || p == phase
        })
    }
}

impl TraderRules {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("Failed to read rules file {}", path.display()))?;
        let mut rules: TraderRules = serde_yaml::from_str(&raw)
            .with_context(|| format!("Failed to parse rules YAML {}", path.display()))?;
        rules.normalize_adaptation();
        rules.validate()?;
        Ok(rules)
    }

    pub fn fractional_shares_allowed(&self) -> bool {
        self.simulation
            .as_ref()
            .map(|s| s.allow_fractional_shares)
            .unwrap_or(false)
    }

    pub fn backtest_learn_enabled(&self) -> bool {
        self.llm.enabled
            && self.llm.allow_rule_adaptation
            && self
                .simulation
                .as_ref()
                .map(|s| s.allow_rule_adaptation)
                .unwrap_or(true)
    }

    /// Fill built-in regime profiles when YAML omits them (backward compatible).
    pub fn normalize_adaptation(&mut self) {
        if self.adaptation.profiles.is_empty() {
            let defaults = AdaptationConfig::default_swing();
            self.adaptation.profiles = defaults.profiles;
            self.adaptation.profile_map = defaults.profile_map;
        }
        if self.adaptation.default_profile.is_empty() {
            self.adaptation.default_profile = "baseline".into();
        }
    }

    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.version == RULES_VERSION,
            "Unsupported rules version {} (expected {RULES_VERSION})",
            self.version
        );
        anyhow::ensure!(!self.trader_id.is_empty(), "trader_id is required");
        anyhow::ensure!(
            self.accounts.iter().any(|a| a.enabled),
            "At least one enabled account is required"
        );
        anyhow::ensure!(
            self.capital.fixed_sleeve_cap_usd > 0.0,
            "capital.fixed_sleeve_cap_usd must be positive"
        );
        anyhow::ensure!(
            self.execution.bracket_mode == "post_fill_oco",
            "Only bracket_mode post_fill_oco is supported in v1"
        );
        if (self.playbook.direction == "short" || self.playbook.direction == "both")
            && !self.playbook.short.enabled
        {
            anyhow::bail!(
                "playbook.direction is {:?} but playbook.short.enabled is false",
                self.playbook.direction
            );
        }
        if self.is_intraday() && !self.playbook.closure.no_overnight_holds {
            anyhow::bail!("intraday playbook requires closure.no_overnight_holds=true");
        }
        anyhow::ensure!(
            self.schedule.timezone == "America/New_York",
            "schedule.timezone must be America/New_York for US equity market hours (EST/EDT); got `{}`",
            self.schedule.timezone
        );
        self.validate_feeds()?;
        Ok(())
    }

    fn validate_feeds(&self) -> Result<()> {
        let mut ids = std::collections::HashSet::new();
        for feed in &self.sources.feeds {
            anyhow::ensure!(!feed.id.trim().is_empty(), "sources.feeds[].id is required");
            anyhow::ensure!(
                ids.insert(feed.id.clone()),
                "duplicate sources.feeds id `{}`",
                feed.id
            );
            anyhow::ensure!(
                !feed.url.trim().is_empty(),
                "sources.feeds[{}].url is required",
                feed.id
            );
            let url = feed.url.trim();
            anyhow::ensure!(
                url.starts_with("https://") || url.starts_with("http://127.0.0.1"),
                "sources.feeds[{}].url must be https:// (or http://127.0.0.1 for local dev)",
                feed.id
            );
            let kind = feed.kind.trim().to_ascii_lowercase();
            anyhow::ensure!(
                matches!(kind.as_str(), "url" | "api" | "rss"),
                "sources.feeds[{}].kind must be url, api, or rss",
                feed.id
            );
            if let Some(auth) = &feed.auth {
                anyhow::ensure!(
                    !auth.token_env.trim().is_empty(),
                    "sources.feeds[{}].auth.token_env is required",
                    feed.id
                );
                let ak = auth.kind.trim().to_ascii_lowercase();
                anyhow::ensure!(
                    matches!(ak.as_str(), "bearer" | "header"),
                    "sources.feeds[{}].auth.kind must be bearer or header",
                    feed.id
                );
                if ak == "header" {
                    anyhow::ensure!(
                        auth.header_name.as_ref().is_some_and(|h| !h.trim().is_empty()),
                        "sources.feeds[{}].auth.header_name required for header auth",
                        feed.id
                    );
                }
            }
        }
        Ok(())
    }

    pub fn feeds_for_phase(&self, phase: &str) -> Vec<&DataFeedSource> {
        self.sources
            .feeds
            .iter()
            .filter(|f| f.applies_to_phase(phase))
            .collect()
    }

    /// Non-fatal configuration hints surfaced by `rules validate` and agent startup.
    pub fn validation_hints(&self) -> Vec<String> {
        let mut hints = Vec::new();
        if !self.playbook.closure.no_overnight_holds
            && !self.playbook.closure.flatten_by_et.trim().is_empty()
        {
            hints.push(format!(
                "flatten_by_et is set to `{}` but no_overnight_holds is false — EOD flatten will not trigger. \
                 Set no_overnight_holds: true to enable EOD flattening.",
                self.playbook.closure.flatten_by_et.trim()
            ));
        }
        hints
    }

    pub fn log_validation_hints(&self) {
        for hint in self.validation_hints() {
            tracing::warn!(target: "trader", "{hint}");
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = serde_yaml::to_string(self)
            .with_context(|| format!("serialize rules {}", path.display()))?;
        fs::write(path, raw).with_context(|| format!("write rules {}", path.display()))?;
        Ok(())
    }

    pub fn is_intraday(&self) -> bool {
        self.playbook.style.eq_ignore_ascii_case("intraday")
    }

    pub fn effective_history(&self) -> HistoryConfig {
        if self.is_intraday() {
            self.technical.intraday_history.clone()
        } else {
            self.technical.history.clone()
        }
    }

    pub fn primary_account(&self) -> Result<&TraderAccount> {
        self.accounts
            .iter()
            .find(|a| a.enabled)
            .context("No enabled account in rules")
    }

    pub fn all_watchlist_symbols(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .watchlists
            .core
            .iter()
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
        for t in &self.watchlists.thematic {
            let sym = t.symbol.trim().to_uppercase();
            if !sym.is_empty() && !out.contains(&sym) {
                out.push(sym);
            }
        }
        out
    }

    /// Rules YAML plus documented includes (`watchlists.candidate_pool_file`).
    pub fn watch_paths(&self, rules_path: &Path) -> Vec<PathBuf> {
        let mut paths = vec![rules_path.to_path_buf()];
        if let Some(rel) = &self.watchlists.candidate_pool_file {
            let rel = rel.trim();
            if !rel.is_empty() {
                let pool = rules_path
                    .parent()
                    .map(|p| p.join(rel))
                    .unwrap_or_else(|| PathBuf::from(rel));
                paths.push(pool);
            }
        }
        paths
    }

    /// Symbols from candidate pool file + inline list (for screening / prefetch).
    pub fn candidate_pool_symbols(&self, rules_path: &Path) -> Result<Vec<String>> {
        let mut out = Vec::new();
        if let Some(rel) = &self.watchlists.candidate_pool_file {
            let rel = rel.trim();
            if !rel.is_empty() {
                let pool_path = rules_path
                    .parent()
                    .map(|p| p.join(rel))
                    .unwrap_or_else(|| PathBuf::from(rel));
                let pool = crate::watchlist::pool::load_pool_file(&pool_path)?;
                for sym in pool.symbols {
                    let u = sym.trim().to_uppercase();
                    if !u.is_empty() && !out.contains(&u) {
                        out.push(u);
                    }
                }
            }
        }
        for sym in &self.watchlists.candidate_pool {
            let u = sym.trim().to_uppercase();
            if !u.is_empty() && !out.contains(&u) {
                out.push(u);
            }
        }
        Ok(out)
    }

    /// Pool symbols eligible for mechanical screening (excludes core, blocked, core holdings).
    pub fn symbols_for_screening(&self, rules_path: &Path) -> Result<Vec<String>> {
        let core: std::collections::HashSet<String> = self
            .all_watchlist_symbols()
            .into_iter()
            .collect();
        let mut out = Vec::new();
        for sym in self.candidate_pool_symbols(rules_path)? {
            if core.contains(&sym) {
                continue;
            }
            if self.is_core_holding(&sym) || self.is_blocked_symbol(&sym) {
                continue;
            }
            if !out.contains(&sym) {
                out.push(sym);
            }
        }
        Ok(out)
    }

    pub fn is_core_holding(&self, symbol: &str) -> bool {
        let sym = symbol.trim().to_uppercase();
        self.capital
            .core_holdings
            .iter()
            .any(|s| s.eq_ignore_ascii_case(&sym))
    }

    pub fn is_blocked_symbol(&self, symbol: &str) -> bool {
        let sym = symbol.trim().to_uppercase();
        self.playbook
            .filters
            .blocked_symbols
            .iter()
            .any(|s| s.eq_ignore_ascii_case(&sym))
    }

    /// Named symbol group containing `symbol`, if configured.
    pub fn symbol_group_name(&self, symbol: &str) -> Option<&str> {
        let sym = symbol.trim().to_uppercase();
        for group in &self.playbook.filters.symbol_groups {
            if group
                .symbols
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&sym))
            {
                return Some(group.name.as_str());
            }
        }
        None
    }

    /// `max_open` for a group name (0 = unlimited).
    pub fn symbol_group_max_open(&self, group_name: &str) -> u32 {
        self.playbook
            .filters
            .symbol_groups
            .iter()
            .find(|g| g.name.eq_ignore_ascii_case(group_name))
            .map(|g| g.max_open)
            .unwrap_or(0)
    }
}

impl Default for TraderRules {
    fn default() -> Self {
        Self {
            version: RULES_VERSION,
            trader_id: String::new(),
            accounts: vec![],
            capital: CapitalConfig::default(),
            schedule: ScheduleConfig::default(),
            playbook: PlaybookConfig::default(),
            watchlists: WatchlistsConfig::default(),
            sources: SourcesConfig::default(),
            technical: TechnicalConfig::default(),
            risk: RiskConfig::default(),
            execution: ExecutionConfig::default(),
            llm: LlmConfig::default(),
            notify: NotifyConfig::default(),
            simulation: None,
            adaptation: AdaptationConfig::default(),
        }
    }
}

pub fn validate_rules_file(path: &Path) -> Result<serde_json::Value> {
    let rules = TraderRules::load(path)?;
    let hints = rules.validation_hints();
    Ok(serde_json::json!({
        "valid": true,
        "trader_id": rules.trader_id,
        "account_count": rules.accounts.len(),
        "watchlist_size": rules.all_watchlist_symbols().len(),
        "fixed_sleeve_cap_usd": rules.capital.fixed_sleeve_cap_usd,
        "hints": hints,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_hint_when_overnight_holds_allowed() {
        let mut rules = TraderRules::default();
        rules.playbook.closure.no_overnight_holds = false;
        rules.playbook.closure.flatten_by_et = "15:55".into();
        let hints = rules.validation_hints();
        assert_eq!(hints.len(), 1);
        assert!(hints[0].contains("no_overnight_holds is false"));
    }

    #[test]
    fn example_rules_parse() {
        let swing = Path::new("../../rules/trader-rules.example.yaml");
        if swing.is_file() {
            TraderRules::load(swing).expect("trader example rules should parse");
        }
        let intraday = Path::new("../../rules/my-intraday-trader.yaml");
        if intraday.is_file() {
            let r = TraderRules::load(intraday).expect("intraday rules should parse");
            assert!(r.is_intraday());
        }
    }

    #[test]
    fn swing_9947_enables_horizon_cap_and_scales_target() {
        let path = Path::new("../../rules/trader-swing-9947.yaml");
        if !path.is_file() {
            return;
        }
        let rules = TraderRules::load(path).expect("swing-9947 rules should parse");
        assert!(rules.playbook.exit.profit_target_horizon_cap.enabled);
        assert!((rules.playbook.exit.profit_target_horizon_cap.sqrt_days_multiple - 1.0).abs() < 1e-9);
        assert_eq!(rules.playbook.holding_period.target_days, 10);
        // ATR 1.5% → atr cap 3.75% binds before horizon ≈ 4.74%
        let pct = crate::capital::effective_profit_target_pct(
            100.0,
            &rules,
            Some(1.5),
            crate::capital::ExitRangeContext::none(),
        );
        assert!((pct - 3.75).abs() < 0.01);
    }

    #[test]
    fn watch_paths_include_candidate_pool_file() {
        let mut rules = TraderRules::default();
        rules.watchlists.candidate_pool_file = Some("universe/sp100-liquid.yaml".into());
        let rules_path = Path::new("/tmp/rules/trader.yaml");
        let paths = rules.watch_paths(rules_path);
        assert_eq!(paths[0], rules_path);
        assert!(paths[1].ends_with("universe/sp100-liquid.yaml"));
    }
}
