//! Machine-readable workflow schema and the prompt an agent should follow.

use serde_json::{json, Value};

pub fn workflow_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "agent-trader-swing/workflow",
        "title": "Continuous workflow",
        "description": "A cron-triggered list of read, compute, check, and order steps. A continuous workflow is allowed. This is not a graph language.",
        "type": "object",
        "required": ["version", "account", "workflows"],
        "properties": {
            "version": { "const": 1 },
            "broker": { "enum": ["schwab", "public"] },
            "account": { "type": "string", "description": "public:ACCOUNT_ID or schwab:HASH" },
            "account_hash": { "type": "string" },
            "data_source": { "enum": ["schwab", "public"] },
            "workflows": {
                "type": "array",
                "minItems": 1,
                "items": { "$ref": "#/$defs/workflow" }
            }
        },
        "$defs": {
            "workflow": {
                "type": "object",
                "required": ["id", "schedule", "steps"],
                "properties": {
                    "id": { "type": "string" },
                    "schedule": { "$ref": "#/$defs/schedule" },
                    "steps": {
                        "type": "array",
                        "minItems": 1,
                        "items": { "$ref": "#/$defs/step" }
                    }
                }
            },
            "schedule": {
                "type": "object",
                "required": ["cron", "timezone"],
                "properties": {
                    "cron": { "type": "string", "description": "5 fields: minute hour day-of-month month day-of-week. 0 is Sunday." },
                    "timezone": { "type": "string", "examples": ["America/New_York"] },
                    "active_between": {
                        "type": "array",
                        "minItems": 2,
                        "maxItems": 2,
                        "items": { "type": "string" },
                        "description": "Inclusive local HH:MM window. Fires outside it are dropped."
                    }
                }
            },
            "step": {
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": { "type": "string" },
                    "fetch": { "enum": ["quote", "bars", "session", "portfolio", "open_orders", "expirations", "chain", "greeks", "strategy_quote", "sma", "ema", "rsi", "atr", "spread_pct", "change_pct", "position"] },
                    "symbol": { "type": "string" },
                    "period": { "description": "Indicator length, or day|week|month|year for bars. EMA and SMA require it. RSI and ATR default to 14. Bars default to year." },
                    "expiration": { "type": "string" },
                    "quote": { "type": "string", "description": "Step id of a quote, used by spread_pct." },
                    "legs": { "type": "array" },
                    "when": { "$ref": "#/$defs/when" },
                    "order": { "$ref": "#/$defs/order" }
                }
            },
            "when": {
                "type": "object",
                "required": ["all"],
                "properties": {
                    "all": {
                        "type": "array",
                        "items": {
                            "description": "Exactly one of: session_open, buying_power_at_least, min_days_since_fill, spread_pct_at_most, position_qty, gt, gte, lt, lte, eq, between."
                        }
                    }
                }
            },
            "order": {
                "type": "object",
                "properties": {
                    "kind": { "enum": ["equity", "exit_bracket", "cancel", "wait", "spread"] },
                    "side": { "enum": ["buy", "sell"] },
                    "symbol": { "type": "string" },
                    "notional_usd": { "type": "number" },
                    "quantity": {},
                    "order_type": { "enum": ["market", "limit"] },
                    "limit_price": {},
                    "tif": { "enum": ["day", "gtc", "fok"] },
                    "session": { "enum": ["core", "extended", "twenty_four_hours"] },
                    "bracket": { "type": "object" },
                    "strategy": { "enum": ["vertical", "iron_condor", "custom"] },
                    "contracts": { "type": "number" },
                    "net_credit": { "type": "number" },
                    "net_debit": { "type": "number" },
                    "legs": { "type": "array" },
                    "order_id": { "type": "string" },
                    "until": { "enum": ["filled", "terminal", "accepted"] },
                    "timeout_seconds": { "type": "integer" }
                }
            }
        },
        "x-bound-fields": {
            "quote": ["bid", "ask", "last", "bid_size", "ask_size", "volume"],
            "bars": ["count", "last_close", "prev_close"],
            "session": ["open"],
            "portfolio": ["cash_buying_power", "options_buying_power", "equity", "positions"],
            "open_orders": ["count", "ids", "first_id"],
            "expirations": ["count", "first"],
            "chain": ["underlying", "expiration", "contract_count"],
            "greeks": ["delta", "gamma", "theta", "vega", "iv"],
            "strategy_quote": ["bid", "ask", "last"],
            "sma": "number",
            "ema": "number. Shorthand for a year of daily bars plus the EMA.",
            "rsi": "number. Wilder, default period 14.",
            "atr": "number. SMA of true range, default period 14.",
            "spread_pct": "number. (ask-bid)/mid*100",
            "change_pct": "number. Last daily close versus the previous close.",
            "position": ["symbol", "quantity", "average_price", "unrealized_pct"]
        },
        "x-capabilities": "Run `agent-trader capabilities` before a notional order or a bracket. Notional requires fractional. A bracket on the entry requires native_bracket. Schwab uses a later exit_bracket step. strategy_quote and max_legs come from the same command."
    })
}

pub fn workflow_prompt() -> Value {
    json!({
        "capability": "A continuous workflow is allowed. Write rules/workflows/<name>.yaml with version 1, an account, and one or more workflows. Each workflow is a cron plus a straight list of steps. The workflow agent already paper-runs that directory, writes rules/agent-trader-workflow-<stem>.json, and the paper dashboard Workflows tab shows the last decision. Writing the file is the setup. Do not start a process, a timer, or a service. Do not invent a graph, a chat compiler, or a step that is not in the schema.",
        "commands": {
            "schema": "agent-trader-swing workflow schema --json",
            "prompt": "agent-trader-swing workflow prompt --json",
            "validate": "agent-trader-swing workflow validate --rules rules/workflows/<file>",
            "dry_run": "agent-trader-swing run --rules <file> --once --dry-run",
            "live": "agent-trader-swing run --rules <file> --trust --yes"
        },
        "before_live": [
            "Replace account with a real public:ID or schwab:HASH.",
            "Run `agent-trader capabilities` and use notional only when fractional is true.",
            "Use an entry bracket only when native_bracket is true. Otherwise add an exit_bracket step.",
            "Run `agent-trader guard <account>` and `agent-trader canary <account>`.",
            "Validate, then --dry-run. safety.json still applies with --trust --yes.",
            "Public orders need public_license_ack: true in the agent-trader safety.json."
        ],
        "checks": [
            "gt, gte, lt, lte, eq: [left, right]. Each side is a bound path (quote.last, ema200) or a number.",
            "between: [probe, low, high]",
            "session_open: true",
            "buying_power_at_least: dollars, compared with cash_buying_power",
            "position_qty: {symbol, eq|gt|gte|lt|lte: number}",
            "min_days_since_fill: days since this workflow last filled that order symbol. No earlier fill passes.",
            "spread_pct_at_most: number. Requires an earlier quote or spread_pct step."
        ],
        "orders": [
            "equity buy or sell, notional_usd or quantity, market or limit. limit_price may be a number or a bound path.",
            "bracket.take_profit_pct and stop_pct are percents from the limit or the bound quote. Schwab cannot attach this; use exit_bracket.",
            "exit_bracket for shares already held.",
            "cancel an id bound from open_orders (working.first_id).",
            "wait polls get_order until filled, terminal, or accepted.",
            "spread submits the legs in the file. It does not scan for a new spread. safety.json blocks it while allow_option_orders or allow_complex_orders is false."
        ],
        "not_a_step": "Earnings, fundamentals, news, regime, and an LLM judgment are not workflow steps. The options scanner stays `agent-trader options tick`.",
        "fail_closed": "A short indicator history binds nothing and the order is skipped. A name that was not bound by an earlier step fails validate.",
        "template": MSFT_TEMPLATE
    })
}

const MSFT_TEMPLATE: &str = r#"version: 1
broker: public
account: "public:ACCOUNT_ID"
workflows:
  - id: msft-dip
    schedule:
      cron: "30 9-15 * * 1-5"
      timezone: America/New_York
      active_between: ["09:30", "16:00"]
    steps:
      - id: quote
        fetch: quote
        symbol: MSFT
      - id: ema20
        fetch: ema
        symbol: MSFT
        period: 20
      - id: ema200
        fetch: ema
        symbol: MSFT
        period: 200
      - id: buy
        when:
          all:
            - session_open: true
            - gt: ["quote.last", "ema200"]
            - lte: ["quote.last", "ema20"]
            - min_days_since_fill: 7
            - buying_power_at_least: 250
        order:
          side: buy
          symbol: MSFT
          notional_usd: 250
          order_type: market
"#;
