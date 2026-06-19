use anyhow::Result;
use serde_json::json;

use crate::cli::MarketCommands;
use crate::config::RuntimeConfig;
use crate::market_info::{build_info_dossier, InfoOptions};
use crate::output::ResponseEnvelope;

pub async fn run(runtime: &RuntimeConfig, command: MarketCommands) -> Result<()> {
    let api = runtime.build_market_api()?;

    match command {
        MarketCommands::Info {
            symbol,
            no_history,
            history_period_type,
            history_period,
            history_frequency_type,
        } => {
            let symbols: Vec<String> = symbol
                .split(',')
                .map(|s| s.trim().to_uppercase())
                .filter(|s| !s.is_empty())
                .collect();

            let period_type = match history_period_type.as_str() {
                "day" | "month" | "year" | "ytd" => history_period_type.as_str(),
                other => {
                    anyhow::bail!(
                        "Invalid --history-period-type {other:?}; use day, month, year, or ytd"
                    );
                }
            };
            let frequency_type = match history_frequency_type.as_str() {
                "minute" | "daily" | "weekly" | "monthly" => history_frequency_type.as_str(),
                other => {
                    anyhow::bail!(
                        "Invalid --history-frequency-type {other:?}; use minute, daily, weekly, or monthly"
                    );
                }
            };

            let options = InfoOptions {
                include_history: !no_history,
                history_period_type: period_type.to_string(),
                history_period,
                history_frequency_type: frequency_type.to_string(),
            };

            let data = build_info_dossier(&api, &symbols, options).await?;
            runtime.emit(
                ResponseEnvelope::ok("market info", json!(data)).with_inputs(json!({
                    "symbol": symbol,
                    "no_history": no_history,
                    "history_period_type": history_period_type,
                    "history_period": history_period,
                    "history_frequency_type": history_frequency_type,
                })),
            );
        }
        MarketCommands::Quotes {
            symbols,
            fields,
            indicative,
        } => {
            let data = api
                .quotes()
                .get_quotes(&symbols, fields.as_deref(), indicative)
                .await?;
            runtime.emit(
                ResponseEnvelope::ok("market quotes", json!(data))
                    .with_inputs(json!({ "symbols": symbols, "fields": fields })),
            );
        }
        MarketCommands::Quote {
            symbol,
            fields,
            indicative,
        } => {
            let data = api
                .quotes()
                .get_quote(&symbol, fields.as_deref(), indicative)
                .await?;
            runtime.emit(
                ResponseEnvelope::ok("market quote", json!(data))
                    .with_inputs(json!({ "symbol": symbol, "fields": fields })),
            );
        }
        MarketCommands::History {
            symbol,
            period_type,
            period,
            frequency_type,
            frequency,
            start_date,
            end_date,
            need_extended_hours_data,
            need_previous_close,
        } => {
            let data = api
                .price_history()
                .get(
                    &symbol,
                    period_type.as_deref(),
                    period,
                    frequency_type.as_deref(),
                    frequency,
                    start_date,
                    end_date,
                    need_extended_hours_data,
                    need_previous_close,
                )
                .await?;
            runtime.emit(
                ResponseEnvelope::ok("market history", json!(data)).with_inputs(json!({
                    "symbol": symbol,
                    "period_type": period_type,
                    "period": period,
                    "frequency_type": frequency_type,
                    "frequency": frequency,
                })),
            );
        }
        MarketCommands::Instrument { symbol, projection } => {
            let data = api.instruments().search(&symbol, &projection).await?;
            runtime.emit(
                ResponseEnvelope::ok("market instrument", json!(data)).with_inputs(json!({
                    "symbol": symbol,
                    "projection": projection,
                })),
            );
        }
        MarketCommands::InstrumentByCusip { cusip } => {
            let data = api.instruments().get_by_cusip(&cusip).await?;
            runtime.emit(
                ResponseEnvelope::ok("market instrument by cusip", json!(data))
                    .with_inputs(json!({ "cusip": cusip })),
            );
        }
        MarketCommands::Hours { markets, date } => {
            let data = api.markets().hours(&markets, date.as_deref()).await?;
            runtime.emit(
                ResponseEnvelope::ok("market hours", json!(data))
                    .with_inputs(json!({ "markets": markets, "date": date })),
            );
        }
        MarketCommands::HoursFor { market, date } => {
            let data = api
                .markets()
                .hours_for_market(&market, date.as_deref())
                .await?;
            runtime.emit(
                ResponseEnvelope::ok("market hours for", json!(data))
                    .with_inputs(json!({ "market": market, "date": date })),
            );
        }
    }

    Ok(())
}
