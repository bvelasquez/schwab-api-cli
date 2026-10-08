//! Bounded multi-turn analyst. Tools only read the point-in-time bundle the
//! batch already assembled. The agent can veto or downsize a candidate the
//! rules already admitted. It cannot originate a trade, widen a stop, or
//! raise size above 1.0.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::llm_signal::{clamp_size_multiplier, SignalDecision};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub struct ModelReply {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentDecision {
    pub action: String,
    pub size_multiplier: f64,
    pub reason_codes: Vec<String>,
    pub turns: u32,
    pub transcript: Vec<Value>,
}

pub fn tool_schemas() -> Value {
    json!([
        {"type": "function", "function": {"name": "get_snapshot", "description": "Technical snapshot already in the bundle", "parameters": {"type": "object", "properties": {}}}},
        {"type": "function", "function": {"name": "get_headlines", "description": "Headlines already fetched for this symbol and day", "parameters": {"type": "object", "properties": {}}}},
        {"type": "function", "function": {"name": "get_filings_summary", "description": "Short filing notes already in the bundle, if any", "parameters": {"type": "object", "properties": {}}}},
        {"type": "function", "function": {"name": "get_peer_rs", "description": "Relative strength vs the benchmark", "parameters": {"type": "object", "properties": {}}}},
        {"type": "function", "function": {"name": "get_base_rates", "description": "Code-computed historical outcomes for similar setups. Refuses below 50 samples.", "parameters": {"type": "object", "properties": {}}}},
        {"type": "function", "function": {"name": "get_position_state", "description": "Whether this symbol is already open", "parameters": {"type": "object", "properties": {}}}}
    ])
}

pub fn execute_tool(name: &str, bundle: &Value) -> Value {
    match name {
        "get_snapshot" => bundle.get("snapshot").cloned().unwrap_or(json!({})),
        "get_headlines" => bundle.get("headlines").cloned().unwrap_or(json!([])),
        "get_filings_summary" => bundle.get("filings").cloned().unwrap_or(json!({})),
        "get_peer_rs" => bundle.get("peer_rs").cloned().unwrap_or(json!({})),
        "get_base_rates" => base_rates(bundle),
        "get_position_state" => bundle
            .get("position")
            .cloned()
            .unwrap_or(json!({"open": false})),
        other => json!({"error": format!("unknown tool {other}")}),
    }
}

fn base_rates(bundle: &Value) -> Value {
    let n = bundle
        .get("base_rates")
        .and_then(|v| v.get("n"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if n < 50 {
        return json!({"status": "insufficient_samples", "n": n, "min_n": 50});
    }
    bundle.get("base_rates").cloned().unwrap_or(json!({}))
}

/// One bull/bear round is two scripted turns inside `max_turns`. The model
/// never gets a tool that places an order.
pub fn analyst_system_prompt() -> &'static str {
    "You are the analyst for a paper swing sleeve. You may call tools, then argue one bull case and one bear case, then stop. \
The risk check is code: you can only approve or veto a candidate the rules already admitted, and size_multiplier must be between 0.5 and 1.0. \
Do not invent a new trade. Final message is JSON: {\"action\":\"approve|veto\",\"size_multiplier\":number,\"reason_codes\":[string],\"bull\":\"\",\"bear\":\"\"}"
}

pub fn parse_agent_json(raw: &str) -> Result<AgentDecision> {
    let start = raw.find('{').context("no json object")?;
    let end = raw.rfind('}').context("no json object")?;
    let v: Value = serde_json::from_str(&raw[start..=end])?;
    let action = v
        .get("action")
        .and_then(|a| a.as_str())
        .unwrap_or("veto")
        .to_string();
    let action = if action == "approve" {
        "approve"
    } else {
        "veto"
    };
    let size = v
        .get("size_multiplier")
        .and_then(|s| s.as_f64())
        .unwrap_or(1.0);
    let reason_codes = v
        .get("reason_codes")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(AgentDecision {
        action: action.into(),
        size_multiplier: clamp_size_multiplier(size),
        reason_codes,
        turns: 0,
        transcript: vec![],
    })
}

/// Drive a tool loop against a scripted or live model. Stops on a final
/// JSON decision, on `max_turns`, or when estimated dollars exceed the budget.
pub fn drive_turns<F>(
    bundle: &Value,
    max_turns: u32,
    dollar_budget: f64,
    mut step: F,
) -> Result<AgentDecision>
where
    F: FnMut(&[Value]) -> Result<ModelReply>,
{
    let mut transcript = vec![json!({
        "role": "system",
        "content": analyst_system_prompt()
    })];
    transcript.push(json!({"role": "user", "content": bundle}));
    let mut spent = 0.0;
    let mut turns = 0u32;
    let mut last_content = String::new();
    while turns < max_turns.max(1) {
        if spent > dollar_budget {
            break;
        }
        let reply = step(&transcript)?;
        turns += 1;
        spent += estimate_dollars(reply.prompt_tokens, reply.completion_tokens);
        transcript.push(json!({
            "role": "assistant",
            "content": reply.content,
            "tool_calls": reply.tool_calls.iter().map(|c| json!({
                "id": c.id,
                "name": c.name,
                "arguments": c.arguments
            })).collect::<Vec<_>>()
        }));
        if reply.tool_calls.is_empty() {
            last_content = reply.content;
            break;
        }
        for call in reply.tool_calls {
            let result = execute_tool(&call.name, bundle);
            transcript.push(json!({
                "role": "tool",
                "tool_call_id": call.id,
                "name": call.name,
                "content": result
            }));
        }
        // After the first tool round, ask for the bull/bear close if the
        // model has not finished. The next step() sees the tool results.
        if turns == 1 {
            transcript.push(json!({
                "role": "user",
                "content": "One bull case and one bear case, then the final JSON. No more tools."
            }));
        }
    }
    let mut decision = if last_content.is_empty() {
        AgentDecision {
            action: "veto".into(),
            size_multiplier: 0.5,
            reason_codes: vec!["agent_budget_or_turns_exhausted".into()],
            turns,
            transcript,
        }
    } else {
        let mut d = parse_agent_json(&last_content).unwrap_or(AgentDecision {
            action: "veto".into(),
            size_multiplier: 0.5,
            reason_codes: vec!["agent_unparseable".into()],
            turns,
            transcript: vec![],
        });
        d.turns = turns;
        d.transcript = transcript;
        d
    };
    decision.size_multiplier = clamp_size_multiplier(decision.size_multiplier);
    if decision.action != "approve" {
        decision.action = "veto".into();
    }
    Ok(decision)
}

/// Flash-class price used only to enforce the per-decision dollar budget.
/// It is an upper bound for accounting, not an invoice.
pub fn estimate_dollars(prompt_tokens: u32, completion_tokens: u32) -> f64 {
    (prompt_tokens as f64) * 0.15 / 1_000_000.0 + (completion_tokens as f64) * 0.60 / 1_000_000.0
}

pub fn decision_row(
    day: &str,
    symbol: &str,
    model: &str,
    cutoff: &str,
    decision: &AgentDecision,
    live: bool,
) -> SignalDecision {
    SignalDecision {
        day: day.to_string(),
        symbol: symbol.trim().to_uppercase(),
        blinded: false,
        model: model.to_string(),
        model_cutoff: cutoff.to_string(),
        prompt_hash: super::llm_signal::hash_hex(analyst_system_prompt().as_bytes()),
        input_hash: String::new(),
        score: if decision.action == "approve" { 1 } else { -1 },
        event_risk: decision.reason_codes.iter().any(|c| c.contains("event")),
        event_type: "agent".into(),
        gap_risk: false,
        confidence: 0.0,
        headline_ids: vec![],
        samples: vec![],
        source: "agent".into(),
        size_multiplier: decision.size_multiplier,
        action: decision.action.clone(),
        reason_codes: decision.reason_codes.clone(),
        live,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_base_rates_refuse_small_samples() {
        let bundle = json!({"base_rates": {"n": 12, "win_rate": 0.9}});
        let v = execute_tool("get_base_rates", &bundle);
        assert_eq!(v["status"], json!("insufficient_samples"));
    }

    #[test]
    fn loop_uses_tools_then_clamps_size_and_cannot_invent_approve_above_one() {
        let bundle = json!({
            "snapshot": {"rsi_14": 55},
            "headlines": [{"id": "h1", "title": "guide cut"}],
            "base_rates": {"n": 80, "mean_excess_5d": 0.002},
            "admitted": true
        });
        let mut n = 0;
        let decision = drive_turns(&bundle, 4, 1.0, |_| {
            n += 1;
            if n == 1 {
                Ok(ModelReply {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "1".into(),
                        name: "get_headlines".into(),
                        arguments: json!({}),
                    }],
                    prompt_tokens: 100,
                    completion_tokens: 20,
                })
            } else {
                Ok(ModelReply {
                    content: r#"{"action":"approve","size_multiplier":3.5,"reason_codes":["event_guidance"],"bull":"trend","bear":"guide"}"#.into(),
                    tool_calls: vec![],
                    prompt_tokens: 100,
                    completion_tokens: 40,
                })
            }
        })
        .unwrap();
        assert_eq!(decision.action, "approve");
        assert!((decision.size_multiplier - 1.0).abs() < 1e-9);
        assert!(decision.turns >= 2);
        assert!(decision.reason_codes.iter().any(|c| c.contains("event")));
    }

    #[test]
    fn exhausted_budget_vetoes() {
        let decision = drive_turns(&json!({}), 3, 0.0, |_| {
            Ok(ModelReply {
                content: "nope".into(),
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "get_snapshot".into(),
                    arguments: json!({}),
                }],
                prompt_tokens: 10_000_000,
                completion_tokens: 10,
            })
        })
        .unwrap();
        assert_eq!(decision.action, "veto");
    }
}
