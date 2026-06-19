use serde_json::{json, Value};

#[derive(Debug, Clone, serde::Serialize)]
pub struct CommandSpec {
    pub path: &'static str,
    pub description: &'static str,
    pub http: Option<&'static str>,
    pub mutation: bool,
    pub requires_auth: bool,
}

pub fn all_commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            path: "capabilities",
            description: "Machine-readable command catalog",
            http: None,
            mutation: false,
            requires_auth: false,
        },
        CommandSpec {
            path: "env schema",
            description: "Environment variable schema",
            http: None,
            mutation: false,
            requires_auth: false,
        },
        CommandSpec {
            path: "instructions",
            description: "Agent system prompt and tool-use guidance",
            http: None,
            mutation: false,
            requires_auth: false,
        },
        CommandSpec {
            path: "auth login",
            description: "OAuth authorization code flow",
            http: None,
            mutation: true,
            requires_auth: false,
        },
        CommandSpec {
            path: "auth status",
            description: "Inspect stored OAuth tokens",
            http: None,
            mutation: false,
            requires_auth: false,
        },
        CommandSpec {
            path: "auth refresh",
            description: "Refresh OAuth access token",
            http: None,
            mutation: true,
            requires_auth: false,
        },
        CommandSpec {
            path: "auth logout",
            description: "Delete stored OAuth tokens",
            http: None,
            mutation: true,
            requires_auth: false,
        },
        CommandSpec {
            path: "accounts numbers",
            description: "List account numbers and encrypted hash values",
            http: Some("GET /accounts/accountNumbers"),
            mutation: false,
            requires_auth: true,
        },
        CommandSpec {
            path: "accounts list",
            description: "List linked accounts with balances and positions",
            http: Some("GET /accounts"),
            mutation: false,
            requires_auth: true,
        },
        CommandSpec {
            path: "accounts get",
            description: "Get one account by account number hash",
            http: Some("GET /accounts/{accountNumber}"),
            mutation: false,
            requires_auth: true,
        },
        CommandSpec {
            path: "portfolio summary",
            description: "Aggregate portfolio equity and holdings across accounts",
            http: Some("GET /accounts"),
            mutation: false,
            requires_auth: true,
        },
        CommandSpec {
            path: "trade buy",
            description: "Buy equity shares with safety guardrails",
            http: Some("POST /accounts/{accountNumber}/orders"),
            mutation: true,
            requires_auth: true,
        },
        CommandSpec {
            path: "trade sell",
            description: "Sell equity shares with safety guardrails",
            http: Some("POST /accounts/{accountNumber}/orders"),
            mutation: true,
            requires_auth: true,
        },
        CommandSpec {
            path: "safety show",
            description: "Show active safety.json limits and path",
            http: None,
            mutation: false,
            requires_auth: false,
        },
        CommandSpec {
            path: "safety init",
            description: "Create default safety.json in config directory",
            http: None,
            mutation: true,
            requires_auth: false,
        },
        CommandSpec {
            path: "safety path",
            description: "Print safety.json path",
            http: None,
            mutation: false,
            requires_auth: false,
        },
        CommandSpec {
            path: "plan schema",
            description: "JSON Schema for trade plan YAML/JSON files",
            http: None,
            mutation: false,
            requires_auth: false,
        },
        CommandSpec {
            path: "plan prompt",
            description: "LLM workflow and template for generating trade plans",
            http: None,
            mutation: false,
            requires_auth: false,
        },
        CommandSpec {
            path: "plan validate",
            description: "Validate trade plan structure and safety limits",
            http: None,
            mutation: false,
            requires_auth: true,
        },
        CommandSpec {
            path: "plan show",
            description: "Display parsed trade plan",
            http: None,
            mutation: false,
            requires_auth: false,
        },
        CommandSpec {
            path: "plan run",
            description: "Execute trade plan steps sequentially",
            http: Some("POST /accounts/{accountNumber}/orders"),
            mutation: true,
            requires_auth: true,
        },
        CommandSpec {
            path: "orders list",
            description: "List orders for an account",
            http: Some("GET /accounts/{accountNumber}/orders"),
            mutation: false,
            requires_auth: true,
        },
        CommandSpec {
            path: "orders all",
            description: "List orders across all accounts",
            http: Some("GET /orders"),
            mutation: false,
            requires_auth: true,
        },
        CommandSpec {
            path: "orders get",
            description: "Get order by ID",
            http: Some("GET /accounts/{accountNumber}/orders/{orderId}"),
            mutation: false,
            requires_auth: true,
        },
        CommandSpec {
            path: "orders wait",
            description: "Poll order status until filled or timeout",
            http: Some("GET /accounts/{accountNumber}/orders/{orderId}"),
            mutation: false,
            requires_auth: true,
        },
        CommandSpec {
            path: "orders place",
            description: "Place a new order",
            http: Some("POST /accounts/{accountNumber}/orders"),
            mutation: true,
            requires_auth: true,
        },
        CommandSpec {
            path: "orders preview",
            description: "Preview order validation and fees",
            http: Some("POST /accounts/{accountNumber}/previewOrder"),
            mutation: false,
            requires_auth: true,
        },
        CommandSpec {
            path: "orders cancel",
            description: "Cancel an open order",
            http: Some("DELETE /accounts/{accountNumber}/orders/{orderId}"),
            mutation: true,
            requires_auth: true,
        },
        CommandSpec {
            path: "orders replace",
            description: "Replace an existing order",
            http: Some("PUT /accounts/{accountNumber}/orders/{orderId}"),
            mutation: true,
            requires_auth: true,
        },
        CommandSpec {
            path: "transactions list",
            description: "List transactions for an account",
            http: Some("GET /accounts/{accountNumber}/transactions"),
            mutation: false,
            requires_auth: true,
        },
        CommandSpec {
            path: "transactions get",
            description: "Get transaction by ID",
            http: Some("GET /accounts/{accountNumber}/transactions/{transactionId}"),
            mutation: false,
            requires_auth: true,
        },
        CommandSpec {
            path: "user preference",
            description: "User preferences and streamer info",
            http: Some("GET /userPreference"),
            mutation: false,
            requires_auth: true,
        },
    ]
}

pub fn command_tree() -> Value {
    json!([
        { "group": "meta", "commands": ["capabilities", "env schema", "instructions"] },
        { "group": "auth", "commands": ["login", "status", "refresh", "logout"] },
        { "group": "accounts", "commands": ["numbers", "list", "get"] },
        { "group": "portfolio", "commands": ["summary"] },
        { "group": "trade", "commands": ["buy", "sell"] },
        { "group": "safety", "commands": ["show", "init", "path"] },
        { "group": "plan", "commands": ["schema", "prompt", "validate", "show", "run"] },
        { "group": "orders", "commands": ["list", "all", "get", "wait", "place", "preview", "cancel", "replace"] },
        { "group": "transactions", "commands": ["list", "get"] },
        { "group": "user", "commands": ["preference"] }
    ])
}

pub fn capabilities_json() -> Value {
    let commands: Vec<Value> = all_commands()
        .into_iter()
        .map(|c| {
            json!({
                "path": c.path,
                "description": c.description,
                "http": c.http,
                "mutation": c.mutation,
                "requires_auth": c.requires_auth,
            })
        })
        .collect();

    json!({
        "cli": "schwab",
        "api_product": "Trader API - Individual (Accounts and Trading Production)",
        "base_url": schwab_api::TRADER_BASE_URL,
        "default_mode": "agent",
        "output_formats": ["pretty", "json", "md"],
        "mutation_policy": "Auth mutations require --yes in non-interactive mode",
        "trading_policy": "Trading mutations require --trust --yes in agent mode; safety.json hard limits always enforced",
        "global_flags": ["--yes", "--trust", "--dry-run", "--json"],
        "commands": commands,
    })
}
