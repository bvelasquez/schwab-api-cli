use serde::{Deserialize, Serialize};

/// GET /accounts and GET /accounts/{accountNumber} wrapper object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub securities_account: Option<SecuritiesAccount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecuritiesAccount {
    pub account_number: Option<String>,
    pub round_trips: Option<i64>,
    pub is_day_trader: Option<bool>,
    pub is_closing_only_restricted: Option<bool>,
    pub pfcb_flag: Option<bool>,
    pub positions: Option<Vec<Position>>,
    /// Balance shapes vary by account type (cash, margin, IRA); keep as JSON.
    pub initial_balances: Option<serde_json::Value>,
    pub current_balances: Option<serde_json::Value>,
    pub projected_balances: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    pub short_quantity: Option<f64>,
    pub average_price: Option<f64>,
    pub current_day_profit_loss: Option<f64>,
    pub current_day_profit_loss_percentage: Option<f64>,
    pub long_quantity: Option<f64>,
    pub settled_long_quantity: Option<f64>,
    pub settled_short_quantity: Option<f64>,
    pub aged_quantity: Option<f64>,
    pub instrument: Option<AccountsInstrument>,
    pub market_value: Option<f64>,
    pub maintenance_requirement: Option<f64>,
    pub average_long_price: Option<f64>,
    pub average_short_price: Option<f64>,
    pub tax_lot_average_long_price: Option<f64>,
    pub tax_lot_average_short_price: Option<f64>,
    pub long_open_profit_loss: Option<f64>,
    pub short_open_profit_loss: Option<f64>,
    pub previous_session_long_quantity: Option<f64>,
    pub previous_session_short_quantity: Option<f64>,
    pub current_day_cost: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsInstrument {
    pub cusip: Option<String>,
    pub symbol: Option<String>,
    pub description: Option<String>,
    pub instrument_id: Option<i64>,
    pub net_change: Option<f64>,
    pub r#type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarginBalances {
    pub available_funds: Option<f64>,
    pub available_funds_non_marginable_trade: Option<f64>,
    pub buying_power: Option<f64>,
    pub buying_power_non_marginable_trade: Option<f64>,
    pub day_trading_buying_power: Option<f64>,
    pub day_trading_buying_power_call: Option<f64>,
    pub equity: Option<f64>,
    pub equity_percentage: Option<f64>,
    pub long_margin_value: Option<f64>,
    pub maintenance_call: Option<f64>,
    pub maintenance_requirement: Option<f64>,
    pub margin_balance: Option<f64>,
    pub reg_t_call: Option<f64>,
    pub short_balance: Option<f64>,
    pub short_margin_value: Option<f64>,
    pub sma: Option<f64>,
    pub is_in_call: Option<f64>,
    pub stock_buying_power: Option<f64>,
    pub option_buying_power: Option<f64>,
    // initialBalances-only fields (ignored if absent)
    pub accrued_interest: Option<f64>,
    pub bond_value: Option<f64>,
    pub cash_balance: Option<f64>,
    pub cash_available_for_trading: Option<f64>,
    pub liquidation_value: Option<f64>,
    pub total_cash: Option<f64>,
    pub account_value: Option<f64>,
}
