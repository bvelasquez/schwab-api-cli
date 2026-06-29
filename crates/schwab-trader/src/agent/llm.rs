use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::rules::LlmConfig;

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

const RESPONSE_JSON_SCHEMA: &str = r#"
Respond ONLY with valid JSON matching this schema:
{
  "market_commentary": "string",
  "web_insights": ["string"],
  "candidates": [{"symbol": "string", "recommendation": "proceed|defer|skip", "reasoning": "string"}],
  "positions": [{"position_id": "string", "recommendation": "hold|watch|tighten_exits|widen_exits", "urgency": "low|medium|high", "reasoning": "string"}],
  "new_entries": {"recommendation": "proceed|defer|skip", "reasoning": "string"},
  "risk_alerts": ["string"],
  "rule_patches": []
}"#;

#[derive(Debug, Clone, Copy)]
enum LlmResponseFormat {
    JsonSchema,
    JsonObject,
    Plain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraderLlmReview {
    pub phase: String,
    pub model: String,
    pub raw: Value,
    pub market_commentary: String,
    pub web_insights: Vec<String>,
    pub candidates: Vec<CandidateReview>,
    pub positions: Vec<PositionReview>,
    pub entry_recommendation: String,
    pub entry_reasoning: String,
    pub risk_alerts: Vec<String>,
    pub rule_patches: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateReview {
    pub symbol: String,
    pub recommendation: String,
    pub reasoning: String,
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
            .context("OPENROUTER_API_KEY required when llm.enabled is true")?;
        Ok(Self {
            http: reqwest::Client::new(),
            api_key,
        })
    }

    pub async fn review(
        &self,
        config: &LlmConfig,
        phase: &str,
        model: &str,
        context: &Value,
        use_web: bool,
    ) -> Result<TraderLlmReview> {
        let has_source_feeds = context.get("source_feeds").is_some();
        let system = build_system_prompt(config, phase, use_web, has_source_feeds);
        let user = serde_json::to_string_pretty(context)?;

        // Perplexity via OpenRouter rejects response_format json_object; use plain text + JSON prompt.
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
                Ok(parsed) => return Ok(parse_review(phase, model, parsed)),
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
            .header("X-Title", "schwab-trader")
            .json(&body)
            .send()
            .await
            .context("OpenRouter request failed")?;

        let status = resp.status();
        let payload: Value = resp.json().await?;
        if !status.is_success() {
            let fallback = payload.to_string();
            let message = payload
                .pointer("/error/message")
                .and_then(|v| v.as_str())
                .unwrap_or(&fallback);
            anyhow::bail!("OpenRouter error {status}: {message}");
        }

        let content = extract_message_content(&payload)?;
        parse_llm_json_content(&content)
    }
}

fn build_system_prompt(
    config: &LlmConfig,
    phase: &str,
    use_web: bool,
    has_source_feeds: bool,
) -> String {
    let instructions = if use_web {
        &config.prompts.selection_web
    } else if phase == "learn" {
        &config.prompts.learn
    } else if phase == "monitor" {
        &config.prompts.monitor
    } else if phase == "premarket_digest" || phase == "overnight_digest" {
        &config.prompts.selection_web
    } else {
        &config.prompts.selection
    };
    let patch_help = if phase == "learn" {
        "\n\nFor rule_patches use: [{\"path\":\"playbook.exit.profit_target_pct\",\"value\":8.5,\"reason\":\"...\"}]. Only patch fields listed in allowed_patch_paths in the user context."
    } else {
        ""
    };
    let feed_help = if has_source_feeds {
        "\n\nThe user message includes source_feeds with pre-fetched content from configured URLs/APIs/RSS feeds. Treat that as primary ground truth. Cite the feed id in web_insights and candidate reasoning."
    } else {
        ""
    };
    let heat_help = if matches!(phase, "selection" | "web") {
        "\n\ncapital_check includes portfolio_heat_pct, heat_headroom_pct, and heat_ceiling_pct. \
         Do not approve entries that would push portfolio_heat_pct near or above heat_ceiling_pct."
    } else {
        ""
    };
    format!("{instructions}{patch_help}{feed_help}{heat_help}\n{RESPONSE_JSON_SCHEMA}")
}

fn llm_review_json_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "market_commentary": { "type": "string" },
            "web_insights": {
                "type": "array",
                "items": { "type": "string" }
            },
            "candidates": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "symbol": { "type": "string" },
                        "recommendation": { "type": "string" },
                        "reasoning": { "type": "string" }
                    },
                    "required": ["symbol", "recommendation", "reasoning"],
                    "additionalProperties": false
                }
            },
            "positions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "position_id": { "type": "string" },
                        "recommendation": { "type": "string" },
                        "urgency": { "type": "string" },
                        "reasoning": { "type": "string" }
                    },
                    "required": ["position_id", "recommendation", "urgency", "reasoning"],
                    "additionalProperties": false
                }
            },
            "new_entries": {
                "type": "object",
                "properties": {
                    "recommendation": { "type": "string" },
                    "reasoning": { "type": "string" }
                },
                "required": ["recommendation", "reasoning"],
                "additionalProperties": false
            },
            "risk_alerts": {
                "type": "array",
                "items": { "type": "string" }
            },
            "rule_patches": {
                "type": "array",
                "items": { "type": "object" }
            }
        },
        "required": [
            "market_commentary",
            "web_insights",
            "candidates",
            "positions",
            "new_entries",
            "risk_alerts",
            "rule_patches"
        ],
        "additionalProperties": false
    })
}

fn response_format_for_mode(mode: LlmResponseFormat) -> Option<Value> {
    match mode {
        LlmResponseFormat::JsonSchema => Some(json!({
            "type": "json_schema",
            "json_schema": {
                "name": "trader_review",
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

    anyhow::bail!("OpenRouter response missing usable content")
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

fn parse_review(phase: &str, model: &str, raw: Value) -> TraderLlmReview {
    let candidates = raw
        .get("candidates")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    Some(CandidateReview {
                        symbol: c.get("symbol")?.as_str()?.to_string(),
                        recommendation: c
                            .get("recommendation")
                            .and_then(|v| v.as_str())
                            .unwrap_or("defer")
                            .to_string(),
                        reasoning: c
                            .get("reasoning")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let positions = raw
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

    let new_entries = raw.get("new_entries").cloned().unwrap_or(json!({}));
    TraderLlmReview {
        phase: phase.into(),
        model: model.into(),
        raw: raw.clone(),
        market_commentary: raw
            .get("market_commentary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        web_insights: raw
            .get("web_insights")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        candidates,
        positions,
        entry_recommendation: new_entries
            .get("recommendation")
            .and_then(|v| v.as_str())
            .unwrap_or("defer")
            .to_string(),
        entry_reasoning: new_entries
            .get("reasoning")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        risk_alerts: raw
            .get("risk_alerts")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        rule_patches: raw
            .get("rule_patches")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default(),
    }
}

pub fn candidate_approved(review: &TraderLlmReview, symbol: &str, veto: bool) -> bool {
    if veto {
        if let Some(c) = review
            .candidates
            .iter()
            .find(|c| c.symbol.eq_ignore_ascii_case(symbol))
        {
            return c.recommendation.eq_ignore_ascii_case("proceed");
        }
        return review.entry_recommendation.eq_ignore_ascii_case("proceed");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_mode_uses_plain_response_format() {
        assert!(response_format_for_mode(LlmResponseFormat::Plain).is_none());
        assert!(response_format_for_mode(LlmResponseFormat::JsonObject).is_some());
    }

    #[test]
    fn extracts_json_from_markdown_fence() {
        let raw = r#"Here is the review:
```json
{"market_commentary":"ok","web_insights":["x"],"candidates":[],"positions":[],"new_entries":{"recommendation":"proceed","reasoning":"fine"},"risk_alerts":[],"rule_patches":[]}
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
    fn perplexity_format_error_is_retryable() {
        let err = anyhow::anyhow!(
            "OpenRouter error 400: response_format type Input should be json_schema"
        );
        assert!(is_retryable_format_error(&err));
    }
}
