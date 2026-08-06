# Options agent rules (`rules.yaml`)

The options agent is a long-running process that reads `rules.yaml`, evaluates entry/exit
conditions on a schedule, and auto-executes **vertical spreads** and **iron condors** within
`safety.json` hard limits. An optional **OpenRouter LLM advisor** reviews positions periodically
and can veto new entries; **Telegram** can push trade and alert notifications.

**LLM authoring guide:** [LLM_SCHEMA_REFERENCE.md](LLM_SCHEMA_REFERENCE.md) (full schema, field reference, monitor context).

**Schedule:** [AGENT_SCHEDULE.md](AGENT_SCHEDULE.md) (regular / overnight / at-open).

## Two-layer safety model

| Layer | File | Purpose |
|-------|------|---------|
| Hard ceiling | `safety.json` | CLI rejects any order exceeding limits |
| Strategy brain | `rules.yaml` | What to trade, when, how much |
| LLM advisor | `rules.yaml` → `llm` | Periodic expert review (optional) |

## Quick start

```bash
# 1. Enable options in safety.json (see safety.json.example)
schwab safety show --json

# 2. Set API keys in .env (for LLM / Telegram when enabled)
# OPENROUTER_API_KEY=...
# TELEGRAM_BOT_TOKEN=...
# TELEGRAM_CHAT_ID=...

# 3. Validate rules
schwab agent validate rules/options-rules.example.yaml --json

# 4. Dry-run one agent tick (no orders)
schwab agent run rules/options-rules.example.yaml --dry-run --once --json

# 5. Foreground daemon (requires --trust --yes for live trades)
schwab agent run rules/options-rules.example.yaml --trust --yes --json

# 6. Background daemon (pid + log next to rules file)
schwab agent run rules/options-rules.example.yaml --background --trust --yes --json
schwab agent stop rules/options-rules.example.yaml --json
```

## Exit rules (automatic)

| Rule | Default | Behavior |
|------|---------|----------|
| `profit_target_pct` | 50 | Close when captured ≥50% of entry credit |
| `stop_loss_pct` | 200 | Close when debit to close ≥ 2× entry credit |
| `stop_loss_require_short_otm_below_pct` | unset | If set, arm the mark stop only when short OTM% is below this (gives time while far from strike) |
| `dte_close` | 21 | Close when DTE ≤ 21 regardless |
| `roll.enabled` | `false` | Prefer a managed vertical roll over a hard stop when eligible (see Defensive rolling) |

Exits run **before** entry scans each regular tick. Marks come from live option chain **`debit_to_close`** (not Schwab `net_market_value`).

Monitor LLM context includes `mechanical_rules.stop_triggered` — only treat a stop as hit when that field is `true`. See [LLM_SCHEMA_REFERENCE.md](LLM_SCHEMA_REFERENCE.md#field-reference--exit_rules-mechanical--authoritative).

### Defensive rolling

When a **credit vertical** hits the mechanical `stop_loss`, the agent can attempt a **managed roll** (close the tested spread, then open a farther-OTM / later-DTE replacement) instead of eating a full stop. Iron condors, thesis exits, and DTE closes are out of scope for v1. Rolls are mechanical (same class as stops — they bypass `require_llm_proceed`) and use **two sequential orders** (close then open), not Schwab `VERTICAL_ROLL`.

| Field | Default | Meaning |
|-------|---------|---------|
| `enabled` | `false` | Must opt in; soak with `--simulate` before live |
| `min_dte_remaining` | 21 | Skip roll if mark DTE is below this |
| `min_short_otm_pct` | 0.5 | Do not roll already-ITM / near-ITM shorts |
| `min_dte_extension` | 7 | Prefer expiry ≥ closed DTE + this many days |
| `target_short_delta_max` | 0.12 | Cap \|short_delta\| (also ≤ original short delta − 0.04) |
| `max_debit_pct_of_entry_credit` | 25 | Allow limited debit: `new_credit − close_debit ≥ −entry × pct/100` |
| `max_rolls_per_position` | 1 | Per lineage (`TrackedPosition.rolls_used`) |
| `max_rolls_per_day` | 1 | Soft account-wide daily cap |

A **successful** roll does **not** call `record_stop_loss_exit` (no stop re-entry cooldown). If the roll is ineligible or the replacement fails after close, the normal stop path / cooldown applies. Tick `skipped` strings distinguish `rolled` vs `stop_loss`. Journal / Telegram event type: `defensive_roll`.

```yaml
exit_rules:
  roll:
    enabled: false
    min_dte_remaining: 21
    min_short_otm_pct: 0.5
    min_dte_extension: 7
    target_short_delta_max: 0.12
    max_debit_pct_of_entry_credit: 25
    max_rolls_per_position: 1
    max_rolls_per_day: 1
```

## Historical backtest (synthetic)

Schwab does not provide historical option chains. `schwab agent backtest` replays your rules over **daily underlying bars** and prices verticals/condors with **Black–Scholes**, using **VIX as the IV proxy**. Use it to research gates (delta/DTE/IV-RV/VIX pause/blackouts), not as OPRA-realistic expectancy.

```bash
schwab agent backtest prefetch --rules-file rules/options-pilot-8709.yaml --from 2025-01-01
schwab agent backtest run --rules-file rules/options-pilot-8709.yaml --from 2025-01-01 --fresh --json
schwab agent backtest report --rules-file rules/options-pilot-8709.yaml --json
```

LLM selection is off in backtest (mechanical gates only). Fill model: daily close. Artifacts next to the rules file: `.options-backtest-cache-*.json`, `agent-backtest-state-*.json`, `agent-backtest-journal-*.jsonl`.

## Broker-side protection (what survives if the agent is down)

All three exit rules above are evaluated by the agent's own tick loop — if the process is
down (crashed, killed, host rebooted, network lost), none of them fire until it resumes.
One piece of protection is broker-resident and survives regardless: after each live entry
fill, the agent places a resting **GTC limit order to close the spread at the profit-target
debit** (`execution.protective_order`, enabled by default). This lives on Schwab's servers,
not in the agent's memory.

Stop-loss and DTE-close **cannot** be made broker-resident the same way — Schwab's order API
rejects a stop trigger (`orderType: STOP`/`STOP_LIMIT`) on a multi-leg complex option order
(`complexOrderStrategyType: VERTICAL`/`IRON_CONDOR`), confirmed via a live `orders preview`
test (`HTTP 400: "Stop price must be populated only for stop orders"` — the identical shape
works fine on a single-leg option). Splitting a spread into two independent single-leg stop
orders is **not** done here: a stop firing on only one leg would turn a defined-risk spread
into a naked position, which is worse than the status quo. Because of this, running the
agent under process supervision with auto-restart (systemd/launchd/Docker restart policy)
plus a paging alert on any tick gap is a **hard operational requirement**, not optional —
stop-loss and DTE-close protection depend on the process being up.

```yaml
execution:
  protective_order:
    enabled: true        # place a broker-resident GTC profit-target close order per entry
    max_attempts: 3       # placement retries right after fill
    max_seconds: 30        # deadline for those retries
```

## Paper simulation slippage

`simulation.fill_slippage_pct` (default 0) applies a spread-cost haircut to virtual fills:
entry credit is reduced by X% and exit debit increased by X% (e.g. 5.0 = 5%). This makes
paper P/L honest about bid/ask — the live sim should not look systematically better than
reality. The synthetic backtest is a separate model (daily-close BS fills) and does not
apply this; treat backtest as gate research, sim as paper P/L.

If placement fails after `max_attempts`, the position stays open without broker-side
protection and is retried every subsequent tick (`reconcile_protective_orders`) until it
succeeds. Tick JSON reports `monitoring.unprotected_count` — treat a nonzero value the same
way `docs/TRADER_ROLLOUT.md` treats `monitoring.unbracketed_count` for the equity bot: it
means live risk is currently uncovered on Schwab's side and should be investigated, not
ignored. Positions opened before this feature existed (no stored `entry_params`) cannot be
retroactively protected and count toward `unprotected_count` without being retried.

The resting order is canceled and replaced whenever the mechanical tick loop closes the
position early (stop/DTE/thesis exit) or tops up an existing position, to avoid a race
between the resting order and the tick-driven close.

## Risk gates vs daily trade count

Hard gates for new entries:

- `entry_rules.*.max_open_positions`
- `risk.max_portfolio_risk_usd` / `risk.max_risk_per_trade_usd`

`risk.max_trades_per_day` is only a **soft churn cap** (redeploy after thesis exits, etc.). Set it to `0` for unlimited daily opens (still subject to open slots + $ risk).

## Macro / event blackouts

Two different mechanisms — do not confuse them:

| Field | Behavior |
|-------|----------|
| `risk.blocked_events` | **Manual kill switch.** Any non-empty list pauses **all** new entries until cleared. Not date-aware. |
| `risk.blocked_dates` | **Calendar.** Each entry has `date` (`YYYY-MM-DD`), `lead_days`, and `label`. New entries pause when `date - lead_days ≤ today ≤ date`. Exits and protective-order reconcile still run. |
| `risk.post_event_entry_window_days` | **Post-event IV-harvest window.** For this many calendar days after the most recent `blocked_dates` event, the tick flags `post_event_window` (tick JSON + LLM context). IV is typically richest right after FOMC/CPI/NFP settles; entries already resume mechanically after the event — this only surfaces the opportunity, no mechanical relaxation. |

```yaml
risk:
  blocked_events: []          # leave empty unless you want a hard manual pause
  blocked_dates:
    - date: "2026-09-16"      # FOMC decision day
      lead_days: 1            # also block the prior calendar day
      label: FOMC
    - date: "2026-09-11"
      lead_days: 0
      label: CPI
```

Populate from the public Fed / BLS calendars (no external API). Tick JSON surfaces
`monitoring.blocked_dates_active` with the active labels.

## Correlation groups

Cap concurrent opens across highly correlated underlyings (e.g. SPY/QQQ):

```yaml
risk:
  correlation_groups:
    - name: broad_market
      symbols: [QQQ, SPY]
      max_open: 1
```

Checked next to `max_open_per_underlying` during entry scan. `max_open: 0` disables the group.

## Portfolio drawdown halt

Mirrors the equity trader's sleeve HWM halt. Tracks:

`sleeve_equity ≈ drawdown_sleeve_usd (or max_portfolio_risk_usd) + realized_pnl + unrealized_mark_pnl`

When drawdown from peak ≥ `max_drawdown_halt_pct`, **new entries pause**; exits and protective orders continue. Clears automatically when drawdown recovers. Telegram notifies on halt / resume.

```yaml
risk:
  max_drawdown_halt_pct: 15.0
  drawdown_sleeve_usd: 4000   # options cash sleeve; omit to use max_portfolio_risk_usd
```

Tick JSON: `monitoring.drawdown`, `monitoring.trading_halted_reason`.

## Regime-aware structure (`regime`)

When `regime.enabled: true`, the agent classifies SPY trend + VIX and scans **one** preferred structure:

| Regime | Typical map |
|--------|-------------|
| `low_vol_trend` / `elevated_vol` / `neutral` | `put_credit` |
| `bearish_trend` (below SMA50 and SMA200) | `call_credit` |
| `high_vol_chop` (incl. below SMA50 but above SMA200, any VIX) | `iron_condor` |
| `hostile` or VIX ≥ `pause_entries_vix_above` | pause new entries |
| VIX ≤ `pause_entries_vix_below` (when set) | pause new entries |
| VIX quote missing, `pause_on_missing_vix: true` (default) | pause new entries |

Below the 50DMA never classifies as `neutral` — soft tape maps to
`high_vol_chop`/`bearish_trend` regardless of how calm VIX looks, so the agent
never sells put credits into a downtrend because "POP looks high."

`vix_low` / `vix_high` only **classify** regimes. Entry pauses use the explicit
`pause_entries_vix_above` / `pause_entries_vix_below` knobs (floor is optional).
Set `pause_on_missing_vix: false` to restore the legacy fail-open behavior when
the VIX quote is unavailable.

Requires `strategies.iron_condor.enabled: true` for condor regimes. Vertical call credits use the same delta/width rules as puts.

### Put-credit guard (`regime.put_credit_guard`)

When VIX is at/above `vix_above` AND the benchmark is below its short-term SMA
(`require_benchmark_above_sma`, e.g. 20), put-credit entries are blocked. This is the
backtest-proven killer zone: every historical stop-loss lived at VIX 18–22 — selling puts
into an elevated-vol tape that is already below its trend line.

```yaml
regime:
  put_credit_guard:
    vix_above: 18.0
    require_benchmark_above_sma: 20
```

Applied in both the live agent and the synthetic backtest (benchmark-level, so it applies
to every symbol on the watchlist). Fail-closed: a missing VIX quote pauses entries via
`pause_on_missing_vix` anyway.

### Regime-mismatch early take

When `exit_rules.thesis.regime_mismatch.enabled: true`, each tick compares the
**open structure** to the live preferred strategy ("what would I open flat now?").
If they differ and unrealized profit ≥ `min_profit_pct` (default 25% of credit),
the agent exits with reason `thesis_regime_mismatch` and sets a redeploy signal.

This runs **even during** `thesis.min_hold_minutes` (unlike POP/delta/OTM thesis
exits). With `treat_pause_as_mismatch: true`, green positions are also taken when
preferred is `pause` (hostile / crushed vol).

```yaml
exit_rules:
  thesis:
    regime_mismatch:
      enabled: true
      min_profit_pct: 25
      treat_pause_as_mismatch: true
```

Pair with `entry_policy.promote_redeploy_symbol: true` and a short
`redeploy_cooldown_minutes` so capital can rotate into the new preferred structure.

## Entry quality gates (edge)

Mechanical filters on vertical candidates (`entry_rules.vertical`):

| Gate | Behavior |
|------|----------|
| `short_delta_min` / `short_delta_max` | Short leg **must** fall in this \|Δ\| band. Missing greeks **fail closed**. The engine never falls back to a fixed % OTM strike. |
| `min_pop_pct` / `min_distance_to_be_pct` / `min_credit_to_width_pct` | Reject weak POP / BE cushion / credit-to-width |
| `min_short_otm_pct` | Reject when short strike OTM % of spot is below threshold |
| `max_adverse_day_change_pct` | Reject puts on a down day (or calls on an up day) beyond this % move |
| `reject_short_inside_1sigma` | Reject shorts inside 1σ expected move. **Fail-closed** when chain IV is missing (never silently passes). |
| `min_iv_rv_ratio` | Reject when `chain_iv / realized_vol <` threshold (e.g. `1.15`). Realized vol uses `regime.realized_vol_lookback` (default 20). **Fail-closed** when IV or RV is missing. |
| `put_credit_max_rsi2` / `call_credit_min_rsi2` | **Timing gates.** Reject put credits when the underlying's 2-period RSI is above `put_credit_max_rsi2` (no puts into a rip) and call credits when RSI(2) is below `call_credit_min_rsi2` (no calls into a dump). **Fail-closed** when daily candles are unavailable. Backtest sensitivity (45/60/65/70/off) found the put side counterproductive — the put-credit guard below is the real breakdown protection — so the pilot ships `put_credit_max_rsi2: null`. The call side (55) is kept: it only fires on call-credit scans. |

Iron condors honor `entry_rules.iron_condor.min_iv_rv_ratio` the same way.

Tick / candidate `market_context` surfaces `realized_vol_pct` and `iv_rv_ratio` for LLM review.

## Entry policy (`entry_policy`)

Controls scan order and the live LLM proceed gate. Defaults are intentionally strict:

| Field | Default | Meaning |
|-------|---------|---------|
| `mode` | `first_qualifying` | Stop after the first watchlist symbol that produces a candidate |
| `fallback_only_after_primary_exhausted` | `true` | Scan `role: fallback` symbols only if no primary candidate |
| `require_llm_proceed` | **`true`** | Live entries need a fresh fingerprint-matched LLM `proceed` (see `proceed_cache_minutes`) |
| `proceed_cache_minutes` | `45` | How long a cached proceed remains valid between LLM selection reviews |
| `entry_attempt_cooldown_minutes` | `30` | After a non-fill attempt, wait before retrying the same candidate |
| `promote_redeploy_symbol` | `false` | After thesis redeploy cooldown, optionally scan that symbol first |
| `fail_open_on_llm_defer` | **`true`** | When `false`, LLM `skip`/`defer`/`hold` **block** entries (fail-closed). When `true`, only `unexpected_catalyst` vetoes block; other defers are ignored (fail-open). |
| `post_stop_tightening` | `None` | **Post-stop caution.** For `cooldown_days` after a stop-loss exit, entries require `min_iv_rv_ratio` and `min_short_otm_pct` at the tightened values (max of base and tightened). Prevents stacking losses right after a stop. Example: `{ enabled: true, min_iv_rv_ratio: 1.25, min_short_otm_pct: 6.0, cooldown_days: 14 }`. |

Set `require_llm_proceed: false` only when you intentionally want mechanical-only entries
(still subject to all rules gates). With `llm.veto_entries: true` and the default policy,
no live entry executes without a valid proceed cache.

## LLM advisor (two-model)

When `llm.enabled: true`, the agent picks the model by phase:

| Phase | When | Model (`rules.yaml`) |
|-------|------|----------------------|
| **Selection** | Rules produced `candidate_entries`, every `review_every_ticks` | `llm.selection_model` (default: `anthropic/claude-sonnet-4`) |
| **Monitor** | Open positions, every `review_every_ticks` (or `monitor_review_every_ticks` when set) | `llm.monitor_model` (default: `google/gemini-2.5-flash`) |
| **Web** | Every `web_research_every_reviews` selection reviews | `llm.web_model` (default: `perplexity/sonar`) |

**Cadence:** Selection is **not** run every tick merely because candidates exist. It shares
the same tick throttle as monitor (`review_every_ticks`). A fresh candidate can wait up to
that many ticks before LLM review (and thus before a proceed-gated live entry). Direction
is fail-closed / conservative.

**Skipped when flat** — no open positions and no candidate entries (no LLM call).

Mechanical profit/stop/DTE exits run every tick without the LLM.

## Schedule (regular / overnight / at open)

See [AGENT_SCHEDULE.md](AGENT_SCHEDULE.md) for the full model.

| Session | LLM | Chains |
|---------|-----|--------|
| **regular** (market open) | selection + monitor | yes |
| **overnight** (`schedule.overnight.enabled`) | web digest only (~hourly) | no |
| **idle** (closed, overnight off) | none | no |

```yaml
schedule:
  tick_interval_seconds: 120
  overnight:
    enabled: true
    tick_interval_seconds: 3600
    web_digest: true
    skip_llm_when_flat: true
    alert_on_risk_only: true
```

Overnight digest uses `llm.prompts.overnight` and saves `open_playbook` in agent state for the first regular tick at the open.

### Configurable prompts (`llm.prompts`)

Each `rules.yaml` can define strategy-specific LLM instructions:

```yaml
llm:
  prompts:
    selection: |          # system: role + entry judgment (Sonnet)
    selection_web: |       # optional override when web_model runs
    selection_context: |   # user message: strategy thesis, account notes
    monitor: |             # system: open-position review (Flash)
    monitor_context: |     # user message: monitoring priorities
    overnight: |           # system: overnight web digest (Sonar)
    overnight_context: |   # user message: overnight priorities
```

Omit any field to use the built-in default for that phase. Run a separate rules file per strategy (conservative pilot vs aggressive spec) with different prompts, models, and `risk` limits.

| Flag | Default | Effect |
|------|---------|--------|
| `veto_entries` | true | Allow LLM entry veto — **engine honors only** `veto_category=unexpected_catalyst` with non-empty `evidence` (fail-open on calendar/math/vague defer) |
| `allow_llm_exits` | false | Execute exits on high-urgency LLM close recommendations |
| `allow_rule_suggestions` | true | After closes, append human-applied suggestions to `llm-suggestions-<rules-stem>.md` (no auto-mutate) |

### Narrow veto schema

Selection `new_entries` must include:

```json
{
  "recommendation": "proceed|defer|skip",
  "reasoning": "...",
  "veto_category": "none|unexpected_catalyst|other",
  "evidence": "concrete catalyst text when unexpected_catalyst"
}
```

Monitor phase forces `recommendation=skip` / `veto_category=none` in code so sticky FOMC defers cannot block entries.

### LLM scorecard

Every selection decision is journaled as `llm_entry_decision`. Fills link `llm_decision_id`; exits emit `llm_scorecard_resolve`.

```bash
schwab agent scorecard --rules-file rules/options-pilot-8709.yaml --json
```

Watch Overview shows `scorecard: vetoes N · ignored defer M · linked W/L …`. Primary health signal: **ignored defer rate** (noise from calendar/math inventing).

Rule-based profit/stop/DTE exits always run first; LLM adds judgment on top.

## Telegram notifications

When `notify.telegram.enabled: true`, set `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID` in `.env`.

- `notify_on_actions: true` — entries, exits, LLM alerts
- `notify_every_tick: true` — summary every tick (noisy)

Tick failures also alert: an `AGENT DEGRADED` message on the first failure and every 10th
consecutive one after (throttled to avoid spam), and `AGENT RECOVERED` once a tick succeeds
again.

## Error handling and backoff

Each tick failure is classified (`agent::resilience`) into one of three classes, which
determines both the backoff and the alert:

| Class | Examples | Backoff |
|-------|----------|---------|
| `recoverable` | Network timeouts, 5xx, 429, transient 401 | Exponential, 5s → 300s cap |
| `auth_fatal` | Refresh token invalid/expired/revoked, not authenticated | Fixed 60s — retries indefinitely until `schwab auth login` |
| `unexpected` | Anything else (a real bug) | Exponential, 5s → 300s cap, same as recoverable |

The agent never exits on a tick error (unattended operation) — it always backs off and
retries, logging which class fired so `recoverable` noise and `unexpected` bugs are visibly
distinguishable in the log/Telegram alert instead of looking identical.

## Manual options commands

```bash
schwab options schema --json
schwab options positions --account-number <hash> --json
schwab options validate --strategy vertical --params '{"underlying":"SPY",...}' --json
schwab options preview --account-number <hash> --strategy vertical --params '<json>' --json
schwab options open --account-number <hash> --strategy vertical --params '<json>' --trust --yes --json
schwab options close --account-number <hash> --position-id "<underlying>|<expiry>" --trust --yes --json
```

## v1 strategies

- **vertical** — put/call credit or debit spreads (`VERTICAL`, `NET_CREDIT`/`NET_DEBIT`)
- **iron_condor** — four-leg defined-risk condor (`IRON_CONDOR`, `NET_CREDIT`)

Both are IRA-safe (defined risk). Covered calls, CSPs, and collars are deferred to v2.

## State file

Agent state is written next to the rules file as `agent-state.json` (open positions, daily trade count, recent actions).

## Account types

Set `accounts[].type` to `margin`, `ira`, or `cash`. v1 only allows vertical and iron condor on all types.
