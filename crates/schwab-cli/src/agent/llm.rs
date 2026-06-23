use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::rules::{LlmConfig, LlmPhase};

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

        let mut body = json!({
            "model": model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user }
            ],
            "max_tokens": config.max_tokens,
        });

        // Perplexity/Sonar has built-in web search but rejects json_object response_format.
        if !use_web {
            body["response_format"] = json!({ "type": "json_object" });
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
        let payload: Value = resp.json().await.context("OpenRouter response parse failed")?;
        if !status.is_success() {
            anyhow::bail!(
                "OpenRouter error {}: {}",
                status,
                payload
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&payload.to_string())
            );
        }

        let content = payload
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .context("OpenRouter response missing content")?;

        let parsed: Value = serde_json::from_str(content)
            .or_else(|_| extract_json_object(content))
            .context("LLM returned non-JSON content")?;

        Ok(parse_llm_review(phase, model, use_web, parsed))
    }
}

pub fn build_system_prompt(config: &LlmConfig, phase: LlmPhase, use_web: bool) -> String {
    let instructions = match phase {
        LlmPhase::Selection => config.prompts.effective_selection_instructions(use_web),
        LlmPhase::Monitor => config.prompts.effective_monitor_instructions(),
    };
    format!("{instructions}\n{RESPONSE_JSON_SCHEMA}")
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

fn parse_llm_review(phase: LlmPhase, model: &str, used_web: bool, parsed: Value) -> LlmReview {
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

    LlmReview {
        phase: phase_label(phase).to_string(),
        model: model.to_string(),
        used_web,
        market_commentary: parsed
            .get("market_commentary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        web_insights: parsed
            .get("web_insights")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        entry_recommendation: parsed
            .pointer("/new_entries/recommendation")
            .and_then(|v| v.as_str())
            .unwrap_or("proceed")
            .to_string(),
        entry_reasoning: parsed
            .pointer("/new_entries/reasoning")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
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
    }
}

fn phase_label(phase: LlmPhase) -> &'static str {
    match phase {
        LlmPhase::Selection => "selection",
        LlmPhase::Monitor => "monitor",
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
fn extract_json_object(content: &str) -> Result<Value> {
    if let Ok(v) = serde_json::from_str(content) {
        return Ok(v);
    }
    if let Some(start) = content.find('{') {
        if let Some(end) = content.rfind('}') {
            if end > start {
                return Ok(serde_json::from_str(&content[start..=end])?);
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
        let review = parse_llm_review(LlmPhase::Selection, "test", true, raw);
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
}
