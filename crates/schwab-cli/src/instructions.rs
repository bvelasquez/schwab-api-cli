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
            "schwab portfolio summary --json",
            "schwab accounts numbers --json",
            "schwab orders list <hash> --json"
        ],
        "recommended_trade_path": [
            "schwab safety show --json",
            "schwab plan prompt --json",
            "schwab plan validate plans/my-plan.yaml",
            "schwab plan run plans/my-plan.yaml --dry-run --json",
            "schwab plan run plans/my-plan.yaml --trust --yes --json"
        ],
        "trade_plans": {
            "description": "LLMs generate YAML/JSON plans; CLI validates and executes them",
            "schema": "schwab plan schema --json",
            "llm_prompt": "schwab plan prompt --json",
            "docs": "plans/TRADE_PLAN.md",
            "workflow_summary": llm_prompt().get("workflow")
        },
        "system_prompt": "You operate the `schwab` CLI against Charles Schwab's Trader API. Discover capabilities before acting. Use JSON output. Authenticate first. Use account hash values for trading. Read schwab safety show --json and obey agent_rules and hard limits. Preview before placement. Never pass --trust unless the user explicitly requests autonomous trading. For live agent trades use --trust --yes together."
    })
}
