use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::rules::{LlmConfig, LlmPhase, selection_market_context_guardrails};

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

const RESPONSE_JSON_SCHEMA: &str = r#"
Respond ONLY with valid JSON matching this schema:
{
  "market_commentary": "string",
  "web_insights": ["string"],
  "positions": [{"position_id": "underlying|expiry", "recommendation": "hold|close|watch", "urgency": "low|medium|high", "reasoning": "string"}],
  "new_entries": {"recommendation": "proceed|defer|skip", "reasoning": "string"},
  "risk_alerts": ["string"]
}"#;

#[derive(Debug, Clone, Copy)]
enum LlmResponseFormat {
    JsonSchema,
    JsonObject,
    Plain,
}

fn llm_review_json_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "market_commentary": {
                "type": "string",
                "description": "Brief market / portfolio commentary"
            },
            "web_insights": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional web research bullets (empty array if none)"
            },
            "positions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "position_id": {
                            "type": "string",
                            "description": "Position id, e.g. IWM|2026-07-31"
                        },
                        "recommendation": {
                            "type": "string",
                            "description": "hold (comfortably OTM), watch (elevated delta/near strike), or close (thesis break only)"
                        },
                        "urgency": {
                            "type": "string",
                            "description": "low, medium, or high (high only for imminent assignment/gap through short strike)"
                        },
                        "reasoning": {
                            "type": "string",
                            "description": "Cite market_context: short_delta, short_otm_pct, distance_to_short_strike_usd"
                        }
                    },
                    "required": ["position_id", "recommendation", "urgency", "reasoning"],
                    "additionalProperties": false
                }
            },
            "new_entries": {
                "type": "object",
                "properties": {
                    "recommendation": {
                        "type": "string",
                        "description": "proceed, defer, or skip"
                    },
                    "reasoning": { "type": "string" }
                },
                "required": ["recommendation", "reasoning"],
                "additionalProperties": false
            },
            "risk_alerts": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": [
            "market_commentary",
            "web_insights",
            "positions",
            "new_entries",
            "risk_alerts"
        ],
        "additionalProperties": false
    })
}

fn response_format_for_mode(mode: LlmResponseFormat) -> Option<Value> {
    match mode {
        LlmResponseFormat::JsonSchema => Some(json!({
            "type": "json_schema",
            "json_schema": {
                "name": "agent_review",
                "strict": true,
                "schema": llm_review_json_schema()
            }
        })),
        LlmResponseFormat::JsonObject => Some(json!({ "type": "json_object" })),
        LlmResponseFormat::Plain => None,
    }
}

fn plugins_for_mode(mode: LlmResponseFormat) -> Option<Value> {
    match mode {
        LlmResponseFormat::JsonSchema | LlmResponseFormat::JsonObject => {
            Some(json!([{ "id": "response-healing" }]))
        }
        LlmResponseFormat::Plain => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmReview {
    pub phase: String,
    pub model: String,
    pub used_web: bool,
    pub raw: Value,
    pub market_commentary: String,
    pub web_insights: Vec<String>,
    pub position_reviews: Vec<PositionReview>,
    pub entry_recommendation: String,
    pub entry_reasoning: String,
    pub risk_alerts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionReview {
    pub position_id: String,
    pub recommendation: String,
    pub urgency: String,
    pub reasoning: String,
}

pub struct OpenRouterClient {
    http: reqwest::Client,
    api_key: String,
}

impl OpenRouterClient {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .context("OPENROUTER_API_KEY is required when llm.enabled is true")?;
        Ok(Self {
            http: reqwest::Client::new(),
            api_key,
        })
    }

    pub async fn review(
        &self,
        config: &LlmConfig,
        phase: LlmPhase,
        context: &Value,
        use_web: bool,
    ) -> Result<LlmReview> {
        let model = config.resolve_model(phase, use_web);
        let system = build_system_prompt(config, phase, use_web);
        let user = build_user_message(config, phase, context)?;

        let modes = if use_web {
            vec![LlmResponseFormat::Plain]
        } else {
            vec![LlmResponseFormat::JsonSchema, LlmResponseFormat::JsonObject]
        };

        let mut last_err = None;
        for mode in modes {
            match self
                .review_with_format(model, &system, &user, config.max_tokens, mode)
                .await
            {
                Ok(review) => return parse_llm_review(phase, model, use_web, review),
                Err(err) if use_web || !is_retryable_format_error(&err) => return Err(err),
                Err(err) => last_err = Some(err),
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("LLM review failed")))
    }

    async fn review_with_format(
        &self,
        model: &str,
        system: &str,
        user: &str,
        max_tokens: u32,
        mode: LlmResponseFormat,
    ) -> Result<Value> {
        let mut body = json!({
            "model": model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user }
            ],
            "max_tokens": max_tokens,
        });

        if let Some(response_format) = response_format_for_mode(mode) {
            body["response_format"] = response_format;
        }
        if let Some(plugins) = plugins_for_mode(mode) {
            body["plugins"] = plugins;
        }

        let resp = self
            .http
            .post(OPENROUTER_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://github.com/schwabinvestbot")
            .header("X-Title", "schwabinvestbot options agent")
            .json(&body)
            .send()
            .await
            .context("OpenRouter request failed")?;

        let status = resp.status();
        let payload: Value = resp
            .json()
            .await
            .context("OpenRouter response parse failed")?;
        if !status.is_success() {
            let fallback = payload.to_string();
            let message = payload
                .pointer("/error/message")
                .and_then(|v| v.as_str())
                .unwrap_or(&fallback);
            if status.as_u16() == 401 {
                anyhow::bail!(
                    "OpenRouter error {status}: {message}. \
                     Check OPENROUTER_API_KEY in the project .env (it overrides shell exports). \
                     Create or rotate a key at https://openrouter.ai/keys"
                );
            }
            anyhow::bail!("OpenRouter error {status}: {message}");
        }

        let content = extract_message_content(&payload)?;
        parse_llm_json_content(&content)
    }
}

fn is_retryable_format_error(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("json_schema")
        || msg.contains("structured")
        || msg.contains("response_format")
        || msg.contains("not support")
        || msg.contains("unsupported")
        || msg.contains("invalid parameter")
}

fn extract_message_content(payload: &Value) -> Result<String> {
    let message = payload
        .pointer("/choices/0/message")
        .context("OpenRouter response missing message")?;

    if let Some(text) = message_content_as_str(message.get("content")) {
        if !text.trim().is_empty() {
            return Ok(text);
        }
    }

    if let Some(reasoning) = message_content_as_str(message.get("reasoning")) {
        if !reasoning.trim().is_empty() {
            return Ok(reasoning);
        }
    }

    if let Some(refusal) = message.get("refusal").and_then(|v| v.as_str()) {
        if !refusal.trim().is_empty() {
            anyhow::bail!("LLM refused request: {refusal}");
        }
    }

    anyhow::bail!(
        "OpenRouter response missing usable content (model may not support structured output for this route)"
    )
}

fn message_content_as_str(content: Option<&Value>) -> Option<String> {
    let content = content?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    if let Some(parts) = content.as_array() {
        let mut out = String::new();
        for part in parts {
            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                out.push_str(text);
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    } else {
        None
    }
}

fn parse_llm_json_content(content: &str) -> Result<Value> {
    serde_json::from_str(content.trim())
        .or_else(|_| extract_json_object(content))
        .with_context(|| format_llm_parse_error(content))
}

fn format_llm_parse_error(content: &str) -> String {
    let preview: String = content.chars().take(240).collect();
    let suffix = if content.chars().count() > 240 {
        "…"
    } else {
        ""
    };
    format!("LLM returned non-JSON content: {preview}{suffix}")
}

pub fn build_system_prompt(config: &LlmConfig, phase: LlmPhase, use_web: bool) -> String {
    let instructions = match phase {
        LlmPhase::Selection => config.prompts.effective_selection_instructions(use_web),
        LlmPhase::Monitor => config.prompts.effective_monitor_instructions(),
        LlmPhase::OvernightDigest => config.prompts.effective_overnight_instructions(),
    };
    let mut prompt = format!("{instructions}\n{RESPONSE_JSON_SCHEMA}");
    if matches!(phase, LlmPhase::Selection) {
        prompt.push_str("\n\n");
        prompt.push_str(selection_market_context_guardrails());
    }
    prompt
}

pub fn build_user_message(config: &LlmConfig, phase: LlmPhase, context: &Value) -> Result<String> {
    let strategy_context = config.prompts.effective_context(phase);
    let context_json = serde_json::to_string_pretty(context)?;
    if strategy_context.trim().is_empty() {
        Ok(format!(
            "Review this options agent state and advise. Context JSON:\n{context_json}"
        ))
    } else {
        Ok(format!(
            "Strategy context:\n{strategy_context}\n\nReview this options agent state and advise. Context JSON:\n{context_json}"
        ))
    }
}

fn parse_llm_review(
    phase: LlmPhase,
    model: &str,
    used_web: bool,
    parsed: Value,
) -> Result<LlmReview> {
    let market_commentary = required_string(&parsed, "market_commentary")?;
    let entry_recommendation = parsed
        .pointer("/new_entries/recommendation")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| matches!(s.as_str(), "proceed" | "defer" | "skip"))
        .context("LLM review missing valid new_entries.recommendation")?;
    let entry_reasoning = parsed
        .pointer("/new_entries/reasoning")
        .and_then(|v| v.as_str())
        .context("LLM review missing new_entries.reasoning")?
        .to_string();

    let position_reviews = parsed
        .get("positions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    Some(PositionReview {
                        position_id: p.get("position_id")?.as_str()?.to_string(),
                        recommendation: p
                            .get("recommendation")
                            .and_then(|v| v.as_str())
                            .unwrap_or("hold")
                            .to_string(),
                        urgency: p
                            .get("urgency")
                            .and_then(|v| v.as_str())
                            .unwrap_or("low")
                            .to_string(),
                        reasoning: p
                            .get("reasoning")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(LlmReview {
        phase: phase_label(phase).to_string(),
        model: model.to_string(),
        used_web,
        market_commentary,
        web_insights: parsed
            .get("web_insights")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        entry_recommendation,
        entry_reasoning,
        risk_alerts: parsed
            .get("risk_alerts")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        position_reviews,
        raw: parsed,
    })
}

fn required_string(parsed: &Value, key: &str) -> Result<String> {
    parsed
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .with_context(|| format!("LLM review missing {key}"))
}

fn phase_label(phase: LlmPhase) -> &'static str {
    match phase {
        LlmPhase::Selection => "selection",
        LlmPhase::Monitor => "monitor",
        LlmPhase::OvernightDigest => "overnight_digest",
    }
}

impl LlmReview {
    pub fn to_json(&self) -> Value {
        json!({
            "phase": self.phase,
            "model": self.model,
            "used_web": self.used_web,
            "market_commentary": self.market_commentary,
            "web_insights": self.web_insights,
            "positions": self.position_reviews,
            "new_entries": {
                "recommendation": self.entry_recommendation,
                "reasoning": self.entry_reasoning,
            },
            "risk_alerts": self.risk_alerts,
        })
    }

    pub fn should_veto_entries(&self) -> bool {
        matches!(
            self.entry_recommendation.as_str(),
            "skip" | "defer" | "hold"
        )
    }

    pub fn urgent_close_positions(&self) -> Vec<&PositionReview> {
        self.position_reviews
            .iter()
            .filter(|p| {
                p.recommendation.eq_ignore_ascii_case("close")
                    && p.urgency.eq_ignore_ascii_case("high")
            })
            .collect()
    }
}

/// Extract JSON object from markdown fences or leading prose (web models).
pub fn extract_json_object(content: &str) -> Result<Value> {
    let trimmed = content.trim();
    if let Ok(v) = serde_json::from_str(trimmed) {
        return Ok(v);
    }

    for fence in ["```json", "```JSON", "```"] {
        if let Some(start) = trimmed.find(fence) {
            let after = &trimmed[start + fence.len()..];
            if let Some(end) = after.find("```") {
                let block = after[..end].trim();
                if let Ok(v) = serde_json::from_str(block) {
                    return Ok(v);
                }
                if let Ok(v) = extract_json_object(block) {
                    return Ok(v);
                }
            }
        }
    }

    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end > start {
                return Ok(serde_json::from_str(&trimmed[start..=end])?);
            }
        }
    }
    anyhow::bail!("no JSON object found in LLM response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::LlmPromptsConfig;

    #[test]
    fn parses_llm_json() {
        let raw = json!({
            "market_commentary": "Markets calm",
            "web_insights": ["VIX low"],
            "positions": [{
                "position_id": "IWM|2026-07-24",
                "recommendation": "hold",
                "urgency": "low",
                "reasoning": "On track"
            }],
            "new_entries": { "recommendation": "proceed", "reasoning": "ok" },
            "risk_alerts": []
        });
        let review = parse_llm_review(LlmPhase::Selection, "test", true, raw).unwrap();
        assert_eq!(review.phase, "selection");
        assert_eq!(review.entry_recommendation, "proceed");
        assert_eq!(review.position_reviews.len(), 1);
    }

    #[test]
    fn system_prompt_uses_configured_selection_instructions() {
        let mut config = LlmConfig::default();
        config.prompts.selection = "YOLO aggressive trader.".into();
        let prompt = build_system_prompt(&config, LlmPhase::Selection, false);
        assert!(prompt.contains("YOLO aggressive trader."));
        assert!(prompt.contains("valid JSON"));
    }

    #[test]
    fn user_message_includes_strategy_context() {
        let mut config = LlmConfig::default();
        config.prompts.selection_context = "Account 9947: conservative income pilot.".into();
        let msg = build_user_message(&config, LlmPhase::Selection, &json!({"tick": 1})).unwrap();
        assert!(msg.contains("Account 9947"));
        assert!(msg.contains("\"tick\": 1"));
    }

    #[test]
    fn empty_context_omits_strategy_block() {
        let config = LlmConfig {
            prompts: LlmPromptsConfig {
                selection_context: String::new(),
                ..Default::default()
            },
            ..Default::default()
        };
        let msg = build_user_message(&config, LlmPhase::Selection, &json!({})).unwrap();
        assert!(!msg.contains("Strategy context:"));
    }

    #[test]
    fn extracts_json_from_markdown_fence() {
        let raw = r#"Here is the review:
```json
{"market_commentary":"ok","web_insights":[],"positions":[],"new_entries":{"recommendation":"proceed","reasoning":"fine"},"risk_alerts":[]}
```"#;
        let parsed = parse_llm_json_content(raw).unwrap();
        assert_eq!(
            parsed
                .pointer("/new_entries/recommendation")
                .and_then(|v| v.as_str()),
            Some("proceed")
        );
    }

    #[test]
    fn llm_review_schema_has_required_fields() {
        let schema = llm_review_json_schema();
        let required = schema
            .get("required")
            .and_then(|v| v.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("positions")));
        assert!(required.iter().any(|v| v.as_str() == Some("new_entries")));
    }

    #[test]
    fn missing_entry_recommendation_is_rejected() {
        let raw = json!({
            "market_commentary": "",
            "web_insights": [],
            "positions": [],
            "new_entries": { "reasoning": "" },
            "risk_alerts": []
        });
        let err = parse_llm_review(LlmPhase::Selection, "test", true, raw).unwrap_err();
        assert!(err.to_string().contains("new_entries.recommendation"));
    }

    #[test]
    fn selection_system_prompt_includes_chain_guardrails() {
        let config = LlmConfig::default();
        let prompt = build_system_prompt(&config, LlmPhase::Selection, false);
        assert!(prompt.contains("CHAIN DATA GUARDRAILS"));
        assert!(prompt.contains("FORBIDDEN"));
    }

    #[test]
    fn monitor_system_prompt_omits_chain_guardrails() {
        let config = LlmConfig::default();
        let prompt = build_system_prompt(&config, LlmPhase::Monitor, false);
        assert!(!prompt.contains("CHAIN DATA GUARDRAILS"));
    }

    #[test]
    fn empty_json_is_not_a_proceed_review() {
        let err = parse_llm_review(LlmPhase::Selection, "test", true, json!({})).unwrap_err();
        assert!(err.to_string().contains("market_commentary"));
    }
}
