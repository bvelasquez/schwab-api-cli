//! Premarket LLM signal cache.
//!
//! The scorer decides once per (symbol, day) and writes a JSON cache. Shadow
//! arms read that cache. Nothing here places an order, and the production
//! entry path never calls [`apply_cached_decisions`].

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{json, Map, Value};

use super::llm::{
    analyst_tools, extract_json_object, push_tool_round, tokens_to_usd, tool_calls_of, ChatTurn,
    OpenRouterClient, ToolBudget,
};
use super::paths::{journal_path, state_path};
use super::state::TraderState;
use crate::rules::TraderRules;

const SIGNAL_PROMPT: &str = "\
You score a swing-trade setup from the point-in-time bundle only. \
Do not use knowledge of what the stock did after the bundle's dates. \
Respond ONLY with JSON: {\"score\": -2..2, \"event_risk\": bool, \
\"event_type\": \"earnings|guidance|mna|litigation|regulatory|other|none\", \
\"gap_risk\": bool, \"confidence\": 0..1, \"headline_ids\": [string], \"reason\": string}. \
score is the 5-10 day directional view. event_risk is true only for a dated catalyst \
inside the next 15 calendar days.";

const AGENT_PROMPT: &str = "\
You are a bounded analyst for names the deterministic rules already admitted. \
You may call tools, then finish with JSON only: \
{\"decision\": \"approve\"|\"veto\", \"size_multiplier\": 0.5..1.0, \"reason_codes\": [string]}. \
You cannot originate a trade, widen a stop, or raise size above 1.0. \
Veto only for event risk or a broken thesis visible in the tools. \
get_base_rates returns insufficient when n < 50; do not invent a base rate.";

pub fn cache_path(rules_path: &Path, trader_id: &str) -> PathBuf {
    rules_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("llm-signal-{trader_id}.json"))
}

pub fn journal_signal_path(rules_path: &Path, trader_id: &str) -> PathBuf {
    rules_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("llm-signal-journal-{trader_id}.jsonl"))
}

pub fn stable_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

pub fn prompt_hash() -> String {
    stable_hash(SIGNAL_PROMPT.as_bytes())
}

/// Median of scores. Empty input is 0.
pub fn median_score(samples: &[i64]) -> i64 {
    if samples.is_empty() {
        return 0;
    }
    let mut xs = samples.to_vec();
    xs.sort();
    xs[xs.len() / 2]
}

pub fn clamp_score(v: i64) -> i64 {
    v.clamp(-2, 2)
}

/// Size multipliers from the agent stay inside [0.5, 1.0]. Non-finite → 1.
pub fn clamp_multiplier(v: f64) -> f64 {
    if !v.is_finite() {
        return 1.0;
    }
    v.clamp(0.5, 1.0)
}

pub fn parse_signal(raw: &Value) -> Value {
    let score = clamp_score(raw.get("score").and_then(|v| v.as_i64()).unwrap_or(0));
    let event_risk = raw.get("event_risk").and_then(|v| v.as_bool()).unwrap_or(false);
    let event_type = raw
        .get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("none");
    let gap_risk = raw.get("gap_risk").and_then(|v| v.as_bool()).unwrap_or(false);
    let confidence = raw
        .get("confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let headline_ids = raw.get("headline_ids").cloned().unwrap_or(json!([]));
    json!({
        "score": score,
        "event_risk": event_risk,
        "event_type": event_type,
        "gap_risk": gap_risk,
        "confidence": confidence,
        "headline_ids": headline_ids,
    })
}

pub fn parse_agent_decision(raw: &Value) -> Value {
    let decision = raw.get("decision").and_then(|v| v.as_str()).unwrap_or("approve");
    let decision = if decision.eq_ignore_ascii_case("veto") {
        "veto"
    } else {
        "approve"
    };
    let mult = clamp_multiplier(
        raw.get("size_multiplier")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0),
    );
    let reasons = raw
        .get("reason_codes")
        .cloned()
        .unwrap_or(json!([]));
    json!({
        "agent_decision": decision,
        "size_multiplier": mult,
        "reason_codes": reasons,
    })
}

/// Mask ticker, company name, and date fields so a blinded score cannot
/// recite a memorized path for that name.
pub fn blind_value(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, val) in map {
                let key = k.to_ascii_lowercase();
                if matches!(key.as_str(), "symbol" | "name" | "company" | "ticker") {
                    out.insert(k.clone(), json!("SYMBOL"));
                } else if key.contains("date") || key.contains("earnings") {
                    out.insert(k.clone(), json!("DATE"));
                } else {
                    out.insert(k.clone(), blind_value(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(blind_value).collect()),
        other => other.clone(),
    }
}

/// Apply a decision cache to a scan object. `mode`:
/// `off`/`log` leave candidates unchanged; `skip_event` drops event-risk
/// names; `downsize_event` sets size_multiplier 0.5; `agent_veto` drops
/// vetoes and copies the clamped multiplier; `exit_event` only annotates.
pub fn apply_cached_decisions(rules_path: &Path, rules: &TraderRules, scan: &mut Value) {
    let mode = rules.llm_signal.mode.as_str();
    if matches!(mode, "off" | "" | "log") {
        return;
    }
    let Ok(raw) = fs::read_to_string(cache_path(rules_path, &rules.trader_id)) else {
        return;
    };
    let Ok(cache) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    let Some(symbols) = cache.get("symbols").and_then(|v| v.as_object()) else {
        return;
    };
    let Some(cands) = scan.get_mut("candidates").and_then(|v| v.as_array_mut()) else {
        return;
    };
    cands.retain(|c| {
        let Some(sym) = c.get("symbol").and_then(|v| v.as_str()) else {
            return true;
        };
        let Some(d) = symbols.get(&sym.to_uppercase()).or_else(|| symbols.get(sym)) else {
            return true;
        };
        match mode {
            "skip_event" => !d.get("event_risk").and_then(|v| v.as_bool()).unwrap_or(false),
            "agent_veto" => d.get("agent_decision").and_then(|v| v.as_str()) != Some("veto"),
            _ => true,
        }
    });
    if let Some(cands) = scan.get_mut("candidates").and_then(|v| v.as_array_mut()) {
        for c in cands {
            let Some(sym) = c.get("symbol").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(d) = symbols.get(&sym.to_uppercase()).or_else(|| symbols.get(sym)) else {
                continue;
            };
            let mult = match mode {
                "downsize_event"
                    if d.get("event_risk").and_then(|v| v.as_bool()).unwrap_or(false) =>
                {
                    0.5
                }
                "agent_veto" => d
                    .get("size_multiplier")
                    .and_then(|v| v.as_f64())
                    .map(clamp_multiplier)
                    .unwrap_or(1.0),
                _ => 1.0,
            };
            if let Some(obj) = c.as_object_mut() {
                obj.insert("size_multiplier".into(), json!(mult));
            }
        }
    }
}

pub fn candidate_size_multiplier(scan: &Value, symbol: &str) -> f64 {
    let Some(cands) = scan.get("candidates").and_then(|v| v.as_array()) else {
        return 1.0;
    };
    for c in cands {
        if c.get("symbol").and_then(|v| v.as_str()) == Some(symbol) {
            return c
                .get("size_multiplier")
                .and_then(|v| v.as_f64())
                .map(clamp_multiplier)
                .unwrap_or(1.0);
        }
    }
    1.0
}

pub fn base_rates_from_outcomes(outcomes_path: &Path, rsi: Option<f64>) -> Value {
    let Ok(raw) = fs::read_to_string(outcomes_path) else {
        return json!({"insufficient": true, "n": 0, "reason": "no outcomes file"});
    };
    let bucket = rsi.map(|r| (r / 10.0).floor() as i64);
    let mut n = 0usize;
    let mut sum = 0.0;
    for line in raw.lines() {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(b) = bucket {
            let row_rsi = row.get("rsi_14").and_then(|v| v.as_f64());
            if row_rsi.map(|r| (r / 10.0).floor() as i64) != Some(b) {
                continue;
            }
        }
        if let Some(ret) = row.get("fwd_10d_excess").and_then(|v| v.as_f64()) {
            n += 1;
            sum += ret;
        }
    }
    if n < 50 {
        return json!({"insufficient": true, "n": n, "min_samples": 50});
    }
    json!({"insufficient": false, "n": n, "mean_fwd_10d_excess": sum / n as f64})
}

pub async fn run_signal_batch(rules_path: &Path) -> Result<Value> {
    let rules = TraderRules::load(rules_path)?;
    if !rules.llm_signal.enabled {
        return Ok(json!({"status": "disabled", "reason": "llm_signal.enabled is false"}));
    }
    let Some(client) = openrouter_optional()? else {
        return Ok(json!({"status": "skipped", "reason": "OPENROUTER_API_KEY unset"}));
    };
    let mut bundles = bundles_from_journal(&journal_path(rules_path), rules.llm_signal.max_symbols_per_day);
    attach_headlines(&mut bundles).await;
    let mut symbols = Map::new();
    let cfg = &rules.llm_signal;
    let samples = cfg.samples.max(1);
    for (sym, bundle) in bundles {
        let named = score_samples(&client, &cfg.model, &bundle, samples, cfg.temperature).await?;
        let blinded = if cfg.blinded {
            let blind = blind_value(&bundle);
            Some(score_samples(&client, &cfg.model, &blind, 1, cfg.temperature).await?)
        } else {
            None
        };
        let compare = if cfg.compare_model.trim().is_empty() {
            None
        } else {
            Some(score_samples(&client, &cfg.compare_model, &bundle, 1, cfg.temperature).await?)
        };
        let mut row = named;
        if let Some(b) = blinded {
            row["blinded_score"] = b["score"].clone();
            row["blinded_input_hash"] = b["input_hash"].clone();
        }
        if let Some(c) = compare {
            row["compare_model"] = json!(cfg.compare_model);
            row["compare_score"] = c["score"].clone();
        }
        symbols.insert(sym, row);
    }
    let reviews = position_reviews(&state_path(rules_path), &symbols);
    let doc = json!({
        "date": Utc::now().date_naive().to_string(),
        "trader_id": rules.trader_id,
        "model": cfg.model,
        "knowledge_cutoff": cfg.knowledge_cutoff,
        "prompt_hash": prompt_hash(),
        "samples": samples,
        "symbols": symbols,
        "position_reviews": reviews,
    });
    let path = cache_path(rules_path, &rules.trader_id);
    fs::write(&path, serde_json::to_string_pretty(&doc)?)
        .with_context(|| format!("write {}", path.display()))?;
    let jpath = journal_signal_path(rules_path, &rules.trader_id);
    let mut line = serde_json::to_string(&doc)?;
    line.push('\n');
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&jpath)?;
    f.write_all(line.as_bytes())?;
    Ok(json!({"status": "ok", "symbols": doc["symbols"].as_object().map(|m| m.len()).unwrap_or(0), "cache": path.display().to_string()}))
}

pub async fn run_agent_batch(rules_path: &Path) -> Result<Value> {
    let rules = TraderRules::load(rules_path)?;
    if !rules.llm_signal.enabled {
        return Ok(json!({"status": "disabled"}));
    }
    let Some(client) = openrouter_optional()? else {
        return Ok(json!({"status": "skipped", "reason": "OPENROUTER_API_KEY unset"}));
    };
    let path = cache_path(rules_path, &rules.trader_id);
    let mut doc: Value = fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({"symbols": {}}));
    let journal = journal_path(rules_path);
    let bundles = bundles_from_journal(&journal, rules.llm_signal.max_symbols_per_day);
    let admitted: Vec<(String, Value)> = bundles
        .into_iter()
        .filter(|(_, b)| b.get("admitted").and_then(|v| v.as_bool()).unwrap_or(false))
        .collect();
    let outcomes = rules_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("analysis/candidate-outcomes.jsonl");
    let state = TraderState::load(&state_path(rules_path), &rules.trader_id).unwrap_or_default();
    let tools = analyst_tools();
    let mut budget = ToolBudget {
        max_turns: 6,
        max_tokens: 8000,
        max_usd: 0.25,
        tokens_used: 0,
        usd_used: 0.0,
        turns: 0,
    };
    let mut n = 0;
    for (sym, bundle) in admitted {
        if budget.exhausted() {
            break;
        }
        let decision = analyst_decision(
            &client,
            &rules.llm_signal.model,
            &sym,
            &bundle,
            &outcomes,
            &state,
            &tools,
            &mut budget,
        )
        .await?;
        if let Some(obj) = doc
            .get_mut("symbols")
            .and_then(|v| v.as_object_mut())
        {
            let row = obj.entry(sym.clone()).or_insert_with(|| json!({}));
            if let Some(row) = row.as_object_mut() {
                for (k, v) in decision.as_object().cloned().unwrap_or_default() {
                    row.insert(k, v);
                }
            }
        }
        n += 1;
    }
    doc["agent_turns"] = json!(budget.turns);
    doc["agent_tokens"] = json!(budget.tokens_used);
    fs::write(&path, serde_json::to_string_pretty(&doc)?)?;
    Ok(json!({"status": "ok", "decisions": n, "turns": budget.turns}))
}

async fn analyst_decision(
    client: &OpenRouterClient,
    model: &str,
    symbol: &str,
    bundle: &Value,
    outcomes: &Path,
    state: &TraderState,
    tools: &Value,
    budget: &mut ToolBudget,
) -> Result<Value> {
    let mut messages = vec![
        json!({"role": "system", "content": AGENT_PROMPT}),
        json!({"role": "user", "content": format!("Admitted symbol {symbol}. Use tools, then return the JSON decision.")}),
    ];
    let mut transcript = Vec::new();
    loop {
        if budget.exhausted() {
            break;
        }
        let turn: ChatTurn = client
            .chat(model, &messages, Some(tools), 800, 0.0)
            .await?;
        budget.turns += 1;
        budget.tokens_used += turn.prompt_tokens + turn.completion_tokens;
        budget.usd_used += tokens_to_usd(turn.prompt_tokens + turn.completion_tokens);
        let calls = tool_calls_of(&turn.message);
        transcript.push(json!({"turn": budget.turns, "tool_calls": calls.len()}));
        if calls.is_empty() {
            let content = turn
                .message
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let parsed = extract_json_object(content).unwrap_or(json!({}));
            let mut decision = parse_agent_decision(&parsed);
            decision["transcript"] = json!(transcript);
            return Ok(decision);
        }
        let mut results = Vec::new();
        for (id, name, args) in &calls {
            let result = dispatch_tool(name, args, symbol, bundle, outcomes, state);
            results.push((id.clone(), result));
        }
        push_tool_round(&mut messages, &turn.message, &results);
    }
    Ok(json!({
        "agent_decision": "approve",
        "size_multiplier": 1.0,
        "reason_codes": ["budget_exhausted"],
        "transcript": transcript,
    }))
}

fn dispatch_tool(
    name: &str,
    args: &Value,
    symbol: &str,
    bundle: &Value,
    outcomes: &Path,
    state: &TraderState,
) -> Value {
    let sym = args
        .get("symbol")
        .and_then(|v| v.as_str())
        .unwrap_or(symbol)
        .to_uppercase();
    match name {
        "get_snapshot" => bundle.clone(),
        "get_headlines" | "get_filings_summary" => bundle
            .get("headlines")
            .cloned()
            .unwrap_or(json!([])),
        "get_peer_rs" => bundle
            .pointer("/technical_context/history_features")
            .cloned()
            .unwrap_or(json!({"symbol": sym})),
        "get_base_rates" => {
            let rsi = bundle
                .pointer("/technical_context/rsi_14")
                .and_then(|v| v.as_f64());
            base_rates_from_outcomes(outcomes, rsi)
        }
        "get_position_state" => {
            let open = state
                .open_positions
                .values()
                .any(|p| p.symbol.eq_ignore_ascii_case(&sym));
            json!({"symbol": sym, "open": open})
        }
        other => json!({"error": format!("unknown tool {other}")}),
    }
}

fn position_reviews(state_file: &Path, symbols: &Map<String, Value>) -> Vec<Value> {
    let Ok(state) = TraderState::load(state_file, "") else {
        return vec![];
    };
    state
        .open_positions
        .values()
        .filter_map(|p| {
            let d = symbols.get(&p.symbol.to_uppercase())?;
            if d.get("event_risk").and_then(|v| v.as_bool()).unwrap_or(false) {
                Some(json!({
                    "symbol": p.symbol,
                    "recommendation": "exit_before_event",
                    "event_type": d.get("event_type").cloned().unwrap_or(json!("none")),
                }))
            } else {
                None
            }
        })
        .collect()
}

async fn score_samples(
    client: &OpenRouterClient,
    model: &str,
    bundle: &Value,
    samples: u32,
    temperature: f64,
) -> Result<Value> {
    let user = serde_json::to_string(bundle)?;
    let input_hash = stable_hash(user.as_bytes());
    let mut scores = Vec::new();
    let mut last = json!({});
    for _ in 0..samples {
        let turn = client
            .chat(
                model,
                &[
                    json!({"role": "system", "content": SIGNAL_PROMPT}),
                    json!({"role": "user", "content": user}),
                ],
                None,
                400,
                temperature,
            )
            .await?;
        let content = turn
            .message
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let parsed = extract_json_object(content).unwrap_or(json!({}));
        last = parse_signal(&parsed);
        scores.push(last["score"].as_i64().unwrap_or(0));
    }
    last["score"] = json!(median_score(&scores));
    last["input_hash"] = json!(input_hash);
    last["model"] = json!(model);
    last["n_samples"] = json!(samples);
    Ok(last)
}

async fn attach_headlines(bundles: &mut BTreeMap<String, Value>) {
    let Ok(client) = crate::fmp::FmpClient::from_env() else {
        return;
    };
    for (sym, bundle) in bundles.iter_mut().take(40) {
        if let Ok(rows) = client.stock_headlines(sym).await {
            if let Some(obj) = bundle.as_object_mut() {
                obj.insert("headlines".into(), json!(rows));
            }
        }
    }
}

fn openrouter_optional() -> Result<Option<OpenRouterClient>> {
    match std::env::var("OPENROUTER_API_KEY") {
        Ok(k) if !k.trim().is_empty() => Ok(Some(OpenRouterClient::from_env()?)),
        _ => Ok(None),
    }
}

/// Last scan in the swing journal: admitted candidates plus rejected rows
/// that carry a technical snapshot. Deduped by symbol, capped.
pub fn bundles_from_journal(journal: &Path, max_symbols: u32) -> BTreeMap<String, Value> {
    let Some(scan) = tail_scan(journal) else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    if let Some(cands) = scan.get("candidates").and_then(|v| v.as_array()) {
        for c in cands {
            push_bundle(&mut out, c, true, max_symbols);
        }
    }
    if let Some(rej) = scan.get("rejected").and_then(|v| v.as_array()) {
        for r in rej {
            if r.get("reason_code").and_then(|v| v.as_str()) == Some("data_unavailable") {
                continue;
            }
            push_bundle(&mut out, r, false, max_symbols);
        }
    }
    out
}

fn push_bundle(out: &mut BTreeMap<String, Value>, row: &Value, admitted: bool, max_symbols: u32) {
    if out.len() >= max_symbols as usize {
        return;
    }
    let Some(sym) = row.get("symbol").and_then(|v| v.as_str()) else {
        return;
    };
    let sym = sym.to_uppercase();
    if out.contains_key(&sym) {
        return;
    }
    let tech = row.get("technical_context").cloned().unwrap_or(json!({}));
    if !admitted && tech.is_null() {
        return;
    }
    out.insert(
        sym.clone(),
        json!({
            "symbol": sym,
            "admitted": admitted,
            "reason_code": row.get("reason_code").cloned().unwrap_or(json!(null)),
            "technical_context": tech,
            "headlines": [],
        }),
    );
}

fn tail_scan(journal: &Path) -> Option<Value> {
    let mut f = fs::File::open(journal).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(2_000_000);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;
    let mut found = None;
    for line in buf.lines() {
        if !line.contains("sim_tick_summary") {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if let Some(scan) = v.pointer("/payload/scan") {
                found = Some(scan.clone());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_and_clamps() {
        assert_eq!(median_score(&[1, -2, 1]), 1);
        assert_eq!(median_score(&[]), 0);
        assert_eq!(clamp_score(9), 2);
        assert_eq!(clamp_multiplier(4.0), 1.0);
        assert_eq!(clamp_multiplier(0.1), 0.5);
        assert_eq!(clamp_multiplier(f64::NAN), 1.0);
    }

    #[test]
    fn blind_masks_identity_and_dates() {
        let v = json!({"symbol": "AAPL", "technical_context": {"rsi_14": 55.0, "last_earnings_date": "2026-01-01"}});
        let b = blind_value(&v);
        assert_eq!(b["symbol"], json!("SYMBOL"));
        assert_eq!(b["technical_context"]["rsi_14"], json!(55.0));
        assert_eq!(b["technical_context"]["last_earnings_date"], json!("DATE"));
    }

    #[test]
    fn skip_event_drops_only_flagged_names() {
        let dir = std::env::temp_dir().join(format!("llm-signal-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let rules_path = dir.join("t.yaml");
        fs::write(
            cache_path(&rules_path, "swing"),
            r#"{"symbols":{"AAPL":{"event_risk":true,"score":-1},"MSFT":{"event_risk":false,"score":1}}}"#,
        )
        .unwrap();
        let mut rules = TraderRules::default();
        rules.trader_id = "swing".into();
        rules.llm_signal.mode = "skip_event".into();
        let mut scan = json!({"candidates":[{"symbol":"AAPL"},{"symbol":"MSFT"}]});
        apply_cached_decisions(&rules_path, &rules, &mut scan);
        let syms: Vec<&str> = scan["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["symbol"].as_str())
            .collect();
        assert_eq!(syms, vec!["MSFT"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_veto_clamps_multiplier_and_drops_veto() {
        let dir = std::env::temp_dir().join(format!("llm-agent-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let rules_path = dir.join("t.yaml");
        fs::write(
            cache_path(&rules_path, "swing"),
            r#"{"symbols":{"AAPL":{"agent_decision":"veto","size_multiplier":0.2},"MSFT":{"agent_decision":"approve","size_multiplier":3.0}}}"#,
        )
        .unwrap();
        let mut rules = TraderRules::default();
        rules.trader_id = "swing".into();
        rules.llm_signal.mode = "agent_veto".into();
        let mut scan = json!({"candidates":[{"symbol":"AAPL"},{"symbol":"MSFT"}]});
        apply_cached_decisions(&rules_path, &rules, &mut scan);
        assert_eq!(scan["candidates"][0]["symbol"], json!("MSFT"));
        assert_eq!(scan["candidates"][0]["size_multiplier"], json!(1.0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn base_rates_refuse_small_samples() {
        let path = std::env::temp_dir().join(format!("outcomes-{}.jsonl", std::process::id()));
        fs::write(&path, "{\"rsi_14\":55,\"fwd_10d_excess\":0.01}\n").unwrap();
        let v = base_rates_from_outcomes(&path, Some(55.0));
        assert_eq!(v["insufficient"], json!(true));
        assert_eq!(v["n"], json!(1));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn tool_round_appends_results() {
        let mut messages = vec![json!({"role":"user","content":"hi"})];
        let assistant = json!({"role":"assistant","tool_calls":[{"id":"1","function":{"name":"get_snapshot","arguments":"{\"symbol\":\"AAPL\"}"}}]});
        let calls = tool_calls_of(&assistant);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, "get_snapshot");
        push_tool_round(&mut messages, &assistant, &[("1".into(), json!({"rsi": 50}))]);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["role"], json!("tool"));
    }

    #[test]
    fn parse_signal_clamps_score() {
        let v = parse_signal(&json!({"score": 9, "event_risk": true, "confidence": 2.0}));
        assert_eq!(v["score"], json!(2));
        assert_eq!(v["confidence"], json!(1.0));
    }
}
