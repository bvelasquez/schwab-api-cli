use crate::plan::llm_prompt;
use crate::safety_config::{config_path, SafetyConfig};
use serde_json::{json, Value};

pub fn instructions_json(safety: &SafetyConfig) -> Value {
    json!({
        "role": "Schwab Trader API operator",
        "product": "Trader API - Individual (Accounts and Trading Production v1.0.0)",
        "base_url": schwab_api::TRADER_BASE_URL,
        "discovery_sequence": [
            "schwab --help --json",
            "schwab capabilities --json",
            "schwab env schema --json",
            "schwab instructions --json",
            "schwab safety show --json"
        ],
        "operating_mode": {
            "default": "agent",
            "flags": ["--mode agent", "--mode human"],
            "note": "Always prefer --json or --output json for automation"
        },
        "auth_flow": [
            "Ensure SCHWAB_APP_KEY and SCHWAB_APP_SECRET are set (see env schema)",
            "Run `schwab auth login` once; tokens persist on disk",
            "Use `schwab auth status --json` before API calls",
            "Access tokens expire ~30 minutes; refresh via `schwab auth refresh` or automatic refresh on API call"
        ],
        "account_ids": {
            "rule": "Use encrypted hashValue from `schwab accounts numbers --json` as {accountNumber} in trading endpoints",
            "never": "Do not confuse plain accountNumber with hashValue"
        },
        "output_contract": {
            "envelope_fields": ["success", "command", "inputs", "data", "warnings", "errors", "next_actions", "timestamp"],
            "formats": ["pretty", "json", "md"]
        },
        "trading_safety": {
            "config_path": config_path(),
            "config_command": "schwab safety show --json",
            "hard_limits": safety.limits,
            "require_preview_before_place": safety.require_preview_before_place,
            "agent_rules": safety.agent_rules,
            "safe_mode_default": true,
            "trust_mode": {
                "flag": "--trust",
                "description": "Required with --yes for autonomous agent trading (non-interactive). Human mode uses interactive confirmation.",
                "note": "CLI enforces safety.json limits even in trust mode"
            },
            "autonomous_trade_flags": ["--trust", "--yes"],
            "dry_run_flag": "--dry-run"
        },
        "mutation_safety": {
            "auth_policy": "auth refresh/logout require --yes in non-interactive mode",
            "trading_policy": "orders place/cancel/replace and trade buy/sell require --trust --yes in agent mode, or interactive confirmation in human mode",
            "preview_first": "Use `schwab orders preview --json` or rely on built-in preview before place"
        },
        "recommended_read_path": [
            "schwab market info SGOV --json",
            "schwab market hours --markets equity --json",
            "schwab portfolio summary --json",
            "schwab portfolio buying-power --account-number <hash> --json",
            "schwab accounts numbers --json",
            "schwab orders list <hash> --json"
        ],
        "market_data": {
            "base_url": schwab_api::MARKET_DATA_BASE_URL,
            "requires_portal_product": "Market Data Production",
            "primary_research_command": "schwab market info <SYMBOL> --json",
            "commands": {
                "info": "schwab market info SGOV --json (quote + fundamentals + history + research hints)",
                "info_multi": "schwab market info SGOV,JPST,AAPL --json",
                "quotes": "schwab market quotes --symbols SGOV,JPST --fields quote,fundamental --json",
                "quote": "schwab market quote SGOV --fields all --json",
                "history": "schwab market history AAPL --period-type month --period 1 --frequency-type daily --json",
                "company_info": "schwab market instrument --symbol AAPL --projection fundamental --json",
                "hours": "schwab market hours --markets equity --json"
            },
            "agent_workflow": [
                "schwab market info <symbol> --json for Schwab-side facts",
                "Web search using data.researchHints.recommendedWebQueries for narrative, holdings, news",
                "schwab portfolio summary --json for account context",
                "schwab plan prompt --json → validate → dry-run → run with --trust --yes"
            ],
            "quote_fields": ["all", "quote", "fundamental", "reference", "extended", "regular"],
            "instrument_projections": ["symbol-search", "fundamental", "search", "desc-search"]
        },
        "recommended_trade_path": [
            "schwab portfolio buying-power --account-number <hash> --json",
            "schwab market quotes --symbols <SYMBOL> --fields quote --json",
            "schwab orders schema --json",
            "schwab safety show --json",
            "schwab orders validate --order '<json>' --json",
            "schwab orders preview --account-number <hash> --order '<json>' --json",
            "schwab orders place --account-number <hash> --order '<json>' --trust --yes --json"
        ],
        "buying_power": {
            "rule": "Before any BUY, check cashAvailableForTrading via `schwab portfolio buying-power --account-number <hash> --json`",
            "cli_enforcement": "trade buy and orders place reject buys when estimated cost exceeds available cash",
            "funding_sequence": [
                "If buying power is insufficient, sell source holdings first",
                "Wait for sell fill: `schwab orders wait <hash> <order_id> --until filled --json`",
                "Re-check buying power, then place the buy"
            ]
        },
        "orders": {
            "schema": "schwab orders schema --json",
            "validate": "schwab orders validate --order '<json>' --json",
            "supported_asset_types": ["EQUITY", "OPTION"],
            "complexOrderStrategyType": ["NONE", "VERTICAL", "IRON_CONDOR", "CUSTOM", "..."],
            "conditionalStrategies": ["OCO", "TRIGGER"],
            "safety_flags": {
                "allow_option_orders": "single-leg and spread option legs",
                "allow_complex_orders": "multi-leg spreads (NET_DEBIT/NET_CREDIT)",
                "allow_conditional_orders": "OCO and TRIGGER childOrderStrategies"
            },
            "option_symbology": "UNDERLYING(6) | YYMMDD | C/P | STRIKE(8) — e.g. XYZ   240315C00500000"
        },
        "trade_plans": {
            "description": "LLMs generate YAML/JSON plans; CLI validates and executes them",
            "schema": "schwab plan schema --json",
            "llm_prompt": "schwab plan prompt --json",
            "docs": "plans/TRADE_PLAN.md",
            "workflow_summary": llm_prompt().get("workflow")
        },
        "options_trading": {
            "schema": "schwab options schema --json",
            "chain": "schwab options chain --symbol SPY --json",
            "positions": "schwab options positions --account-number <hash> --json",
            "strategies_v1": ["vertical", "iron_condor"],
            "workflow": [
                "schwab options chain --symbol <UNDERLYING> --json",
                "schwab options validate --strategy vertical --params '<json>' --json",
                "schwab options preview --account-number <hash> --strategy vertical --params '<json>' --json",
                "schwab options open --account-number <hash> --strategy vertical --params '<json>' --trust --yes --json"
            ],
            "skill": ".cursor/skills/schwab-options/SKILL.md",
            "docs": "docs/OPTIONS_RULES.md"
        },
        "options_agent": {
            "rules_schema": "schwab agent schema --json",
            "validate": "schwab agent validate rules.yaml --json",
            "dry_run_tick": "schwab agent run rules.yaml --dry-run --once --json",
            "live_daemon": "schwab agent run rules.yaml --trust --yes",
            "status": "schwab agent status --rules-file rules.yaml --json",
            "example_rules": "rules/options-rules.example.yaml",
            "note": "Agent auto-executes vertical and iron condor entries/exits within safety.json and rules.yaml risk limits"
        },
        "system_prompt": "You operate the `schwab` CLI against Charles Schwab's Trader API. Discover capabilities before acting. Use JSON output. Authenticate first. Use account hash values for trading. Read schwab safety show --json and obey agent_rules and hard limits. Preview before placement. Never pass --trust unless the user explicitly requests autonomous trading. For live agent trades use --trust --yes together."
    })
}
