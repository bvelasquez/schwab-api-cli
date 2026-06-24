use anyhow::{bail, Context, Result};
use schwab_api::models::account::Account;
use serde_json::{json, Value};

use crate::safety_config::{estimate_notional, parse_order, ParsedOrder};

#[derive(Debug, Clone, serde::Serialize)]
pub struct PortfolioSummary {
    pub total_equity: f64,
    pub total_positions: usize,
    pub accounts: Vec<AccountSummary>,
    pub aggregated_holdings: Vec<AggregatedHolding>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AccountSummary {
    pub account_number: Option<String>,
    pub account_number_last4: String,
    pub equity: Option<f64>,
    pub position_count: usize,
    pub positions: Vec<PositionSummary>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PositionSummary {
    pub symbol: String,
    pub description: Option<String>,
    pub quantity: f64,
    pub market_value: f64,
    pub pct_of_account: Option<f64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AggregatedHolding {
    pub symbol: String,
    pub total_quantity: f64,
    pub total_market_value: f64,
    pub pct_of_portfolio: f64,
}

pub fn summarize_accounts(accounts: &[Account]) -> PortfolioSummary {
    let mut account_summaries = Vec::new();
    let mut agg: std::collections::HashMap<String, (f64, f64)> = std::collections::HashMap::new();
    let mut total_equity = 0.0;
    let mut total_positions = 0usize;

    for account in accounts {
        let sa = match &account.securities_account {
            Some(sa) => sa,
            None => continue,
        };

        let equity = extract_equity(sa.current_balances.as_ref());
        if let Some(eq) = equity {
            total_equity += eq;
        }

        let acct_num = sa.account_number.clone().unwrap_or_default();
        let last4 = last4(&acct_num);

        let positions = sa.positions.as_deref().unwrap_or_default();
        total_positions += positions.len();

        let mut pos_summaries = Vec::new();
        for pos in positions {
            let symbol = pos
                .instrument
                .as_ref()
                .and_then(|i| i.symbol.clone())
                .unwrap_or_else(|| "?".into());
            let description = pos
                .instrument
                .as_ref()
                .and_then(|i| i.description.clone());
            let quantity = pos.long_quantity.unwrap_or(0.0) - pos.short_quantity.unwrap_or(0.0);
            let market_value = pos.market_value.unwrap_or(0.0);
            let pct_of_account = equity.filter(|e| *e > 0.0).map(|e| (market_value / e) * 100.0);

            pos_summaries.push(PositionSummary {
                symbol: symbol.clone(),
                description,
                quantity,
                market_value,
                pct_of_account,
            });

            let entry = agg.entry(symbol).or_insert((0.0, 0.0));
            entry.0 += quantity;
            entry.1 += market_value;
        }

        pos_summaries.sort_by(|a, b| {
            b.market_value
                .partial_cmp(&a.market_value)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        account_summaries.push(AccountSummary {
            account_number: sa.account_number.clone(),
            account_number_last4: last4,
            equity,
            position_count: positions.len(),
            positions: pos_summaries,
        });
    }

    account_summaries.sort_by(|a, b| {
        b.equity
            .unwrap_or(0.0)
            .partial_cmp(&a.equity.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut aggregated_holdings: Vec<AggregatedHolding> = agg
        .into_iter()
        .map(|(symbol, (total_quantity, total_market_value))| {
            let pct_of_portfolio = if total_equity > 0.0 {
                (total_market_value / total_equity) * 100.0
            } else {
                0.0
            };
            AggregatedHolding {
                symbol,
                total_quantity,
                total_market_value,
                pct_of_portfolio,
            }
        })
        .collect();

    aggregated_holdings.sort_by(|a, b| {
        b.total_market_value
            .partial_cmp(&a.total_market_value)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    PortfolioSummary {
        total_equity,
        total_positions,
        accounts: account_summaries,
        aggregated_holdings,
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BuyingPower {
    pub cash_available_for_trading: f64,
    pub cash_balance: f64,
    pub option_buying_power: Option<f64>,
    pub liquidation_value: Option<f64>,
}

pub async fn account_equity(api: &schwab_api::TraderApi, account_hash: &str) -> Result<Option<f64>> {
    let buying_power = account_buying_power(api, account_hash).await?;
    Ok(buying_power.liquidation_value)
}

pub async fn account_buying_power(
    api: &schwab_api::TraderApi,
    account_hash: &str,
) -> Result<BuyingPower> {
    let account = api.accounts().get(account_hash, None).await?;
    let sa = account
        .securities_account
        .as_ref()
        .context("Account has no securitiesAccount payload")?;

    Ok(extract_buying_power(
        sa.current_balances.as_ref(),
        sa.projected_balances.as_ref(),
    ))
}

pub fn extract_buying_power(
    current: Option<&Value>,
    projected: Option<&Value>,
) -> BuyingPower {
    // Cash accounts expose cashAvailableForTrading; margin accounts use buyingPower /
    // availableFunds instead. Try each in order so both account types work correctly.
    let cash_available_for_trading = extract_balance_field(current, "cashAvailableForTrading")
        .or_else(|| extract_balance_field(projected, "cashAvailableForTrading"))
        .or_else(|| extract_balance_field(current, "buyingPower"))
        .or_else(|| extract_balance_field(current, "availableFunds"))
        .or_else(|| extract_balance_field(projected, "buyingPower"))
        .or_else(|| extract_balance_field(projected, "availableFunds"))
        .unwrap_or(0.0);
    let option_buying_power = extract_balance_field(current, "optionBuyingPower")
        .or_else(|| extract_balance_field(projected, "optionBuyingPower"));
    let cash_balance = extract_balance_field(current, "cashBalance")
        .or_else(|| extract_balance_field(current, "totalCash"))
        .unwrap_or(0.0);
    let liquidation_value = extract_balance_field(current, "liquidationValue")
        .or_else(|| extract_equity(current));

    let effective_available = option_buying_power
        .unwrap_or(cash_available_for_trading)
        .max(cash_available_for_trading);

    BuyingPower {
        cash_available_for_trading: effective_available,
        cash_balance,
        option_buying_power,
        liquidation_value,
    }
}

pub fn estimate_equity_buy_cost(
    quantity: f64,
    order_type: &str,
    limit_price: Option<f64>,
    market_ask: Option<f64>,
) -> Result<f64> {
    let order_type = order_type.to_uppercase();
    match order_type.as_str() {
        "LIMIT" | "STOP_LIMIT" | "LIMIT_ON_CLOSE" => {
            let price = limit_price.context("limit price required to estimate buy cost")?;
            Ok(quantity * price)
        }
        "MARKET" => {
            let ask = market_ask.context(
                "market ask price required to estimate buy cost for MARKET orders",
            )?;
            Ok(quantity * ask)
        }
        other => bail!("Cannot estimate buy cost for order type `{other}`"),
    }
}

pub fn order_requires_buying_power(parsed: &ParsedOrder) -> bool {
    if parsed.legs.iter().any(|leg| leg.asset_type == "EQUITY" && leg.instruction == "BUY") {
        return true;
    }
    if parsed.legs.iter().any(|leg| leg.asset_type == "OPTION") {
        return matches!(
            parsed.order_type.as_str(),
            "NET_DEBIT" | "LIMIT" | "MARKET"
        );
    }
    false
}

pub fn ensure_sufficient_buying_power(
    buying_power: &BuyingPower,
    estimated_cost: f64,
) -> Result<()> {
    let available = buying_power.cash_available_for_trading;
    if estimated_cost > available {
        let shortfall = estimated_cost - available;
        bail!(
            "Insufficient buying power: need ${estimated_cost:.2}, available ${available:.2} \
             (shortfall ${shortfall:.2}). Sell holdings or wait for a prior sell to fill and \
             settle before placing buys. Check with `schwab portfolio buying-power --account-number <hash> --json`."
        );
    }
    Ok(())
}

pub async fn validate_buying_power_for_order(
    api: &schwab_api::TraderApi,
    account_hash: &str,
    order: &Value,
    market_ask: Option<f64>,
) -> Result<BuyingPower> {
    let parsed = parse_order(order)?;
    if !order_requires_buying_power(&parsed) {
        return account_buying_power(api, account_hash).await;
    }

    let buying_power = account_buying_power(api, account_hash).await?;
    if let Some(cost) = estimate_notional(&parsed, None).or_else(|| {
        parsed.legs.iter().find_map(|leg| {
            if leg.instruction != "BUY" || leg.asset_type != "EQUITY" {
                return None;
            }
            let price = parsed.limit_price.or(market_ask)?;
            Some(leg.quantity * price)
        })
    }) {
        ensure_sufficient_buying_power(&buying_power, cost)?;
    }

    Ok(buying_power)
}

pub async fn validate_buying_power_after_preview(
    api: &schwab_api::TraderApi,
    account_hash: &str,
    order: &Value,
    preview: &Value,
) -> Result<()> {
    ensure_preview_accepted(preview)?;
    ensure_preview_buying_power(preview)?;

    let parsed = parse_order(order)?;
    if !order_requires_buying_power(&parsed) {
        return Ok(());
    }

    let buying_power = account_buying_power(api, account_hash).await?;
    if let Some(cost) = estimate_notional(&parsed, Some(preview)) {
        ensure_sufficient_buying_power(&buying_power, cost)?;
    }
    Ok(())
}

/// Schwab embeds hard rejects in preview even when the preview HTTP call succeeds.
pub fn ensure_preview_accepted(preview: &Value) -> Result<()> {
    let Some(rejects) = preview
        .pointer("/orderValidationResult/rejects")
        .and_then(|v| v.as_array())
    else {
        return Ok(());
    };
    if rejects.is_empty() {
        return Ok(());
    }
    let messages: Vec<String> = rejects
        .iter()
        .filter_map(|r| {
            r.get("activityMessage")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .collect();
    bail!(
        "Schwab preview rejected order: {}",
        if messages.is_empty() {
            "unknown reason".into()
        } else {
            messages.join("; ")
        }
    );
}

/// Block orders that would drive projected buying power negative (common on spread margin).
pub fn ensure_preview_buying_power(preview: &Value) -> Result<()> {
    let balance = preview
        .pointer("/orderStrategy/orderBalance")
        .or_else(|| preview.get("orderBalance"));
    let Some(balance) = balance else {
        return Ok(());
    };
    for key in ["projectedBuyingPower", "projectedAvailableFund"] {
        if let Some(v) = balance.get(key).and_then(parse_num) {
            if v < 0.0 {
                bail!(
                    "Schwab preview shows insufficient buying power after order ({key}: ${v:.2})"
                );
            }
        }
    }
    Ok(())
}

pub fn summary_to_json(summary: &PortfolioSummary) -> Value {
    json!(summary)
}

fn extract_equity(balances: Option<&Value>) -> Option<f64> {
    let b = balances?;
    for key in ["equity", "accountValue", "liquidationValue"] {
        if let Some(v) = b.get(key).and_then(parse_num) {
            return Some(v);
        }
    }
    None
}

fn extract_balance_field(balances: Option<&Value>, key: &str) -> Option<f64> {
    balances?.get(key).and_then(parse_num)
}

fn parse_num(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn last4(acct: &str) -> String {
    if acct.len() >= 4 {
        acct[acct.len() - 4..].to_string()
    } else {
        acct.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schwab_api::models::account::{Account, AccountsInstrument, Position, SecuritiesAccount};
    use serde_json::json;

    #[test]
    fn preview_reject_is_surfaced() {
        let preview = json!({
            "orderValidationResult": {
                "rejects": [{
                    "activityMessage": "You do not have enough available cash/buying power for this order."
                }]
            }
        });
        assert!(ensure_preview_accepted(&preview).is_err());
    }

    #[test]
    fn preview_negative_projected_buying_power_is_blocked() {
        let preview = json!({
            "orderStrategy": {
                "orderBalance": {
                    "projectedBuyingPower": -100.0
                }
            }
        });
        assert!(ensure_preview_buying_power(&preview).is_err());
    }

    #[test]
    fn summarizes_positions() {
        let accounts = vec![Account {
            securities_account: Some(SecuritiesAccount {
                account_number: Some("12345678".into()),
                round_trips: None,
                is_day_trader: None,
                is_closing_only_restricted: None,
                pfcb_flag: None,
                positions: Some(vec![Position {
                    short_quantity: None,
                    average_price: None,
                    current_day_profit_loss: None,
                    current_day_profit_loss_percentage: None,
                    long_quantity: Some(10.0),
                    settled_long_quantity: None,
                    settled_short_quantity: None,
                    aged_quantity: None,
                    instrument: Some(AccountsInstrument {
                        cusip: None,
                        symbol: Some("AAPL".into()),
                        description: Some("Apple".into()),
                        instrument_id: None,
                        net_change: None,
                        r#type: None,
                    }),
                    market_value: Some(1000.0),
                    maintenance_requirement: None,
                    average_long_price: None,
                    average_short_price: None,
                    tax_lot_average_long_price: None,
                    tax_lot_average_short_price: None,
                    long_open_profit_loss: None,
                    short_open_profit_loss: None,
                    previous_session_long_quantity: None,
                    previous_session_short_quantity: None,
                    current_day_cost: None,
                }]),
                initial_balances: None,
                current_balances: Some(json!({ "equity": 5000.0 })),
                projected_balances: None,
            }),
        }];

        let summary = summarize_accounts(&accounts);
        assert_eq!(summary.total_equity, 5000.0);
        assert_eq!(summary.aggregated_holdings[0].symbol, "AAPL");
    }

    #[test]
    fn extracts_buying_power() {
        let current = json!({
            "cashAvailableForTrading": 78.96,
            "cashBalance": 78.96,
            "liquidationValue": 28413.79
        });
        let power = extract_buying_power(Some(&current), None);
        assert_eq!(power.cash_available_for_trading, 78.96);
        assert_eq!(power.liquidation_value, Some(28413.79));
    }

    #[test]
    fn blocks_buy_with_insufficient_funds() {
        let power = BuyingPower {
            cash_available_for_trading: 78.96,
            cash_balance: 78.96,
            option_buying_power: None,
            liquidation_value: Some(28413.79),
        };
        let err = ensure_sufficient_buying_power(&power, 253.25).unwrap_err();
        assert!(err.to_string().contains("Insufficient buying power"));
    }

    #[test]
    fn estimates_limit_buy_cost() {
        let cost = estimate_equity_buy_cost(5.0, "limit", Some(50.65), None).unwrap();
        assert!((cost - 253.25).abs() < 0.01);
    }
}
