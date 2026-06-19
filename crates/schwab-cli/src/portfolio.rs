use anyhow::Result;
use schwab_api::models::account::Account;
use serde_json::{json, Value};

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

pub async fn account_equity(api: &schwab_api::TraderApi, account_hash: &str) -> Result<Option<f64>> {
    let account = api
        .accounts()
        .get(account_hash, Some("positions"))
        .await?;
    Ok(account
        .securities_account
        .as_ref()
        .and_then(|sa| extract_equity(sa.current_balances.as_ref())))
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
}
