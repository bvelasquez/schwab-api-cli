use serde_json::{json, Value};

pub fn env_schema_json() -> Value {
    json!({
        "precedence": ["CLI flags", "environment variables", "defaults"],
        "dotenv": {
            "enabled": true,
            "behavior": "Loads nearest .env walking up from current working directory"
        },
        "variables": [
            {
                "name": "SCHWAB_APP_KEY",
                "aliases": ["SCHWAB_CLIENT_ID"],
                "required": true,
                "secret": true,
                "description": "Schwab Developer Portal app key (OAuth client id)"
            },
            {
                "name": "SCHWAB_APP_SECRET",
                "aliases": ["SCHWAB_CLIENT_SECRET"],
                "required": true,
                "secret": true,
                "description": "Schwab Developer Portal app secret"
            },
            {
                "name": "SCHWAB_REDIRECT_URI",
                "aliases": [],
                "required": false,
                "default": "https://127.0.0.1:8182",
                "description": "OAuth redirect URI registered with your Schwab app"
            },
            {
                "name": "SCHWAB_TOKEN_DIR",
                "aliases": [],
                "required": false,
                "description": "Override directory for tokens.json"
            },
            {
                "name": "SCHWAB_SAFETY_CONFIG",
                "aliases": [],
                "required": false,
                "description": "Override path to safety.json trading limits (default: platform config dir)"
            },
            {
                "name": "SCHWAB_MODE",
                "aliases": [],
                "required": false,
                "default": "agent",
                "enum": ["agent", "human"],
                "description": "CLI operating mode"
            },
            {
                "name": "SCHWAB_OUTPUT",
                "aliases": [],
                "required": false,
                "default": "pretty",
                "enum": ["pretty", "json", "md"],
                "description": "Default output format"
            },
            {
                "name": "NO_COLOR",
                "aliases": [],
                "required": false,
                "description": "Disable ANSI colors in pretty output"
            },
            {
                "name": "OPENROUTER_API_KEY",
                "aliases": [],
                "required": false,
                "secret": true,
                "description": "OpenRouter API key for LLM-powered agent reviews (when llm.enabled in rules.yaml)"
            },
            {
                "name": "TELEGRAM_BOT_TOKEN",
                "aliases": [],
                "required": false,
                "secret": true,
                "description": "Telegram bot token from @BotFather (when notify.telegram.enabled in rules.yaml)"
            },
            {
                "name": "TELEGRAM_CHAT_ID",
                "aliases": [],
                "required": false,
                "secret": true,
                "description": "Telegram chat ID for agent notifications"
            }
        ]
    })
}
