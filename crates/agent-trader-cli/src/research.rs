//! Paper-only research arm (plan `docs/RESEARCH_ARM_PLAN.md`).
//!
//! This module builds a Public-only, read-only registry wrapped in
//! [`research_core::PaperOnlyBroker`], runs the fail-closed self-test, and
//! (Phase 1) gathers market context and an LLM forecast without ever applying a
//! patch or placing an order.
//!
//! Hard gate layers: L1 (no live flags), L2 (paper broker), L3 (read-only HTTP),
//! L4 (config/account), L5 (runtime tripwire).

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use broker_core::{Broker, BrokerKind, BrokerRegistry, DataSource};
use broker_public::PublicBackend;
use chrono::Utc;
use research_core::web_search::{OpenRouterClient, SearchParams};
use research_core::{run_self_test, ArmConfig, Forecast, GateInputs, PaperOnlyBroker};
use serde_json::{json, Value};

use crate::paths::state_dir;

/// A Public-only registry whose trading handle refuses every write.
pub struct PaperRegistry {
    pub registry: BrokerRegistry,
    pub paper: Arc<PaperOnlyBroker>,
    pub read_only: bool,
}

/// Build the paper registry. Schwab is never constructed here. Public is
/// read-only at the HTTP layer and wrapped in `PaperOnlyBroker` at the trait
/// layer (plan §4, L2 + L3).
pub fn build_paper_registry() -> Result<PaperRegistry> {
    let backend = PublicBackend::from_env_read_only()
        .context("research arm needs Public read access (PUBLIC_API_KEY)")?;
    let read_only = backend.is_read_only();
    let (trading, data) = backend.into_arcs();
    let tripwire = state_dir()?.join("paper-research").join("unsafe_attempt.json");
    let paper = Arc::new(PaperOnlyBroker::new(trading, Some(tripwire)));
    let mut registry = BrokerRegistry::default();
    let trading_handle: Arc<dyn Broker> = paper.clone();
    registry.insert(BrokerKind::Public, trading_handle, data);
    Ok(PaperRegistry {
        registry,
        paper,
        read_only,
    })
}

/// Static config validation. Does not need credentials and does not network.
pub fn run_validate(rules: &Path) -> Result<Value> {
    let config = ArmConfig::load(rules)?;
    let allowlist = research_core::patch::effective_allowlist(&config.adaptation.allowlist);
    Ok(json!({
        "ok": true,
        "arm_id": config.arm_id,
        "mode": config.mode,
        "account": config.account_address()?,
        "forecast": {
            "target": config.forecast.target,
            "benchmark": config.forecast.benchmark,
            "horizon_days": config.forecast.horizon_days,
        },
        "universe": config.universe,
        "interval": { "cron": config.interval.cron, "timezone": config.interval.timezone },
        "effective_patch_allowlist": allowlist,
        "search": {
            "enabled": config.research.search.enabled,
            "provider": config.research.search.provider,
            "engine": config.research.search.engine,
            "model": config.research.search.model,
            "max_total_results": config.research.search.max_total_results,
        },
        "note": "static validation only; the hard gate runs on `research tick`",
    }))
}

/// One Phase 1 research pass: gate, gather, optional forecast, journal.
/// Never applies a patch and never places an order.
pub async fn run_tick(rules: &Path) -> Result<Value> {
    let config = ArmConfig::load(rules)?;
    let paper = build_paper_registry()?;
    let account = broker_core::parse_account_address(&config.account_address()?)?;

    // --- Hard gate (plan §4, L4 + startup self-test) ---
    let gate = run_self_test(&GateInputs {
        config: &config,
        registry_brokers: vec!["public".to_string()],
        public_paper_wrapped: true,
        public_read_only: paper.read_only,
        personal_secret_set: std::env::var("PUBLIC_PERSONAL_SECRET").is_ok(),
        binary_hash: None,
    });
    if !gate.ok {
        let failures: Vec<&str> = gate.failures().iter().map(|c| c.name.as_str()).collect();
        bail!(
            "research gate failed: {}",
            failures.join(", ")
        );
    }

    let handle = paper
        .registry
        .resolve(&account, DataSource::AccountBroker)?;

    // --- Gather Public market data (read-only) ---
    let quotes = handle.data.quotes(&account, &config.universe).await?;

    // --- Optional LLM + web-search forecast (Phase 1) ---
    let search_params = SearchParams::from_config(&config.research.search);
    let mut forecast: Option<Forecast> = None;
    let mut citations: Vec<Value> = Vec::new();
    let mut forecast_note = String::new();

    if config.research.search.enabled && std::env::var("OPENROUTER_API_KEY").is_ok() {
        let client = OpenRouterClient::from_env()?;
        let (system, user) = build_prompt(&config, &quotes);
        let result = client
            .chat(
                &config.research.search.model,
                &system,
                &user,
                &search_params,
                4096,
            )
            .await?;
        citations = result
            .citations
            .iter()
            .map(|c| json!({"url": c.url, "title": c.title}))
            .collect();
        match parse_forecast(&result.content, &config) {
            Ok(parsed) => {
                parsed.validate()?;
                forecast = Some(parsed);
            }
            Err(err) => {
                forecast_note = format!("forecast parse failed: {err}");
            }
        }
    } else {
        forecast_note =
            "no OPENROUTER_API_KEY or search disabled: context-only pass".to_string();
    }

    // --- Journal (no apply, no place) ---
    let record = json!({
        "kind": "research_tick",
        "phase": 1,
        "arm_id": config.arm_id,
        "at": Utc::now().to_rfc3339(),
        "account": account.address(),
        "gate": gate,
        "universe": config.universe,
        "quote_count": quotes.len(),
        "forecast": forecast,
        "citations": citations,
        "forecast_note": forecast_note,
        "applied_patches": [],
        "orders_placed": 0,
        "place_attempts": paper.paper.attempts(),
    });
    journal_append(&config.arm_id, &record)?;

    if paper.paper.attempts() != 0 {
        bail!(
            "paper tripwire fired {} time(s); see unsafe_attempt.json",
            paper.paper.attempts()
        );
    }
    Ok(record)
}

fn journal_append(arm_id: &str, record: &Value) -> Result<()> {
    let dir = state_dir()?.join("paper-research");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("journal-{arm_id}.jsonl"));
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

fn build_prompt(config: &ArmConfig, quotes: &[broker_core::Quote]) -> (String, String) {
    let system = format!(
        "You are the research analyst for a PAPER-ONLY trading arm. You never place orders. \
         Produce a forecast for the target `{}` over the next {} trading days relative to {}. \
         You may use the web_search tool to gather news and macro context. \
         Respond with ONLY a JSON object matching this shape: \
         {{\"arm_id\":string,\"as_of\":string,\"target\":\"{}\",\"benchmark\":\"{}\",\
         \"horizon_days\":number,\"signals\":[{{\"symbol\":string,\"direction\":\"up|down|flat\",\
         \"expected_return_pct\":number,\"confidence\":0..1,\"rank\":number,\"citations\":[string]}}],\
         \"regime\":\"risk_on|risk_off|chop\",\"plan_patch\":[{{\"path\":string,\"value\":number,\
         \"reason\":string}}],\"risk_alerts\":[string],\"notes\":string}}. \
         Only propose plan_patch paths from this allowlist: {:?}.",
        config.forecast.target,
        config.forecast.horizon_days,
        config.forecast.benchmark,
        config.forecast.target,
        config.forecast.benchmark,
        research_core::patch::effective_allowlist(&config.adaptation.allowlist),
    );
    let user = json!({
        "arm_id": config.arm_id,
        "as_of": Utc::now().to_rfc3339(),
        "universe": config.universe,
        "quotes": quotes,
    })
    .to_string();
    (system, user)
}

fn parse_forecast(content: &str, config: &ArmConfig) -> Result<Forecast> {
    let trimmed = strip_code_fence(content);
    let mut value: Value = serde_json::from_str(&trimmed)
        .context("forecast content was not valid JSON")?;
    // Fill in fields the model may omit, so validation failures are about content.
    if let Some(object) = value.as_object_mut() {
        object
            .entry("arm_id")
            .or_insert_with(|| json!(config.arm_id));
        object
            .entry("target")
            .or_insert_with(|| json!(config.forecast.target));
        object
            .entry("benchmark")
            .or_insert_with(|| json!(config.forecast.benchmark));
        object
            .entry("horizon_days")
            .or_insert_with(|| json!(config.forecast.horizon_days));
        object
            .entry("as_of")
            .or_insert_with(|| json!(Utc::now().to_rfc3339()));
    }
    let forecast: Forecast = serde_json::from_value(value)?;
    Ok(forecast)
}

fn strip_code_fence(content: &str) -> String {
    let trimmed = content.trim();
    let without_open = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    without_open
        .strip_suffix("```")
        .unwrap_or(without_open)
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_fence_handles_plain_and_fenced() {
        assert_eq!(strip_code_fence("{\"a\":1}"), "{\"a\":1}");
        assert_eq!(strip_code_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_code_fence("```\n{\"a\":1}\n```"), "{\"a\":1}");
    }

    #[test]
    fn parse_forecast_fills_missing_metadata() {
        let config = ArmConfig {
            arm_id: "arm".into(),
            account: Some("public:ACC".into()),
            universe: vec!["MSFT".into()],
            ..Default::default()
        };
        let raw = r#"{"signals":[{"symbol":"MSFT","direction":"up","confidence":0.6}]}"#;
        let forecast = parse_forecast(raw, &config).unwrap();
        assert_eq!(forecast.arm_id, "arm");
        assert_eq!(forecast.target, research_core::config::EXPECTED_TARGET);
        assert_eq!(forecast.horizon_days, 5);
        forecast.validate().unwrap();
    }
}
