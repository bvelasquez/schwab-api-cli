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
| `dte_close` | 21 | Close when DTE ≤ 21 regardless |

Exits run **before** entry scans each regular tick. Marks come from live option chain **`debit_to_close`** (not Schwab `net_market_value`).

Monitor LLM context includes `mechanical_rules.stop_triggered` — only treat a stop as hit when that field is `true`. See [LLM_SCHEMA_REFERENCE.md](LLM_SCHEMA_REFERENCE.md#field-reference--exit_rules-mechanical--authoritative).

## Risk gates vs daily trade count

Hard gates for new entries:

- `entry_rules.*.max_open_positions`
- `risk.max_portfolio_risk_usd` / `risk.max_risk_per_trade_usd`

`risk.max_trades_per_day` is only a **soft churn cap** (redeploy after thesis exits, etc.). Set it to `0` for unlimited daily opens (still subject to open slots + $ risk).

## Regime-aware structure (`regime`)

When `regime.enabled: true`, the agent classifies SPY trend + VIX and scans **one** preferred structure:

| Regime | Typical map |
|--------|-------------|
| `low_vol_trend` / `elevated_vol` / `neutral` | `put_credit` |
| `bearish_trend` (below SMA50 and SMA200) | `call_credit` |
| `high_vol_chop` | `iron_condor` |
| `hostile` or VIX ≥ `pause_entries_vix_above` | pause new entries |

Requires `strategies.iron_condor.enabled: true` for condor regimes. Vertical call credits use the same delta/width rules as puts.

## LLM advisor (two-model)

When `llm.enabled: true`, the agent picks the model by phase:

| Phase | When | Model (`rules.yaml`) |
|-------|------|----------------------|
| **Selection** | Rules produced `candidate_entries` | `llm.selection_model` (default: `anthropic/claude-sonnet-4`) |
| **Monitor** | Open positions, every `review_every_ticks` | `llm.monitor_model` (default: `google/gemini-2.5-flash`) |
| **Web** | Every `web_research_every_reviews` selection reviews | `llm.web_model` (default: `perplexity/sonar`) |

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
| `veto_entries` | true | Block new entries when LLM says defer/skip |
| `allow_llm_exits` | false | Execute exits on high-urgency LLM close recommendations |

Rule-based profit/stop/DTE exits always run first; LLM adds judgment on top.

## Telegram notifications

When `notify.telegram.enabled: true`, set `TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID` in `.env`.

- `notify_on_actions: true` — entries, exits, LLM alerts
- `notify_every_tick: true` — summary every tick (noisy)

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
