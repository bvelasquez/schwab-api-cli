use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use broker_core::{
    AccountKind, AccountRef, AssetKind, Balances, Bar, Broker, BrokerKind, Capabilities, Greeks,
    MarketData, OptionChain, OptionContract, Order, OrderIntent, OrderStatus, PlaceResult,
    Portfolio, Position, Preflight, Quote, SessionStatus, Side, SpreadLeg, StrategyLeg,
};
use chrono::{DateTime, Utc};
use schwab_api::{ClientConfig, SchwabClient, TraderApi};
use schwab_market_data::endpoints::chains::ChainQuery;
use schwab_market_data::MarketDataApi;
use serde_json::Value;

use crate::translate::schwab_order_json;

pub struct SchwabBackend {
    trader: TraderApi,
    market: MarketDataApi,
}

impl SchwabBackend {
    pub fn from_env() -> Result<Self> {
        let config = ClientConfig::from_env().context("Schwab client config")?;
        let client = SchwabClient::new(config);
        Ok(Self {
            trader: TraderApi::new(client.clone()),
            market: MarketDataApi::new(client),
        })
    }

    pub fn into_arcs(self) -> (Arc<dyn Broker>, Arc<dyn MarketData>) {
        let trading: Arc<dyn Broker> = Arc::new(Self {
            trader: self.trader.clone(),
            market: self.market.clone(),
        });
        let data: Arc<dyn MarketData> = Arc::new(self);
        (trading, data)
    }
}

#[async_trait]
impl Broker for SchwabBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities::schwab()
    }

    async fn accounts(&self) -> Result<Vec<AccountRef>> {
        let numbers = self.trader.accounts().account_numbers().await?;
        Ok(numbers
            .into_iter()
            .map(|row| {
                let mut account = AccountRef::new(BrokerKind::Schwab, row.hash_value);
                account.label = Some(row.account_number);
                account
            })
            .collect())
    }

    async fn portfolio(&self, account: &AccountRef) -> Result<Portfolio> {
        let raw = self
            .trader
            .accounts()
            .get(&account.id, Some("positions"))
            .await?;
        let sa = raw
            .securities_account
            .context("Schwab account has no securitiesAccount")?;
        let balances = balances_from(sa.current_balances.as_ref(), sa.projected_balances.as_ref());
        let positions = sa
            .positions
            .unwrap_or_default()
            .into_iter()
            .filter_map(|position| {
                let symbol = position.instrument.as_ref()?.symbol.clone()?;
                let asset = match position
                    .instrument
                    .as_ref()
                    .and_then(|instrument| instrument.r#type.as_deref())
                {
                    Some("OPTION") => AssetKind::Option,
                    _ => AssetKind::Equity,
                };
                let long_qty = position.long_quantity.unwrap_or(0.0);
                let short_qty = position.short_quantity.unwrap_or(0.0);
                Some(Position {
                    symbol,
                    asset,
                    quantity: long_qty - short_qty,
                    average_price: position.average_price,
                    market_value: position.market_value,
                })
            })
            .collect();
        let kind = if balances.options_buying_power.is_some() {
            AccountKind::Margin
        } else {
            AccountKind::Cash
        };
        Ok(Portfolio {
            account: account.clone(),
            kind,
            balances,
            positions,
            strategies: Vec::new(),
        })
    }

    async fn open_orders(&self, account: &AccountRef) -> Result<Vec<Order>> {
        let rows = self
            .trader
            .orders()
            .list_for_account(&account.id, None, None, Some("WORKING"), None)
            .await?;
        Ok(rows.iter().filter_map(order_from_value).collect())
    }

    async fn order_history(&self, account: &AccountRef, _since: DateTime<Utc>) -> Result<Vec<Order>> {
        let rows = self
            .trader
            .orders()
            .list_for_account(&account.id, None, None, None, Some("200"))
            .await?;
        Ok(rows.iter().filter_map(order_from_value).collect())
    }

    async fn preflight(&self, account: &AccountRef, intent: &OrderIntent) -> Result<Preflight> {
        intent.validate_for(&self.capabilities())?;
        let body = schwab_order_json(intent)?;
        let preview = self.trader.orders().preview(&account.id, &body).await?;
        Ok(preflight_from_preview(&preview))
    }

    async fn place(
        &self,
        account: &AccountRef,
        intent: &OrderIntent,
        _client_id: &str,
    ) -> Result<PlaceResult> {
        intent.validate_for(&self.capabilities())?;
        let body = schwab_order_json(intent)?;
        let placed = self.trader.orders().place(&account.id, &body).await?;
        let order_id = placed
            .location
            .as_deref()
            .and_then(order_id_from_location)
            .unwrap_or_default();
        Ok(PlaceResult {
            order_id,
            status: OrderStatus::Accepted,
            message: format!("Schwab accepted the order (HTTP {})", placed.status),
        })
    }

    async fn get_order(&self, account: &AccountRef, order_id: &str) -> Result<Order> {
        let raw = self.trader.orders().get(&account.id, order_id).await?;
        order_from_value(&raw).context("Schwab order payload missing id")
    }

    async fn cancel(&self, account: &AccountRef, order_id: &str) -> Result<()> {
        self.trader.orders().cancel(&account.id, order_id).await?;
        Ok(())
    }

    async fn replace(
        &self,
        account: &AccountRef,
        order_id: &str,
        intent: &OrderIntent,
    ) -> Result<PlaceResult> {
        let body = schwab_order_json(intent)?;
        let placed = self
            .trader
            .orders()
            .replace(&account.id, order_id, &body)
            .await?;
        Ok(PlaceResult {
            order_id: order_id.to_string(),
            status: OrderStatus::Accepted,
            message: format!("Schwab replace submitted (HTTP {})", placed.status),
        })
    }
}

#[async_trait]
impl MarketData for SchwabBackend {
    fn broker_name(&self) -> &'static str {
        "schwab"
    }

    async fn quotes(&self, _account: &AccountRef, symbols: &[String]) -> Result<Vec<Quote>> {
        if symbols.is_empty() {
            return Ok(vec![]);
        }
        let joined = symbols.join(",");
        let raw = self
            .market
            .quotes()
            .get_quotes(&joined, Some("quote"), None)
            .await?;
        Ok(parse_quotes(&raw))
    }

    async fn option_expirations(&self, _account: &AccountRef, underlying: &str) -> Result<Vec<String>> {
        let chain = self
            .market
            .chains()
            .get(&ChainQuery {
                symbol: underlying,
                strike_count: Some(1),
                ..ChainQuery::default()
            })
            .await?;
        Ok(expiration_keys(&chain))
    }

    async fn option_chain(
        &self,
        _account: &AccountRef,
        underlying: &str,
        expiration: &str,
    ) -> Result<OptionChain> {
        let raw = self
            .market
            .chains()
            .get(&ChainQuery {
                symbol: underlying,
                from_date: Some(expiration),
                to_date: Some(expiration),
                ..ChainQuery::default()
            })
            .await?;
        Ok(parse_chain(underlying, &raw))
    }

    async fn greeks(&self, account: &AccountRef, osi_symbol: &str) -> Result<Greeks> {
        let parsed = broker_core::parse_osi_symbol(osi_symbol)?;
        let expiry = parsed.expiry.format("%Y-%m-%d").to_string();
        let chain = self
            .option_chain(account, &parsed.underlying, &expiry)
            .await?;
        chain
            .contracts
            .into_iter()
            .find(|contract| contract.symbol.trim() == osi_symbol.trim())
            .map(|contract| contract.greeks)
            .context("option not found on the Schwab chain")
    }

    async fn strategy_quote(&self, _account: &AccountRef, legs: &[SpreadLeg]) -> Result<Quote> {
        anyhow::bail!(
            "Schwab has no strategy quote ({} legs); price the spread from the legs",
            legs.len()
        );
    }

    async fn bars(&self, _account: &AccountRef, symbol: &str, period: &str) -> Result<Vec<Bar>> {
        let (period_type, period_n, frequency_type) = match period.to_ascii_lowercase().as_str() {
            "day" | "1d" => ("year", 1, "daily"),
            "year" | "1y" => ("year", 1, "daily"),
            "month" => ("month", 1, "daily"),
            other => ("year", 1, if other.contains("min") { "minute" } else { "daily" }),
        };
        let raw = self
            .market
            .price_history()
            .get(
                symbol,
                Some(period_type),
                Some(period_n),
                Some(frequency_type),
                Some(1),
                None,
                None,
                Some(false),
                None,
            )
            .await?;
        Ok(parse_candles(&raw))
    }

    async fn session_open(&self, _account: &AccountRef) -> Result<SessionStatus> {
        let raw = self.market.markets().hours("equity", None).await?;
        let open = raw.to_string().to_ascii_lowercase().contains("\"isopen\":true")
            || raw.to_string().contains("\"isOpen\":true");
        Ok(SessionStatus {
            open,
            source: "schwab-market-hours".into(),
        })
    }
}

fn balances_from(current: Option<&Value>, projected: Option<&Value>) -> Balances {
    let cash = field(current, "cashAvailableForTrading")
        .or_else(|| field(projected, "cashAvailableForTrading"))
        .or_else(|| field(current, "buyingPower"))
        .or_else(|| field(current, "availableFunds"))
        .unwrap_or(0.0);
    let options = field(current, "optionBuyingPower").or_else(|| field(projected, "optionBuyingPower"));
    let equity = field(current, "liquidationValue")
        .or_else(|| field(current, "equity"))
        .unwrap_or(0.0);
    Balances {
        cash_buying_power: cash,
        options_buying_power: options,
        equity,
    }
}

fn field(value: Option<&Value>, name: &str) -> Option<f64> {
    value?.get(name)?.as_f64()
}

fn order_id_from_location(location: &str) -> Option<String> {
    location
        .rsplit('/')
        .next()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
}

fn order_from_value(value: &Value) -> Option<Order> {
    let id = value
        .get("orderId")
        .and_then(|id| id.as_i64().map(|n| n.to_string()).or_else(|| id.as_str().map(str::to_string)))?;
    let raw_status = value
        .get("status")
        .and_then(|status| status.as_str())
        .unwrap_or("UNKNOWN")
        .to_string();
    let filled = value
        .get("filledQuantity")
        .and_then(|qty| qty.as_f64())
        .unwrap_or(0.0);
    let average_price = value.get("price").and_then(|price| price.as_f64());
    Some(Order {
        id,
        status: OrderStatus::parse(&raw_status),
        filled_quantity: filled,
        average_price,
        raw_status,
    })
}

fn preflight_from_preview(preview: &Value) -> Preflight {
    let message = preview.to_string();
    let rejected = message.to_ascii_lowercase().contains("reject");
    Preflight {
        accepted: !rejected,
        estimated_cost: None,
        estimated_proceeds: None,
        buying_power_requirement: None,
        fees: None,
        message: "Schwab preview".into(),
    }
}

fn parse_quotes(raw: &Value) -> Vec<Quote> {
    let Some(map) = raw.as_object() else {
        return vec![];
    };
    map.iter()
        .filter_map(|(symbol, payload)| {
            let quote = payload.get("quote").unwrap_or(payload);
            Some(Quote {
                symbol: symbol.clone(),
                bid: num(quote.get("bidPrice")),
                ask: num(quote.get("askPrice")),
                last: num(quote.get("lastPrice")),
                bid_size: num(quote.get("bidSize")),
                ask_size: num(quote.get("askSize")),
                volume: num(quote.get("totalVolume")),
            })
        })
        .collect()
}

fn expiration_keys(raw: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["callExpDateMap", "putExpDateMap"] {
        if let Some(map) = raw.get(key).and_then(|value| value.as_object()) {
            for expiry in map.keys() {
                let date = expiry.split(':').next().unwrap_or(expiry);
                if !out.iter().any(|existing: &String| existing == date) {
                    out.push(date.to_string());
                }
            }
        }
    }
    out.sort();
    out
}

fn parse_chain(underlying: &str, raw: &Value) -> OptionChain {
    let mut contracts = Vec::new();
    collect_side(raw.get("callExpDateMap"), "C", &mut contracts);
    collect_side(raw.get("putExpDateMap"), "P", &mut contracts);
    OptionChain {
        underlying: underlying.to_uppercase(),
        expirations: expiration_keys(raw),
        contracts,
    }
}

fn collect_side(map: Option<&Value>, put_call: &str, out: &mut Vec<OptionContract>) {
    let Some(expiries) = map.and_then(|value| value.as_object()) else {
        return;
    };
    for (expiry_key, strikes) in expiries {
        let expiration = expiry_key.split(':').next().unwrap_or(expiry_key).to_string();
        let Some(strikes) = strikes.as_object() else {
            continue;
        };
        for (strike, rows) in strikes {
            let Some(rows) = rows.as_array() else {
                continue;
            };
            for row in rows {
                let symbol = row
                    .get("symbol")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                if symbol.is_empty() {
                    continue;
                }
                out.push(OptionContract {
                    symbol,
                    expiration: expiration.clone(),
                    strike: strike.parse().unwrap_or(0.0),
                    put_call: put_call.into(),
                    bid: num(row.get("bid")),
                    ask: num(row.get("ask")),
                    open_interest: num(row.get("openInterest")),
                    greeks: Greeks {
                        delta: num(row.get("delta")),
                        gamma: num(row.get("gamma")),
                        theta: num(row.get("theta")),
                        vega: num(row.get("vega")),
                        iv: num(row.get("volatility")),
                    },
                });
            }
        }
    }
}

fn parse_candles(raw: &Value) -> Vec<Bar> {
    raw.get("candles")
        .and_then(|value| value.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(Bar {
                        timestamp: row.get("datetime")?.to_string(),
                        open: num(row.get("open"))?,
                        high: num(row.get("high"))?,
                        low: num(row.get("low"))?,
                        close: num(row.get("close"))?,
                        volume: num(row.get("volume")).unwrap_or(0.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn num(value: Option<&Value>) -> Option<f64> {
    value.and_then(|value| {
        value
            .as_f64()
            .or_else(|| value.as_i64().map(|n| n as f64))
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
    })
}

pub fn group_option_legs(positions: &[Position]) -> Vec<broker_core::OptionStrategyGroup> {
    let mut groups = Vec::new();
    let mut index = 0;
    let options: Vec<_> = positions
        .iter()
        .filter(|position| position.asset == AssetKind::Option && position.quantity.abs() > 0.0)
        .collect();
    if options.is_empty() {
        return groups;
    }
    index += 1;
    groups.push(broker_core::OptionStrategyGroup {
        id: format!("schwab-options-{index}"),
        name: "options".into(),
        quantity: 1.0,
        legs: options
            .iter()
            .map(|position| StrategyLeg {
                symbol: position.symbol.clone(),
                quantity: position.quantity.abs(),
                side: if position.quantity >= 0.0 {
                    Side::Buy
                } else {
                    Side::Sell
                },
            })
            .collect(),
        market_value: None,
        unit_cost: options.iter().find_map(|position| position.average_price),
    });
    groups
}
