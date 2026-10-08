//! Research commands. They write journals and arm inputs. They do not place orders.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use serde_json::{json, Value};

use crate::agent::llm::OpenRouterClient;
use crate::agent::llm_agent::{
    self, decision_row, drive_turns, execute_tool, tool_schemas, ModelReply, ToolCall,
};
use crate::agent::llm_signal::{
    self, append_decision, build_named_decision, parse_score_json, prompt_template, stub_score,
    ParsedScore,
};
use crate::backtest::cache::BacktestCache;
use crate::cli::ResearchCommands;
use crate::config::TraderRuntime;
use crate::research::outcomes::{self, label_cache};
use crate::rules::TraderRules;

pub async fn run(runtime: &TraderRuntime, command: ResearchCommands) -> Result<()> {
    match command {
        ResearchCommands::Outcomes {
            rules_file,
            cache,
            output,
            max_symbols,
        } => {
            let rules = TraderRules::load(&rules_file)?;
            let cache_path =
                cache.unwrap_or_else(|| crate::agent::paths::backtest_cache_path(&rules_file));
            let loaded = BacktestCache::load(&cache_path)?;
            let rows = label_cache(&rules, &loaded, max_symbols);
            let out = output.unwrap_or_else(|| PathBuf::from("rules/candidate-outcomes.jsonl"));
            outcomes::write_jsonl(&out, &rows)?;
            runtime.emit(schwab_cli::output::ResponseEnvelope::ok(
                "research outcomes",
                json!({"rows": rows.len(), "output": out, "cache": cache_path}),
            ));
            Ok(())
        }
        ResearchCommands::Signal {
            rules_file,
            dry_run,
            symbol,
        } => run_signal(&rules_file, dry_run, symbol).await,
        ResearchCommands::Agent {
            rules_file,
            dry_run,
            symbol,
        } => run_agent(&rules_file, dry_run, symbol.as_deref()).await,
        ResearchCommands::EarningsRefresh { rules_file, days } => {
            refresh_earnings(&rules_file, days).await
        }
    }
}

async fn run_signal(
    rules_file: &std::path::Path,
    dry_run: bool,
    only: Option<String>,
) -> Result<()> {
    let rules = TraderRules::load(rules_file)?;
    let symbols = match only {
        Some(s) => vec![s.trim().to_uppercase()],
        None => rules
            .candidate_pool_symbols(rules_file)
            .unwrap_or_else(|_| rules.all_watchlist_symbols().into_iter().take(60).collect()),
    };
    let day = crate::market_session::trading_day(&rules.schedule.timezone);
    let path = llm_signal::journal_path(&rules);
    let mut written = 0u32;
    let client = if dry_run {
        None
    } else {
        Some(OpenRouterClient::from_env()?)
    };
    let fmp = crate::fmp::FmpClient::from_env().ok();
    for symbol in &symbols {
        let headlines = if let Some(fmp) = &fmp {
            fmp.headlines(symbol, 5).await.unwrap_or_default()
        } else {
            Vec::new()
        };
        let bundle = json!({
            "symbol": symbol,
            "as_of": day.format("%Y-%m-%d").to_string(),
            "headlines": headlines.iter().map(|h| json!({"id": h.id, "title": h.title, "published": h.published})).collect::<Vec<_>>(),
            "note": "Point-in-time bundle. Do not use knowledge of later prices."
        });
        for model in &rules.llm_signal.models {
            for blinded in [false, true] {
                if blinded && !rules.llm_signal.blinded {
                    continue;
                }
                let shown = if blinded {
                    llm_signal::blind_value(&bundle)
                } else {
                    bundle.clone()
                };
                let samples = if dry_run {
                    let score = stub_score(None);
                    vec![ParsedScore {
                        score,
                        event_risk: false,
                        event_type: "none".into(),
                        gap_risk: false,
                        confidence: 0.0,
                        headline_ids: vec![],
                    }]
                } else {
                    let mut out = Vec::new();
                    let n = rules.llm_signal.samples.max(1);
                    for _ in 0..n {
                        let raw = score_once(
                            client.as_ref().unwrap(),
                            model,
                            &shown,
                            rules.llm_signal.max_tokens,
                            rules.llm_signal.temperature,
                        )
                        .await?;
                        out.push(parse_score_json(&raw)?);
                    }
                    out
                };
                let row = build_named_decision(
                    &rules.llm_signal,
                    day,
                    symbol,
                    model,
                    &shown,
                    samples,
                    blinded,
                    !dry_run,
                );
                append_decision(&path, &row)?;
                written += 1;
            }
        }
    }
    println!(
        "llm-signal wrote {written} rows to {} (live={})",
        path.display(),
        !dry_run
    );
    Ok(())
}

async fn score_once(
    client: &OpenRouterClient,
    model: &str,
    bundle: &Value,
    max_tokens: u32,
    temperature: f64,
) -> Result<Value> {
    let body = json!({
        "model": model,
        "temperature": temperature,
        "max_tokens": max_tokens,
        "messages": [
            {"role": "system", "content": prompt_template()},
            {"role": "user", "content": serde_json::to_string(bundle)?}
        ],
        "response_format": {"type": "json_object"}
    });
    let message = client.post_chat(body).await?;
    let content = message
        .get("content")
        .and_then(|v| v.as_str())
        .context("empty LLM content")?;
    crate::agent::llm::extract_json_object(content)
}

async fn run_agent(
    rules_file: &std::path::Path,
    dry_run: bool,
    symbol: Option<&str>,
) -> Result<()> {
    let rules = TraderRules::load(rules_file)?;
    let symbol = symbol.unwrap_or("").trim().to_uppercase();
    anyhow::ensure!(!symbol.is_empty(), "research agent requires --symbol");
    let day = crate::market_session::trading_day(&rules.schedule.timezone);
    let bundle = json!({
        "symbol": symbol,
        "as_of": day.format("%Y-%m-%d").to_string(),
        "admitted": true,
        "snapshot": {},
        "headlines": [],
        "filings": {},
        "peer_rs": {},
        "position": {"open": false},
        "base_rates": {"n": 0}
    });
    let model = rules
        .llm_signal
        .models
        .first()
        .cloned()
        .unwrap_or_else(|| "google/gemini-2.5-flash".into());
    let decision = if dry_run {
        drive_turns(
            &bundle,
            rules.llm_signal.agent_max_turns,
            rules.llm_signal.agent_dollar_budget,
            |_| {
                Ok(ModelReply {
                    content: r#"{"action":"approve","size_multiplier":1.0,"reason_codes":["dry_run"],"bull":"","bear":""}"#.into(),
                    tool_calls: vec![],
                    prompt_tokens: 1,
                    completion_tokens: 1,
                })
            },
        )?
    } else {
        let client = OpenRouterClient::from_env()?;
        live_agent(&client, &model, &bundle, &rules).await?
    };
    let cutoff = rules
        .llm_signal
        .knowledge_cutoffs
        .get(&model)
        .cloned()
        .unwrap_or_default();
    let row = decision_row(
        &day.format("%Y-%m-%d").to_string(),
        &symbol,
        &model,
        &cutoff,
        &decision,
        !dry_run,
    );
    let path = llm_signal::journal_path(&rules);
    append_decision(&path, &row)?;
    println!(
        "llm-agent {symbol} action={} size={:.2} turns={} journal={}",
        decision.action,
        decision.size_multiplier,
        decision.turns,
        path.display()
    );
    Ok(())
}

async fn live_agent(
    client: &OpenRouterClient,
    model: &str,
    bundle: &Value,
    rules: &TraderRules,
) -> Result<llm_agent::AgentDecision> {
    // The sync driver cannot await. Replay the same bounds here: tools, one
    // bull/bear nudge, clamp, veto on a bad parse. No order tool exists.
    let mut messages = vec![
        json!({"role": "system", "content": llm_agent::analyst_system_prompt()}),
        json!({"role": "user", "content": serde_json::to_string(bundle)?}),
    ];
    let mut spent = 0.0;
    let mut turns = 0u32;
    let mut last = String::new();
    while turns < rules.llm_signal.agent_max_turns.max(1)
        && spent <= rules.llm_signal.agent_dollar_budget
    {
        let body = json!({
            "model": model,
            "temperature": 0,
            "max_tokens": rules.llm_signal.max_tokens,
            "messages": messages,
            "tools": tool_schemas()
        });
        let message = client.post_chat(body).await?;
        turns += 1;
        let prompt = message
            .pointer("/usage/prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(500) as u32;
        let completion = message
            .pointer("/usage/completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(200) as u32;
        spent += llm_agent::estimate_dollars(prompt, completion);
        let calls = tool_calls_from(&message);
        messages.push(message.clone());
        if calls.is_empty() {
            last = message
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            break;
        }
        for call in calls {
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": serde_json::to_string(&execute_tool(&call.name, bundle))?
            }));
        }
        if turns == 1 {
            messages.push(json!({"role": "user", "content": "One bull case and one bear case, then the final JSON."}));
        }
    }
    let mut decision = llm_agent::parse_agent_json(&last).unwrap_or(llm_agent::AgentDecision {
        action: "veto".into(),
        size_multiplier: 0.5,
        reason_codes: vec!["agent_unparseable".into()],
        turns,
        transcript: messages,
    });
    decision.turns = turns;
    decision.size_multiplier = llm_signal::clamp_size_multiplier(decision.size_multiplier);
    if decision.action != "approve" {
        decision.action = "veto".into();
    }
    Ok(decision)
}

fn tool_calls_from(message: &Value) -> Vec<ToolCall> {
    message
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let id = c.get("id").and_then(|v| v.as_str())?.to_string();
                    let name = c
                        .pointer("/function/name")
                        .and_then(|v| v.as_str())?
                        .to_string();
                    let arguments = c
                        .pointer("/function/arguments")
                        .and_then(|v| v.as_str())
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or(json!({}));
                    Some(ToolCall {
                        id,
                        name,
                        arguments,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn refresh_earnings(rules_file: &std::path::Path, days: u32) -> Result<()> {
    let rules = TraderRules::load(rules_file)?;
    let client = crate::fmp::FmpClient::from_env()?;
    let from = Utc::now().date_naive();
    let to = from + Duration::days(days as i64);
    let cal = client
        .earnings_calendar(
            &from.format("%Y-%m-%d").to_string(),
            &to.format("%Y-%m-%d").to_string(),
        )
        .await?;
    let dest = rules
        .earnings
        .calendar_file
        .clone()
        .unwrap_or_else(|| "rules/earnings-calendar.yaml".into());
    let mut lines = String::from("# Confirmed dates from FMP. Heuristic is the fallback when a symbol is absent.\nsymbols:\n");
    let mut keys: Vec<_> = cal.dates.keys().cloned().collect();
    keys.sort();
    for sym in keys {
        let dates = &cal.dates[&sym];
        let rendered: Vec<String> = dates.iter().map(|d| format!("\"{d}\"")).collect();
        lines.push_str(&format!("  {sym}: [{}]\n", rendered.join(", ")));
    }
    if let Some(parent) = std::path::Path::new(&dest).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&dest, lines)?;
    println!("wrote {} symbols to {dest}", cal.dates.len());
    Ok(())
}
