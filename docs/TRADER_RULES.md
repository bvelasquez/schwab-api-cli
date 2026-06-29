# Equity swing trader (`schwab-trader`)

Separate CLI for **short/medium-term equity swing** trading on a dedicated account sleeve.
Shares OAuth and `safety.json` with `schwab`; does **not** mix commands with the options agent.

## Locked decisions (v1)

| Topic | Choice |
|-------|--------|
| Account | Beneficiary 9947 — dedicated equity swing sleeve |
| Horizon | Swing (2–30 days); intraday later |
| Direction | Long default; shorts schema-only until enabled |
| Capital | Fixed `$4,000` sleeve cap + 80% of free cash after options loss reserve |
| Brackets | Post-fill OCO (Option A) |
| Web picks | Perplexity via OpenRouter |
| Rule changes | Human writes initial YAML; LLM auto-adapts bounded fields + Telegram |
| CLI | Separate `schwab-trader` binary |

## Architecture

| Component | Location |
|-----------|----------|
| CLI binary | `schwab-trader` (`crates/schwab-trader`) |
| Rules | `rules/trader-swing-9947.yaml` |
| Runtime state | `rules/trader-state-swing-beneficiary-9947.json` |
| Journal | `rules/trader-journal-swing-beneficiary-9947.jsonl` |
| Options overlap | Reads `rules/agent-state-options-pilot-9947.json` for loss reserve |

## Safety model

| Layer | Purpose |
|-------|---------|
| `safety.json` | Hard ceilings (max trade value, order types, conditional orders) |
| `trader-rules.yaml` | Playbook, capital sleeve, watchlists |
| `capital_check` | Pre-trade budget (fixed cap + % free cash − options reserve) |
| LLM adaptation | Auto-tune bounded playbook fields; Telegram on change; immutable caps |

Enable conditional orders for OCO brackets:

```json
"allow_conditional_orders": true
```

## Capital formula (every entry)

```
options_reserved = options_agent_state.reserved_risk_usd() × (1 + buffer_pct/100)
free_cash        = max(0, cash_available − options_reserved − min_cash_floor)
pct_budget       = free_cash × max_pct_of_free_cash / 100
cap_remaining    = fixed_sleeve_cap_usd − equity_deployed (trader positions only)
tradable_budget  = min(pct_budget, cap_remaining)
```

`schwab-trader capital show --rules-file <path> --json` prints the full ledger.

## Bracket execution (v1)

**Post-fill OCO (Option A):**

1. Limit buy → wait for fill
2. Place OCO: limit sell (profit) + stop-limit sell (stop)
3. Dynamic adjustments: cancel/replace OCO legs on monitor ticks

All wall-clock session times use **`America/New_York`** (US Eastern, EST/EDT). The agent never uses your machine's local timezone (e.g. Pacific). Rules validation rejects other `schedule.timezone` values.

Check `market_clock` in tick JSON to verify: `now_eastern`, `regular_session_open`, `minutes_to_open`.

## Agent tick schedule

| Session | When | Sleep | LLM | Trades |
|---------|------|-------|-----|--------|
| **regular** | 9:30–16:00 ET | `tick_interval_seconds` (90s) | Selection when candidates; monitor every `review_every_ticks` | Yes |
| **premarket** | 8:00–9:30 ET (`premarket_scan`) | `premarket_tick_interval_seconds` (30m); faster in last 30 min before open | Web digest only (`web_model`) | No |
| **overnight** | Closed + `overnight.enabled` | `overnight.tick_interval_seconds` (3600s) | Web digest only if positions (or not flat) | No |
| **idle** | Closed, overnight off | 3600s | None | No |

`regular_tick_count` tracks only regular-session ticks so overnight/premarket wakes do not skew monitor LLM cadence.

## Agent tick (regular session)

1. **Reconcile** Schwab positions ↔ trader state (`reconcile.rs`)
2. **Drawdown check** — halt entries if `max_drawdown_halt_pct` breached
3. Quotes + technical indicators for watchlist (ranked candidates)
4. **Trailing stops** — OCO cancel/replace when profit threshold met (live)
5. **Closure** — OCO poll for stop/target; manual flatten for time/EOD/overnight only
6. Perplexity web picks → `dynamic_watchlist` (when enabled)
7. Mechanical entry filter → LLM selection
8. `capital_check` (incl. sibling sleeve + drawdown) → limit buy → post-fill OCO with retry
9. Monitor LLM → bounded rule adaptation (sim by default; live requires `adaptation.live_auto_apply`)

## Playbook styles

| Style | Rules file | Holds | Closure |
|-------|------------|-------|---------|
| **Swing** | `trader-swing-9947.yaml` | 2–30 days | Optional overnight |
| **Intraday** | `trader-intraday-9947.yaml` | Same session only | Flatten by 15:55 ET, no new entries after 15:30 ET |

Intraday uses **minute-bar** analytics (relative volume, SMA 9/20, tighter RSI band) and `time_stop_minutes`. OCO duration should be `DAY`, not `GTC`.

## Learning loop

After closed trades (sim or live), the agent runs a **learn** phase (`learn_model` + `prompts.learn`) when:

- `closed_trades_since_learn >= learn_min_closed_trades`, or
- `learn_every_ticks` elapsed with pending closed trades

The LLM receives `recent_closed_trades`, `sim_stats`, `regime`, `profile_catalog`, current `adaptable_playbook`, and `adaptation_bounds`. Patches are validated, clamped, and written to the rules YAML (simulate + live; dry-run journals only).

## Regime-aware adaptation

Each regular-hours tick:

1. **Regime detection** — SPY trend (SMA 50/200), VIX level, realized-vol percentile → `low_vol_trend` | `elevated_vol` | `high_vol_chop` | `neutral`
2. **Profile selection** — mechanical map (`adaptation.profile_map`) picks a named profile; LLM may override via `profile_selection` in selection/monitor/learn reviews
3. **Effective playbook** — baseline `playbook` merged with profile `overrides` (in-memory; baseline YAML unchanged)
4. **Monitor OCO adjust** — `tighten_exits` / `widen_exits` recommendations cancel/replace broker OCO within bounds
5. **ATR-normalized sizing** — profiles may set `position_size.method: atr_normalized` to scale risk down in high vol

Built-in profiles (when `adaptation.profiles` omitted): `baseline`, `low_vol_trend`, `high_vol_chop`, `elevated_vol`.

| Profile | Typical use |
|---------|-------------|
| `baseline` | Default playbook from YAML |
| `low_vol_trend` | Wider targets, normal size, calm uptrend |
| `elevated_vol` | Reduced ATR-scaled size, moderate brackets |
| `high_vol_chop` | No new entries, defensive size, wider stops |

```bash
# Intraday paper trading + learn loop
schwab-trader agent run rules/trader-intraday-9947.yaml --simulate --once --json
```

## Runtime modes

| Mode | Flag | Orders | Positions | ROI |
|------|------|--------|-----------|-----|
| **Dry-run** | `--dry-run` | None | Not persisted | No (exit evaluation only) |
| **Simulation** | `--simulate` | None (instant paper fill) | Virtual ledger in state file | Yes — `sim stats` |
| **Live** | `--trust --yes` | Real Schwab orders | Broker + state | Real P&L |

`--dry-run` and `--simulate` are mutually exclusive. Simulation uses **live quotes** for mark-to-market and stop/profit/time exits (including ATR trailing), but debits a separate paper cash balance (`simulation.starting_cash_usd` in rules). LLM rule adaptation can run against simulated outcomes when `simulation.allow_rule_adaptation` is true.

### Simulation analysis

Each `--simulate` tick appends a `sim_tick_summary` event to the journal. Entries/exits log `sim_entry_filled` / `sim_exit_filled` with brackets, P&L, profile, and regime.

```bash
# Quick ROI
schwab-trader sim stats --rules-file rules/trader-swing-9947.yaml --json

# Full week review (journal + ledger + adaptations + equity curve)
schwab-trader sim report --rules-file rules/trader-swing-9947.yaml --json

# Export to file for offline analysis
schwab-trader sim report --rules-file rules/trader-swing-9947.yaml --output sim-week-report.json
```

## Quick start

```bash
# Auth (once, shared with schwab)
schwab auth login

# Validate rules
schwab-trader rules validate rules/trader-swing-9947.yaml --json

# Capital ledger
schwab-trader capital show --rules-file rules/trader-swing-9947.yaml --json

# One-shot scan (no orders)
schwab-trader scan --rules-file rules/trader-swing-9947.yaml --json

# Dry-run one tick (scan + capital + LLM + entry preview, no orders)
schwab-trader agent run rules/trader-swing-9947.yaml --dry-run --once --json

# Paper trading: simulated fills/exits, ROI tracking, no Schwab orders
schwab-trader agent run rules/trader-swing-9947.yaml --simulate --once --json
schwab-trader sim stats --rules-file rules/trader-swing-9947.yaml --json
schwab-trader sim reset --rules-file rules/trader-swing-9947.yaml --json

# Watch TUI + embedded agent (dry-run safe)
schwab-trader watch --rules-file rules/trader-swing-9947.yaml --dry-run

# Watch TUI in simulation mode (paper portfolio + live quotes)
schwab-trader watch --rules-file rules/trader-swing-9947.yaml --simulate

# Intraday (sim)
schwab-trader agent run rules/trader-intraday-9947.yaml --simulate --once --json
schwab-trader watch --rules-file rules/trader-intraday-9947.yaml --simulate

# Monitor state only (no agent)
schwab-trader watch --rules-file rules/trader-swing-9947.yaml --monitor-only

# Live (requires disclaimer + --trust --yes)
schwab disclaimer accept --yes
schwab-trader watch --rules-file rules/trader-swing-9947.yaml --trust --yes
```

## LLM governance

- **Human:** initial `trader-rules.yaml` (accounts, capital caps, direction, short toggle, custom profiles)
- **Automatic (regime):** mechanical profile from SPY/VIX/realized vol each tick when `adaptation.regime_auto_select`
- **Automatic (LLM):** `profile_selection` + bounded `rule_patches`; monitor OCO tighten/widen
- **Automatic (sim):** bounded baseline YAML tuning when `simulation.allow_rule_adaptation` is true
- **Automatic (live):** baseline YAML patches only when `adaptation.live_auto_apply: true` (default **false**); Telegram on apply
- **Immutable by LLM:** `capital.fixed_sleeve_cap_usd`, `max_pct_of_free_cash`, `playbook.direction`, `accounts`

## Data sources (`sources.feeds`)

Configure URLs, JSON APIs, and RSS feeds in rules YAML. The agent **prefetches** enabled feeds before each LLM call and injects them into the user message as `source_feeds` (plus `source_feed_catalog`).

| Field | Description |
|-------|-------------|
| `id` | Unique key; cite in `web_insights` / candidate reasoning |
| `kind` | `url` (HTML/text), `api` (JSON pretty-printed), `rss` (item titles + descriptions) |
| `url` | Must be `https://` (or `http://127.0.0.1` for local dev) |
| `phases` | LLM phases: `selection`, `web`, `monitor`, `learn`, `premarket_digest`, `overnight_digest`, or `all`. Empty = all digest + regular phases except `learn` |
| `auth` | Optional; `bearer` or `header` — token read from `token_env` (never put secrets in YAML) |
| `max_bytes` | Truncate fetched body (default 12000) |
| `timeout_seconds` | HTTP timeout (default 15) |

```yaml
sources:
  feeds:
    - id: marketwatch_headlines
      label: MarketWatch Top Stories
      kind: rss
      url: https://feeds.marketwatch.com/marketwatch/topstories/
    - id: my_api
      kind: api
      url: https://api.example.com/v1/sentiment
      auth:
        kind: bearer
        token_env: MY_API_TOKEN
      phases: [all]
```

CLI:

```bash
# List configured feeds
schwab-trader sources list --rules-file rules/trader-swing-9947.yaml --json

# Fetch all enabled feeds (or filter with --phase premarket_digest)
schwab-trader sources test --rules-file rules/trader-swing-9947.yaml --json
```

Feeds run **in addition to** Perplexity web research (`sources.web`). When `source_feeds` is present, the LLM is instructed to treat prefetched content as primary ground truth.

## v1 scope

- [x] Separate `schwab-trader` binary
- [x] Rules schema + validate
- [x] Capital ledger + pre-trade sanity
- [x] Post-fill OCO with retry + unbracketed recovery
- [x] Broker reconciliation every tick
- [x] Drawdown halt + trailing stops (OCO replace)
- [x] Candidate ranking + web picks → dynamic watchlist
- [x] Agent loop (scan, entries, journal)
- [x] LLM hooks (selection/monitor/adapt)
- [x] Configurable URL/API/RSS feeds for LLM context
- [ ] Full watch TUI (deferred)
- [ ] Intraday bars (deferred)
- [ ] Short selling in production (schema only; default off)

## Related

- [TRADER_ROLLOUT.md](TRADER_ROLLOUT.md) — live deployment checklist
- [OPTIONS_RULES.md](OPTIONS_RULES.md) — options agent on same account
- [LLM_SCHEMA_REFERENCE.md](LLM_SCHEMA_REFERENCE.md) — options LLM schemas (trader uses parallel schemas in crate)
