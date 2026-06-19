# schwab-api-cli

Agent-first Rust CLI for the [Charles Schwab Trader API](https://developer.schwab.com/) (Accounts and Trading Production). Built for **LLM-driven workflows**: discover capabilities via JSON, generate trade plans, validate against hard safety limits, and execute with explicit trust mode.

## Features

- **OAuth 2.0** — browser login, token refresh, secure local storage
- **Read** — accounts, positions, orders, transactions, user preferences
- **Portfolio summary** — aggregated equity and holdings across accounts
- **Trading** — `trade buy` / `trade sell` with preview and safety guardrails
- **Trade plans** — YAML/JSON multi-step rebalances; LLM-authored, CLI-validated
- **Order wait** — poll until limit orders fill before advancing a plan
- **Safety config** — `safety.json` enforces max trade size, symbols, order types (cannot be bypassed)
- **Trust mode** — autonomous agent execution requires `--trust --yes`

## Requirements

- Rust 1.75+ ([rustup](https://rustup.rs/))
- Schwab Developer Portal app with **Trader API – Individual** (Production)
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

**Never commit `.env` or tokens.** They are listed in `.gitignore`.

### Schwab Developer Portal setup

1. Create an app at [developer.schwab.com](https://developer.schwab.com/)
2. Enable **Trader API – Individual** (Production)
3. Set callback URL to `https://127.0.0.1:8182` (HTTPS required)
4. Copy App Key and Secret into `.env`

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
│   ├── schwab-api/     # HTTP client, OAuth, API endpoints
│   └── schwab-cli/     # CLI binary (`schwab`)
├── plans/              # Example trade plans + TRADE_PLAN.md
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
