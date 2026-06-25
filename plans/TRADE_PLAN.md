# Trade plan format (for LLMs and humans)

This document describes how to author files consumed by `schwab plan validate` and `schwab plan run`.

**Full LLM authoring guide (plans + options rules):** [docs/LLM_SCHEMA_REFERENCE.md](../docs/LLM_SCHEMA_REFERENCE.md)

## Commands

| Command | Purpose |
|---------|---------|
| `schwab plan schema --json` | JSON Schema |
| `schwab plan prompt --json` | Copy-paste LLM system prompt + template |
| `schwab plan validate <file>` | Structure + safety checks |
| `schwab plan show <file>` | Pretty-print parsed plan |
| `schwab plan run <file> --dry-run` | Simulate all steps |
| `schwab plan run <file> --trust --yes` | Execute live |

## Required fields

| Field | Description |
|-------|-------------|
| `version` | Must be `1` |
| `plan_id` | Stable id, lowercase slug (e.g. `9947-sgov-jpst-2026-06-19`) |
| `title` | Short human title |
| `account_hash` | **hashValue** from `schwab accounts numbers --json` |
| `created_at` | ISO-8601 UTC timestamp |
| `steps` | Non-empty array of trade steps |

## Step fields

| Field | Required | Description |
|-------|----------|-------------|
| `id` | yes | Unique step id (e.g. `step-01-sell-sgov`) |
| `side` | yes | `buy` or `sell` |
| `symbol` | yes | Ticker (equity) |
| `quantity` | yes | Share count (> 0) |
| `order_type` | no | `limit` (default) or `market` |
| `limit_price` | if limit | Limit price per share |
| `duration` | no | `day` (default), `gtc`, `fok` |
| `session` | no | `normal` (default) |
| `note` | no | Rationale for this batch |

## Safety rules when generating steps

Read `schwab safety show --json` and ensure **each step** satisfies:

- `quantity <= max_shares_per_order`
- `quantity * limit_price <= max_trade_value_usd` (for limits)
- `quantity * limit_price <= max_trade_pct_of_equity * account_equity`

If a rebalance is larger than one step allows, **split into multiple steps**.

## Execution options

```yaml
execution:
  stop_on_error: true          # halt plan on first failed order
  pause_seconds_between_steps: 2  # optional throttle between live steps
```

## Execution options

```yaml
execution:
  stop_on_error: true
  pause_seconds_between_steps: 3
  wait_for_fill: true              # plan-level: wait for FILLED before next step
  default_wait_until: filled       # alternative to wait_for_fill
  fill_timeout_seconds: 3600
  poll_interval_seconds: 10
  proceed_on_partial_fill: false
```

Per-step override:

```yaml
  - id: step-01-sell-sgov
    side: sell
    symbol: SGOV
    quantity: 14
    order_type: limit
    limit_price: 100.55
    wait_until: filled             # accepted | filled | terminal
```

After each step is submitted, `schwab plan run` polls `orders get` until the wait condition is met (or times out). You can also poll manually:

```bash
schwab orders wait <account_hash> <order_id> --until filled --timeout-seconds 3600 --json
```

## Example (minimal)

```yaml
version: 1
plan_id: example-2026-06-19
title: Example sell/buy pair
account_hash: "<hash-from-accounts-numbers>"
created_at: "2026-06-19T12:00:00Z"
steps:
  - id: step-01-sell
    side: sell
    symbol: SGOV
    quantity: 14
    order_type: limit
    limit_price: 100.55
  - id: step-02-buy
    side: buy
    symbol: JPST
    quantity: 28
    order_type: limit
    limit_price: 50.50
```

## LLM system prompt (summary)

You are preparing a trade plan for the `schwab` CLI. Always:

1. Ground the plan in live `portfolio summary` and `safety show` output.
2. Use account **hash**, not plain account number.
3. Prefer limit orders; state assumptions for limit prices.
4. Never exceed per-step safety limits — batch instead.
5. Include `rationale` explaining the thesis and risks.
6. Output valid YAML unless the user requests JSON.

For the full prompt: `schwab plan prompt --json`.
