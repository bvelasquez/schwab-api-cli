# schwab-api-cli

Agent-first Rust CLI for the [Charles Schwab Trader API](https://developer.schwab.com/) (Accounts and Trading Production). Built for **LLM-driven workflows**: discover capabilities via JSON, generate trade plans, validate against hard safety limits, and execute with explicit trust mode.

## Features

- **OAuth 2.0** — browser login, token refresh, secure local storage
- **Read** — accounts, positions, orders, transactions, user preferences
- **Market data** — quotes, price history, instrument fundamentals, market hours
- **Portfolio summary** — aggregated equity and holdings across accounts
- **Trading** — `trade buy` / `trade sell` with preview and safety guardrails
- **Trade plans** — YAML/JSON multi-step rebalances; LLM-authored, CLI-validated
- **Options agent** — long-running daemon from `rules/*.yaml`; put credit spreads, mechanical exits, OpenRouter LLM advisor, Telegram alerts
- **Order wait** — poll until limit orders fill before advancing a plan
- **Safety config** — `safety.json` enforces max trade size, symbols, order types (cannot be bypassed)
- **Trust mode** — autonomous agent execution requires `--trust --yes`

## Requirements

- Rust 1.75+ ([rustup](https://rustup.rs/))
- Schwab Developer Portal app with **Trader API – Individual** (Production)
- Same app should also enable **Market Data Production** for quotes and history
- macOS / Linux / Windows

## Quick start

```bash
git clone https://github.com/bvelasquez/schwab-api-cli.git
cd schwab-api-cli

# Build and install the `schwab` binary
cargo install --path crates/schwab-cli

# Configure credentials
cp .env.example .env
# Edit .env — see Configuration below

# Authenticate (opens browser)
schwab auth login

# Agent discovery
schwab capabilities --json
schwab instructions --json
schwab portfolio summary --json
```

## Configuration

### Environment variables (`.env`)

Copy `.env.example` to `.env` in the project root (or `~/.config/schwabinvestbot/.env`):

| Variable | Required | Description |
|----------|----------|-------------|
| `SCHWAB_APP_KEY` | Yes | App key from Schwab Developer Portal |
| `SCHWAB_APP_SECRET` | Yes | App secret |
| `SCHWAB_REDIRECT_URI` | No | Default `https://127.0.0.1:8182` (must match portal) |
| `SCHWAB_TOKEN_DIR` | No | Override token storage directory |
| `SCHWAB_SAFETY_CONFIG` | No | Override path to `safety.json` |
| `SCHWAB_MODE` | No | `agent` (default) or `human` |
| `SCHWAB_OUTPUT` | No | `pretty`, `json`, or `md` |
| `OPENROUTER_API_KEY` | No | OpenRouter API key (required when `llm.enabled` in a rules file) |
| `TELEGRAM_BOT_TOKEN` | No | Telegram bot token from [@BotFather](https://t.me/BotFather) |
| `TELEGRAM_CHAT_ID` | No | Telegram chat ID for agent notifications (DM or group) |

**Never commit `.env` or tokens.** They are listed in `.gitignore`.

### Schwab Developer Portal setup

1. Create an app at [developer.schwab.com](https://developer.schwab.com/)
2. Enable **Trader API – Individual** (Production)
3. Enable **Market Data Production** (quotes, price history, instruments)
4. Set callback URL to `https://127.0.0.1:8182` (HTTPS required)
5. Copy App Key and Secret into `.env`

### Token storage

OAuth tokens are saved to:

- **macOS:** `~/Library/Application Support/schwabinvestbot/tokens.json`
- **Linux:** `~/.config/schwabinvestbot/tokens.json`

### Safety limits (`safety.json`)

Hard trading limits for humans and agents:

```bash
schwab safety init --yes    # write defaults
schwab safety show --json   # view active limits
```

Default location: platform config dir (`schwabinvestbot/safety.json`). See `safety.json.example`.

Example limits: max trade value, max shares per order, allowed symbols, blocked symbols, allowed order types.

## CLI modes

| Mode | Flag | Behavior |
|------|------|----------|
| **Agent** (default) | `--mode agent` | Structured JSON envelopes; non-interactive |
| **Human** | `--mode human` | Guided prompts when args are omitted |

Global flags: `--json`, `--yes`, `--trust`, `--dry-run`

## Authentication

```bash
schwab auth login           # OAuth browser flow
schwab auth status --json   # token expiry
schwab auth refresh         # refresh access token
schwab auth logout          # delete stored tokens
```

## Reading data

```bash
schwab accounts numbers --json          # account numbers + hash values
schwab accounts list --json             # positions (default)
schwab portfolio summary --json         # cross-account summary
schwab orders list <account_hash> --json
schwab transactions list <account_hash> --json
schwab user preference --json
```

**Important:** Use `hashValue` from `accounts numbers` as `{accountNumber}` in trading endpoints — not the plain account number.

## Market data

Uses the same OAuth tokens as the Trader API (`https://api.schwabapi.com/marketdata/v1`).

```bash
# Agent research dossier (quote + fundamentals + 1mo history + web research hints)
schwab market info SGOV --json
schwab market info SGOV,JPST,AAPL --json

# Lower-level endpoints
# Market hours (works when markets are closed)
schwab market hours --markets equity --json

# Live quotes — quote + fundamentals for company info
schwab market quotes --symbols SGOV,JPST --fields quote,fundamental,reference --json
schwab market quote SGOV --fields all --json

# Price history (OHLCV candles)
schwab market history AAPL \
  --period-type month --period 1 --frequency-type daily --json

# Company / instrument fundamentals
schwab market instrument --symbol AAPL --projection fundamental --json

# Symbol search
schwab market instrument --symbol SGO --projection symbol-search --json
```

Quote `fields`: `all`, `quote`, `fundamental`, `reference`, `extended`, `regular`

Instrument `projection`: `symbol-search`, `fundamental`, `search`, `desc-search`, etc.

## Trading

### Single orders

```bash
# Dry-run (validates safety, no order sent)
schwab trade buy --dry-run \
  --account-number <hash> --symbol AAPL --quantity 1 \
  --order-type limit --price 150 --json

# Live (agent mode — both flags required)
schwab trade buy --trust --yes \
  --account-number <hash> --symbol AAPL --quantity 1 \
  --order-type limit --price 150 --json
```

Low-level JSON orders: `schwab orders place`, `schwab orders preview`, etc.

### Options and complex orders

Schwab supports **EQUITY** and **OPTION** orders including spreads, OCO, and TRIGGER sequences. Use raw JSON via `orders place` / `orders preview`:

```bash
# Schema + official Schwab examples (vertical spread, OCO, trailing stop, etc.)
schwab orders schema --json

# Validate shape + safety.json before preview/place
schwab orders validate --order '{"orderType":"NET_DEBIT",...}' --json

# Preview then place a complex order
schwab orders preview --account-number <hash> --order '<json>' --json
schwab orders place --account-number <hash> --order '<json>' --trust --yes --json
```

Enable in `safety.json` as needed:
- `allow_option_orders` — single-leg and spread option legs
- `allow_complex_orders` — multi-leg spreads (`NET_DEBIT`, `NET_CREDIT`)
- `allow_conditional_orders` — `OCO`, `TRIGGER` with `childOrderStrategies`

Option symbol format: `UNDERLYING(6 chars) | YYMMDD | C/P | STRIKE` — e.g. `XYZ   240315C00500000`

Fields supported include `cancelTime`, `complexOrderStrategyType` (`VERTICAL`, `IRON_CONDOR`, etc.), and `orderStrategyType` (`SINGLE`, `OCO`, `TRIGGER`).

### Order status / wait

After placing an order, the response includes `order_id` (parsed from the `Location` header):

```bash
schwab orders get <hash> <order_id> --json

# Poll until filled (or timeout)
schwab orders wait <hash> <order_id> \
  --until filled \
  --timeout-seconds 3600 \
  --interval-seconds 5 \
  --json
```

`--until` values: `accepted`, `filled`, `terminal`

## Trade plans (LLM workflow)

Trade plans are YAML/JSON files under `plans/` that describe multi-step rebalances.

```bash
schwab plan schema --json     # JSON Schema
schwab plan prompt --json     # LLM instructions + template
schwab plan validate plans/my-plan.yaml
schwab plan run plans/my-plan.yaml --dry-run --json
schwab plan run plans/my-plan.yaml --trust --yes --json
```

See `plans/TRADE_PLAN.md` for the file format.

### Fill-aware plan execution

Plans can wait for limit orders to fill before the next step:

```yaml
execution:
  wait_for_fill: true
  fill_timeout_seconds: 3600
  poll_interval_seconds: 10
steps:
  - id: step-01-sell
    side: sell
    symbol: SGOV
    quantity: 14
    order_type: limit
    limit_price: 100.55
    wait_until: filled
```

### Safety + trust model

| Action | Agent mode requirement |
|--------|------------------------|
| Read (portfolio, orders get) | None |
| Auth refresh/logout | `--yes` |
| Trade / plan run | `--trust --yes` |
| Dry-run | Always allowed |

`safety.json` limits apply to **every** step — LLM instructions cannot override them.

## Options trading agent

The options agent is a **long-running process** that reads a `rules/*.yaml` file, scans Schwab option chains on a schedule, applies mechanical entry/exit rules, optionally consults an **OpenRouter LLM** for entry judgment and position review, and can push **Telegram** notifications.

See also [docs/OPTIONS_RULES.md](docs/OPTIONS_RULES.md) for a concise reference.

### Architecture

```
Every tick (e.g. 120s)
  │
  ├─ 1. Reconcile open positions (Schwab ↔ agent-state.json)
  ├─ 2. EXIT scan (rules — every tick, no LLM)
  │      profit_target_pct · stop_loss_pct · dte_close
  ├─ 3. ENTRY scan (rules — chain, delta, credit, DTE)
  ├─ 4. LLM review (optional — selection OR monitor OR skip)
  │      Sonnet on new signals · Flash on open positions · Sonar for web
  └─ 5. Execute entries (if LLM did not veto) · save state
```

| Layer | File | Role |
|-------|------|------|
| Hard ceiling | `safety.json` | CLI rejects orders exceeding limits (cannot bypass) |
| Strategy brain | `rules/*.yaml` | What to trade, when, how much, LLM prompts |
| LLM advisor | `rules.yaml` → `llm` | Entry veto + qualitative monitoring (optional) |
| Persistence | `rules/agent-state.json` | Open positions, tick count, daily trades, LLM history |

**Important:** Mechanical exits (50% profit, 2× credit stop, 21 DTE) run **every tick** from live chain quotes. The LLM does not replace those rules — it adds judgment on entries and optional commentary on open trades.

### Bundled rules files

| File | Purpose |
|------|---------|
| [rules/options-rules.example.yaml](rules/options-rules.example.yaml) | Template with all fields documented |
| [rules/options-pilot-9947.yaml](rules/options-pilot-9947.yaml) | Conservative pilot — $2-wide spreads, SPY/IWM, account 9947 |
| [rules/options-monthly-income.yaml](rules/options-monthly-income.yaml) | “Selling Puts for Monthly Income” — put credit spreads, ~30 DTE, PDF-aligned prompts |

Run **one live agent per account** unless you intend overlapping logic. Each rules file gets its own `agent-state.json`, `agent.pid`, and `agent.log` in `rules/`.

### Quick start (options agent)

```bash
# 1. Enable options in safety.json
schwab safety show --json
# Set allow_option_orders and allow_complex_orders to true (see safety.json.example)

# 2. Optional: LLM + Telegram in .env
# OPENROUTER_API_KEY=sk-or-...
# TELEGRAM_BOT_TOKEN=...
# TELEGRAM_CHAT_ID=...

# 3. Validate rules
schwab agent validate rules/options-pilot-9947.yaml --json

# 4. Dry-run one tick (no orders)
schwab agent run rules/options-pilot-9947.yaml --dry-run --once --json

# 5. Foreground live loop
schwab agent run rules/options-pilot-9947.yaml --trust --yes --json

# 6. Background daemon (survives terminal close)
schwab agent run rules/options-pilot-9947.yaml --background --trust --yes --json
schwab agent stop rules/options-pilot-9947.yaml --json
```

### Agent commands

```bash
schwab agent schema --json                              # JSON Schema for rules.yaml
schwab agent validate rules/<file>.yaml --json
schwab agent status --rules-file rules/<file>.yaml --json
schwab agent run rules/<file>.yaml --dry-run --once --json
schwab agent run rules/<file>.yaml --trust --yes        # foreground
schwab agent run rules/<file>.yaml --background --trust --yes --json
schwab agent stop rules/<file>.yaml --json
```

### Monitoring a background agent

| Method | Command / location |
|--------|------------------|
| Log tail | `tail -f rules/agent.log` |
| State snapshot | `schwab agent status --rules-file rules/<file>.yaml --json` |
| Raw state | `rules/agent-state.json` |
| Process | `cat rules/agent.pid` then `ps -p <pid>` |
| Telegram | Entries, exits, LLM alerts when `notify.telegram.enabled: true` |

### State persistence (resume after restart)

The agent **continues where it left off**. Each tick saves `rules/agent-state.json`:

- `open_positions` — entry credit, underlying, expiry, strategy
- `trades_today` / `trades_day` — resets at midnight
- `tick_count`, `last_llm_review_tick`, `llm_review_count`
- `last_actions` — recent entries, exits, LLM reviews

On startup the agent loads this file and **reconciles** with live Schwab option positions. Stopping and restarting the same `agent run` command is safe.

### Rules file structure

```yaml
version: 1
agent_id: my-strategy

accounts:           # Schwab hash values from `schwab accounts numbers`
schedule:           # tick_interval_seconds, market_hours_only
strategies:         # vertical, iron_condor toggles
watchlist:          # underlyings to scan
entry_rules:        # DTE window, delta, min credit, max width
exit_rules:         # profit_target_pct, stop_loss_pct, dte_close
risk:               # portfolio caps, max trades/day, allowed symbols
execution:          # limit orders, wait_for_fill
llm:                # models, prompts, veto_entries
notify:             # telegram settings
```

### Entry rules (v1 — vertical put credit spreads)

The engine scans the watchlist each tick and builds **put credit verticals** when:

- DTE is within `dte_min`–`dte_max`
- Short strike delta is within `short_delta_min`–`short_delta_max` (proxy for % OTM)
- Estimated credit ≥ `min_credit`
- Long strike is `max_width` below short strike
- Position and risk caps are not exceeded

Tune per strategy. Example from the monthly-income rules (~5% OTM, ~30 DTE):

```yaml
entry_rules:
  vertical:
    type: put_credit
    dte_min: 28
    dte_max: 38
    min_credit: 0.35
    max_width: 5
    short_delta_min: 0.10
    short_delta_max: 0.22
```

### Exit rules (automatic — every tick)

| Rule | Typical value | Behavior |
|------|---------------|----------|
| `profit_target_pct` | `50` | Close when ≥50% of entry credit captured |
| `stop_loss_pct` | `200` | Close when debit to close ≥ 2× entry credit |
| `dte_close` | `21` | Close when ≤21 DTE (gamma management) |

Marks come from live option chain quotes plus `entry_credit` from state or Schwab leg averages.

### LLM advisor (OpenRouter, two-model)

When `llm.enabled: true`:

| Phase | When it runs | Model key | Default model |
|-------|--------------|-----------|---------------|
| **Selection** | Rules produced candidate entries | `selection_model` | `anthropic/claude-3.5-sonnet` |
| **Monitor** | Open positions, every `review_every_ticks` | `monitor_model` | `google/gemini-2.5-flash` |
| **Web** | Every Nth selection review | `web_model` | `perplexity/sonar` |

**Skipped when flat** — no open positions and no candidate entries (saves cost).

| Flag | Default | Effect |
|------|---------|--------|
| `veto_entries` | `true` | Block live entry when LLM returns `defer` / `skip` |
| `allow_llm_exits` | `false` | LLM can trigger discretionary closes (off by default; rules handle exits) |

#### Configurable prompts (`llm.prompts`)

Each rules file can define strategy-specific instructions — use separate YAML files for conservative vs aggressive plans:

```yaml
llm:
  prompts:
    selection: |           # system: role + entry judgment
    selection_web: |        # optional override when web_model runs
    selection_context: |    # user message: strategy thesis, account notes
    monitor: |              # system: open-position review
    monitor_context: |      # user message: monitoring priorities
```

Omit any field to use the built-in default. The LLM always receives a **Context JSON** blob with open positions, candidate entries, exit rules, watchlist, and risk caps.

### Telegram notifications

```yaml
notify:
  telegram:
    enabled: true
    notify_on_actions: true   # entries, exits, LLM alerts
    notify_every_tick: false  # set true for verbose tick summaries
```

Get your chat ID: message your bot, then `curl "https://api.telegram.org/bot<TOKEN>/getUpdates"` and read `chat.id`.

### Monthly income strategy (PDF → rules)

[rules/options-monthly-income.yaml](rules/options-monthly-income.yaml) encodes **“Selling Puts for Monthly Income”** as **put credit spreads** (the PDF’s recommended risk-reduced approach, not naked puts):

| PDF concept | Rules encoding |
|-------------|----------------|
| Sell puts for premium | `put_credit` vertical |
| Defined risk (spread vs naked) | `max_width`, LLM forbids naked puts |
| ~30 DTE monthly cycle | `dte_min: 28`, `dte_max: 38` |
| Strike ~5% OTM | `short_delta` 0.10–0.22 (until `% OTM` rule exists) |
| Close early at ~50% profit | `profit_target_pct: 50` |
| Prefer elevated IV / avoid post-rally entries | `llm.prompts` + `web_model` |

**Not yet automated:** naked cash-secured puts, IV/VIX filters in rules engine, auto-roll at expiry, assignment/stock handling. See v1 limitations below.

### Manual options commands

```bash
schwab options chain --symbol SPY --json
schwab options positions --account-number <hash> --json
schwab options validate --strategy vertical --params '<json>' --json
schwab options preview --account-number <hash> --strategy vertical --params '<json>' --json
schwab options open --account-number <hash> --strategy vertical --params '<json>' --trust --yes --json
schwab options close --account-number <hash> --position-id "SPY|2026-07-24" --trust --yes --json
```

### v1 strategies and limitations

**Supported:**

- `vertical` — put/call credit or debit spreads
- `iron_condor` — four-leg defined-risk condor (optional in rules)

**Deferred to v2:**

- Cash-secured puts / naked short puts
- Covered calls, collars
- Rules-engine IV rank / VIX gates
- Strike selection by `% OTM` (engine uses delta today)
- Automatic roll at expiration

Both vertical spreads and iron condors are IRA-safe (defined risk).

## Agent discovery

```bash
schwab --help --json
schwab capabilities --json
schwab env schema --json
schwab instructions --json   # includes safety rules + plan workflow
```

Output envelope fields: `success`, `command`, `inputs`, `data`, `warnings`, `errors`, `next_actions`, `timestamp`

## Project structure

```
schwab-api-cli/
├── crates/
│   ├── schwab-api/          # HTTP client, OAuth, Trader API endpoints
│   ├── schwab-market-data/  # Market Data API client (quotes, history, instruments)
│   └── schwab-cli/          # CLI binary (`schwab`)
├── docs/
│   └── OPTIONS_RULES.md     # Options agent reference
├── plans/                   # Example trade plans + TRADE_PLAN.md
├── rules/                   # Options agent rules + runtime state
│   ├── options-rules.example.yaml
│   ├── options-pilot-9947.yaml
│   ├── options-monthly-income.yaml
│   ├── agent-state.json     # written at runtime
│   ├── agent.pid            # background daemon PID
│   └── agent.log            # background daemon log
├── safety.json.example
├── .env.example
└── README.md
```

## Development

```bash
cargo check --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo build --release -p schwab-cli
```

## Security

- **Do not commit** `.env`, `tokens.json`, `safety.json` with personal limits, or API keys
- **Do not commit** account hash values in public plans — use placeholders
- Rotate Schwab app credentials if ever exposed
- Use `--dry-run` and `plan validate` before live trades
- Enable `--trust` only when you explicitly want autonomous agent execution

## License

MIT — see [LICENSE](LICENSE).

## Disclaimer

This software is not affiliated with or endorsed by Charles Schwab & Co., Inc. Use at your own risk. Trading involves substantial risk of loss. The authors are not responsible for financial losses from use of this tool.
