# LLM schema reference — trade plans & options rules

This document is for **LLMs and automation agents** that author configuration files for the `schwab` CLI. Humans can read it too, but the structure is optimized for copy-paste into system prompts.

**Canonical machine-readable schemas:**

| Artifact | Validate | Schema JSON |
|----------|----------|-------------|
| Equity trade plan | `schwab plan validate <file>` | `schwab plan schema --json` |
| Options agent rules | `schwab agent validate <file>` | `schwab agent schema --json` |
| LLM plan workflow | — | `schwab plan prompt --json` |
| Full CLI discovery | — | `schwab instructions --json` |

Related docs: [plans/TRADE_PLAN.md](../plans/TRADE_PLAN.md) (equity plans), [OPTIONS_RULES.md](OPTIONS_RULES.md) (agent ops), [AGENT_SCHEDULE.md](AGENT_SCHEDULE.md) (regular / overnight sessions).

---

## Two authoring modes

| Mode | File location | Executes | Use when |
|------|---------------|----------|----------|
| **Trade plan** | `plans/*.yaml` | One-shot multi-step **equity** buy/sell | Rebalance ETFs, rotate cash, trim/add stock |
| **Options rules** | `rules/*.yaml` | Long-running **options agent** daemon | Automated put credit spreads, iron condors, mechanical exits |

**Hard limits:** Both modes are capped by `safety.json` (max trade size, allowed symbols, option flags). LLM-authored configs **cannot** bypass `safety.json`.

**Live execution:** Agent and plan runs require explicit human approval: `--trust --yes`. Always validate and dry-run first.

---

## Authoring workflow (all LLMs)

```
1. schwab instructions --json          # discovery + safety rules
2. schwab safety show --json             # hard limits
3. schwab accounts numbers --json        # account_hash values (NOT plain account numbers)
4. schwab portfolio summary --json       # current holdings
5. Author YAML/JSON file
6. schwab plan validate …  OR  schwab agent validate …
7. schwab plan run … --dry-run  OR  schwab agent run … --dry-run --once
8. Live only when user requests: … --trust --yes
```

---

## Part 1 — Equity trade plans

### Purpose

A trade plan is a **sequential list of equity orders** (buy/sell). The CLI validates each step against `safety.json`, submits orders, and optionally waits for fills before the next step.

Plans are **not** for options spreads — use the options agent or `schwab options open` for those.

### Required top-level fields

| Field | Type | Rules |
|-------|------|-------|
| `version` | integer | Must be `1` |
| `plan_id` | string | Stable slug, e.g. `ira-trim-sgov-2026-06-19` |
| `title` | string | Short human title |
| `account_hash` | string | `hashValue` from `schwab accounts numbers --json` |
| `created_at` | ISO-8601 UTC | e.g. `2026-06-19T12:00:00Z` |
| `steps` | array | At least one step |

### Optional top-level fields

| Field | Type | Purpose |
|-------|------|---------|
| `account_label` | string | Display only |
| `author` | string | e.g. `llm-agent` |
| `rationale` | string | Thesis and risks (recommended) |
| `assumptions.notes` | string | Price/limit assumptions |
| `assumptions.limit_prices` | map symbol → number | Reference prices used |
| `execution` | object | Plan-wide order-wait behavior |

### Step fields

| Field | Required | Values / notes |
|-------|----------|----------------|
| `id` | yes | Unique within plan, e.g. `step-01-sell-sgov` |
| `side` | yes | `buy` or `sell` (lowercase) |
| `symbol` | yes | Equity ticker |
| `quantity` | yes | Shares, > 0 |
| `order_type` | no | `limit` (default) or `market` |
| `limit_price` | if limit | Per-share limit |
| `duration` | no | `day`, `gtc`, `fok` |
| `session` | no | `normal`, `am`, `pm`, `seamless` |
| `note` | no | Step rationale |
| `wait_until` | no | `accepted`, `filled`, `terminal` — wait before next step |

### Execution block

```yaml
execution:
  stop_on_error: true              # halt on first failed step
  pause_seconds_between_steps: 2     # throttle between live steps
  wait_for_fill: true              # shorthand: default wait = filled
  default_wait_until: filled       # alternative to wait_for_fill
  fill_timeout_seconds: 3600
  poll_interval_seconds: 10
  proceed_on_partial_fill: false
```

When rotating cash, **sell before buy** and set `wait_until: filled` on sells so buying power is available.

### Safety rules when generating steps

Read `schwab safety show --json`. **Each step** must satisfy:

- `quantity <= max_shares_per_order`
- For limits: `quantity * limit_price <= max_trade_value_usd`
- `quantity * limit_price <= max_trade_pct_of_equity * account_equity`

If a rebalance exceeds one step, **split into multiple steps** with unique ids.

### Minimal plan template

```yaml
version: 1
plan_id: example-rebalance-2026-06-19
title: Example sell/buy pair
account_hash: "<hash-from-schwab-accounts-numbers>"
created_at: "2026-06-19T12:00:00Z"
rationale: |
  Brief thesis for the rebalance.
execution:
  wait_for_fill: true
  fill_timeout_seconds: 3600
steps:
  - id: step-01-sell
    side: sell
    symbol: SGOV
    quantity: 14
    order_type: limit
    limit_price: 100.55
    wait_until: filled
  - id: step-02-buy
    side: buy
    symbol: JPST
    quantity: 28
    order_type: limit
    limit_price: 50.50
```

### LLM rules for plans

1. Ground plans in live `portfolio summary` and `safety show`.
2. Use **account hash**, never plain account number.
3. Prefer limit orders with explicit `limit_price`.
4. Never exceed per-step safety limits — batch instead.
5. Include `rationale`.
6. Output valid YAML unless JSON is requested.
7. Do **not** include options, short sales, or symbols blocked in `safety.json`.

Full prompt payload: `schwab plan prompt --json`

---

## Part 2 — Options agent rules

### Purpose

A rules file configures a **long-running daemon** that:

1. Reconciles open option positions with Schwab each tick
2. Runs **mechanical exits** every regular-hours tick (profit / stop / DTE)
3. Scans watchlist for new spread entries
4. Optionally calls an OpenRouter LLM for entry veto and position commentary
5. Optionally sends Telegram notifications

The LLM **does not replace** mechanical exits unless `allow_llm_exits: true` (off by default).

### Required top-level fields

| Field | Type | Rules |
|-------|------|-------|
| `version` | integer | Must be `1` |
| `agent_id` | string | Stable id, e.g. `my-options-pilot` |
| `accounts` | array | At least one enabled account with `hash` |
| `watchlist` | array | Underlying tickers to scan, e.g. `SPY`, `IWM` |

### Full schema (all sections)

```yaml
version: 1
agent_id: my-strategy-id

accounts:
  - hash: "<hash-from-schwab-accounts-numbers>"
    label: "IRA (example)"
    type: ira                    # margin | ira | cash
    enabled: true

schedule:
  tick_interval_seconds: 120     # regular session poll (min 5)
  market_hours_only: true
  timezone: America/New_York
  overnight:                     # optional — see AGENT_SCHEDULE.md
    enabled: true
    tick_interval_seconds: 3600
    web_digest: true
    skip_llm_when_flat: true
    alert_on_risk_only: true

strategies:
  vertical:
    enabled: true
  iron_condor:
    enabled: false

watchlist:
  - SPY
  - IWM

entry_rules:
  vertical:
    type: put_credit             # put_credit | call_credit | put_debit | call_debit
    dte_min: 30
    dte_max: 45
    min_credit: 0.25             # per-share minimum net credit
    max_width: 2                 # strike width (short − long)
    short_delta_min: 0.15
    short_delta_max: 0.30
    max_open_positions: 2
    max_contracts_per_trade: 1
  iron_condor:
    dte_min: 30
    dte_max: 45
    min_credit: 1.00
    wing_width: 5
    short_delta: 0.16
    max_open_positions: 2
    max_contracts_per_trade: 1

exit_rules:
  profit_target_pct: 50          # close when ≥50% of entry credit captured
  stop_loss_pct: 200             # close when debit_to_close ≥ 2× entry credit
  dte_close: 21                  # close when DTE ≤ this

risk:
  max_portfolio_risk_usd: 4000
  max_risk_per_trade_usd: 500
  max_trades_per_day: 1          # caps NEW entries only; exits/monitor continue
  allowed_underlyings:
    - SPY
    - IWM
  blocked_events: []             # reserved for future event gates

execution:
  order_type: limit
  require_preview: true
  wait_for_fill: true
  fill_timeout_seconds: 300

llm:
  enabled: true
  selection_model: anthropic/claude-sonnet-4
  monitor_model: google/gemini-2.5-flash
  web_model: perplexity/sonar
  review_every_ticks: 5          # monitor LLM every N regular ticks
  web_research_every_reviews: 3
  max_tokens: 2000
  veto_entries: true             # LLM can block new entries (defer/skip)
  allow_llm_exits: false         # LLM cannot auto-close (recommended)
  prompts:
    selection: |                 # system: entry judgment
    selection_web: |             # optional override when web_model runs
    selection_context: |         # user: strategy thesis, account notes
    monitor: |                   # system: open-position review
    monitor_context: |           # user: monitoring priorities
    overnight: |                 # system: overnight web digest
    overnight_context: |         # user: overnight priorities

notify:
  telegram:
    enabled: true
    notify_on_actions: true
    notify_every_tick: false
```

### Field reference — `entry_rules.vertical`

| Field | Meaning |
|-------|---------|
| `type` | `put_credit` (default income strategy), or call/put debit/credit variants |
| `dte_min` / `dte_max` | Days to expiration window for new entries |
| `min_credit` | Minimum net credit per share (×100 = dollars per contract) |
| `max_width` | Dollar width between short and long strike |
| `short_delta_min` / `short_delta_max` | Short leg delta filter (proxy for % OTM) |
| `max_open_positions` | Cap concurrent spreads per account |
| `max_contracts_per_trade` | Contracts per new entry |

### Field reference — `exit_rules` (mechanical — authoritative)

| Field | Default | Behavior |
|-------|---------|----------|
| `profit_target_pct` | 50 | Close when captured profit ≥ this % of entry credit |
| `stop_loss_pct` | 200 | Close when **debit_to_close** ≥ `(stop_loss_pct/100) × entry_credit` |
| `dte_close` | 21 | Close when days to expiration ≤ this |

**Stop example:** Entry credit $0.25/share, `stop_loss_pct: 200` → mechanical stop at **$0.50/share** debit to close.

**Critical:** Mechanical exits use **option chain** `debit_to_close` (short ask − long bid), **not** Schwab `net_market_value`. The monitor LLM receives `mechanical_rules.stop_triggered` — only alert a stop hit when that field is `true`.

### Field reference — `risk`

| Field | Meaning |
|-------|---------|
| `max_portfolio_risk_usd` | Cap total defined risk across open spreads |
| `max_risk_per_trade_usd` | Cap risk per new entry (width − credit) × 100 × contracts |
| `max_trades_per_day` | Max **new entries** per calendar day; **exits and monitoring continue** when cap is hit |
| `allowed_underlyings` | Empty = watchlist only; non-empty = extra filter |
| `blocked_events` | Reserved (v2) |

### Field reference — `llm`

| Field | Default | Meaning |
|-------|---------|---------|
| `enabled` | false | Master switch for OpenRouter calls |
| `selection_model` | claude-sonnet-4 | Entry veto when candidates exist |
| `monitor_model` | gemini-2.5-flash | Open-position review |
| `web_model` | perplexity/sonar | Web research (selection + overnight) |
| `review_every_ticks` | 5 | Monitor phase cadence (**regular** ticks only) |
| `veto_entries` | true | Block entry on LLM `defer` / `skip` |
| `allow_llm_exits` | false | Execute LLM `close` with high urgency |
| `prompts.*` | built-in defaults | Override per strategy file |

### LLM phases (what the agent calls you for)

| Phase | When | Model | Your job |
|-------|------|-------|------------|
| **selection** | Rules found `candidate_entries` | `selection_model` | `proceed` / `defer` / `skip` new entries |
| **monitor** | Open positions, every N regular ticks | `monitor_model` | `hold` / `watch` / `close` per position (advisory unless `allow_llm_exits`) |
| **overnight** | Market closed, `overnight.web_digest` | `web_model` | Build open playbook; `new_entries` must be `skip` |

**Skipped when flat:** No open positions and no candidates → no LLM call (saves cost).

### Monitor context the agent sends (do not invent)

Each `open_positions[]` item in the LLM context JSON includes:

| Key | Use |
|-----|-----|
| `status` | `holding` or `exit: <reason>` — if holding, mechanical stop has **not** fired |
| `entry_credit` | Per-share credit at open |
| `debit_to_close` | Per-share cost to close (from chain) |
| `profit_pct` | % of entry credit captured |
| `dte` | Days to expiration |
| `mechanical_rules.stop_debit_threshold_per_share` | Stop price level |
| `mechanical_rules.stop_triggered` | **Only** cite stop hit when `true` |
| `market_context.short_delta` | Short leg delta |
| `market_context.short_otm_pct` | % OTM on short strike |
| `market_context.watch_near_short_strike` | Price near short strike |
| `net_market_value` | Schwab leg MV sum in **dollars** — **do not** use for stop/profit rules |

### LLM response schema (options agent)

The agent expects structured JSON from the monitor/selection models:

```json
{
  "market_commentary": "string",
  "web_insights": ["string"],
  "positions": [
    {
      "position_id": "IWM|2026-07-31",
      "recommendation": "hold|watch|close",
      "urgency": "low|medium|high",
      "reasoning": "string"
    }
  ],
  "new_entries": {
    "recommendation": "proceed|defer|skip",
    "reasoning": "string"
  },
  "risk_alerts": ["string"]
}
```

**Monitor LLM rules:**

- Do **not** duplicate mechanical profit/stop/DTE — those run every tick.
- Do **not** put stop-loss hits in `risk_alerts` unless `mechanical_rules.stop_triggered` is true.
- Use `market_context` greeks for hold/watch/close — not memory.
- `close` + `urgency: high` only for thesis-breaking gap/assignment risk.
- Early in a 30–45 DTE trade, mark swings are normal; theta needs time.

### Session modes (schedule)

See [AGENT_SCHEDULE.md](AGENT_SCHEDULE.md).

| Session | Chains | Entries | Mechanical exits | LLM |
|---------|--------|---------|------------------|-----|
| `regular` | yes | yes | yes | selection + monitor |
| `overnight` | no | no | no | web digest only |
| `idle` | no | no | no | none |

### Runtime state (do not author — agent writes)

Next to the rules file: `agent-state-<agent_id>.json` with `open_positions`, `trades_today`, `open_playbook`, `last_actions`.

One live agent per account per strategy unless you intend overlapping logic.

### Example rules files

| File | Purpose |
|------|---------|
| [rules/options-rules.example.yaml](../rules/options-rules.example.yaml) | All fields, commented |
| [rules/options-rules.example.yaml](../rules/options-rules.example.yaml) | Public template — copy locally and set account hash |
| [rules/options-monthly-income.yaml](../rules/options-monthly-income.yaml) | Monthly income PDF-aligned |

### LLM rules for options rules files

1. One `agent_id` per strategy; match `accounts[].hash` to the target account.
2. IRA accounts: use **defined-risk** strategies only (`vertical`, `iron_condor`).
3. Enable options in `safety.json` (`allow_option_orders`, `allow_complex_orders`) before live agent.
4. Set `OPENROUTER_API_KEY` when `llm.enabled: true`.
5. Keep `allow_llm_exits: false` unless the user explicitly wants discretionary LLM closes.
6. Tune `entry_rules` and `exit_rules` together — document thesis in `llm.prompts.*_context`.
7. Validate: `schwab agent validate rules/<file>.yaml`
8. Dry-run one tick: `schwab agent run rules/<file>.yaml --dry-run --once`

---

## Part 3 — `safety.json` (applies to everything)

```bash
schwab safety show --json
schwab safety init --yes   # write defaults
```

Relevant flags for options:

- `allow_option_orders` — required for option legs
- `allow_complex_orders` — required for spreads (`NET_CREDIT`, `NET_DEBIT`)
- `max_trade_value_usd`, `max_shares_per_order` — cap plan steps and order size
- `allowed_symbols` / `blocked_symbols` — symbol allowlists

The CLI validates **every** plan step and agent order against these limits.

---

## Part 4 — Quick decision tree

```
Need to trade stock/ETF once or rebalance?
  → Trade plan (plans/*.yaml)
  → schwab plan validate / plan run

Need ongoing options income / spreads?
  → Options rules (rules/*.yaml)
  → schwab agent validate / agent run

Need a single manual spread?
  → schwab options open --strategy vertical --params '…'
```

---

## Part 5 — Environment variables

| Variable | Required for |
|----------|----------------|
| `SCHWAB_APP_KEY`, `SCHWAB_APP_SECRET` | All API calls |
| `OPENROUTER_API_KEY` | `llm.enabled: true` in rules |
| `TELEGRAM_BOT_TOKEN`, `TELEGRAM_CHAT_ID` | `notify.telegram.enabled: true` |

Never commit `.env`, tokens, or real account hashes in public repos.
