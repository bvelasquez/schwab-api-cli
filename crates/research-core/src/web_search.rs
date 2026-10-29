//! OpenRouter `openrouter:web_search` client (plan §0.2, §6).
//!
//! The server tool lets the model decide when and how often to search within a
//! single chat request. This module builds the request, sends it, and parses
//! `url_citation` annotations into a stable shape.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::SearchConfig;

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchParams {
    pub engine: String,
    pub max_results: u32,
    pub max_total_results: u32,
    pub search_context_size: String,
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    #[serde(default)]
    pub excluded_domains: Vec<String>,
}

impl SearchParams {
    pub fn from_config(config: &SearchConfig) -> Self {
        Self {
            engine: config.engine.clone(),
            max_results: config.max_results,
            max_total_results: config.max_total_results,
            search_context_size: config.search_context_size.clone(),
            allowed_domains: config.allowed_domains.clone(),
            excluded_domains: config.excluded_domains.clone(),
        }
    }
}

/// The `tools` array value for a chat request.
pub fn build_web_search_tool(params: &SearchParams) -> Value {
    let mut parameters = json!({
        "engine": params.engine,
        "max_results": params.max_results,
        "max_total_results": params.max_total_results,
        "search_context_size": params.search_context_size,
    });
    if !params.allowed_domains.is_empty() {
        parameters["allowed_domains"] = json!(params.allowed_domains);
    }
    if !params.excluded_domains.is_empty() {
        parameters["excluded_domains"] = json!(params.excluded_domains);
    }
    json!([{ "type": "openrouter:web_search", "parameters": parameters }])
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Citation {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatResult {
    pub content: String,
    pub citations: Vec<Citation>,
    pub model: String,
    #[serde(default)]
    pub usage: Value,
}

pub struct OpenRouterClient {
    http: Client,
    api_key: String,
}

impl OpenRouterClient {
    pub fn new(api_key: String) -> Self {
        Self {
            http: Client::new(),
            api_key,
        }
    }

    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .context("OPENROUTER_API_KEY required for the research arm's web search")?;
        if api_key.trim().is_empty() {
            anyhow::bail!("OPENROUTER_API_KEY is empty");
        }
        Ok(Self::new(api_key))
    }

    /// Request body for a web-search chat turn. Split out for testing.
    pub fn build_chat_body(
        model: &str,
        system: &str,
        user: &str,
        params: &SearchParams,
        max_tokens: u32,
    ) -> Value {
        json!({
            "model": model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user }
            ],
            "tools": build_web_search_tool(params),
            "max_tokens": max_tokens,
        })
    }

    pub async fn chat(
        &self,
        model: &str,
        system: &str,
        user: &str,
        params: &SearchParams,
        max_tokens: u32,
    ) -> Result<ChatResult> {
        let body = Self::build_chat_body(model, system, user, params, max_tokens);
        let resp = self
            .http
            .post(OPENROUTER_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", "https://github.com/schwabinvestbot")
            .header("X-Title", "agent-trader research")
            .json(&body)
            .send()
            .await
            .context("OpenRouter web-search request failed")?;
        let status = resp.status();
        let payload: Value = resp.json().await.context("OpenRouter response was not JSON")?;
        if !status.is_success() {
            let message = payload
                .pointer("/error/message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown OpenRouter error");
            anyhow::bail!("OpenRouter error {status}: {message}");
        }
        Ok(ChatResult {
            content: extract_content(&payload)?,
            citations: parse_citations(&payload),
            model: payload
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or(model)
                .to_string(),
            usage: payload.get("usage").cloned().unwrap_or(Value::Null),
        })
    }
}

pub fn extract_content(payload: &Value) -> Result<String> {
    let message = payload
        .pointer("/choices/0/message")
        .context("OpenRouter response missing message")?;
    match message.get("content") {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Array(parts)) => {
            let mut out = String::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    out.push_str(text);
                }
            }
            if out.is_empty() {
                anyhow::bail!("OpenRouter message content array had no text");
            }
            Ok(out)
        }
        _ => anyhow::bail!("OpenRouter message had no content"),
    }
}

pub fn parse_citations(payload: &Value) -> Vec<Citation> {
    let mut out = Vec::new();
    let annotations = payload
        .pointer("/choices/0/message/annotations")
        .and_then(|v| v.as_array());
    if let Some(annotations) = annotations {
        for annotation in annotations {
            if annotation.get("type").and_then(|v| v.as_str()) != Some("url_citation") {
                continue;
            }
            let source = annotation.get("url_citation").unwrap_or(annotation);
            let Some(url) = source.get("url").and_then(|v| v.as_str()) else {
                continue;
            };
            out.push(Citation {
                url: url.to_string(),
                title: source
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                snippet: source
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_carries_caps_and_domains() {
        let params = SearchParams {
            engine: "auto".into(),
            max_results: 5,
            max_total_results: 20,
            search_context_size: "medium".into(),
            allowed_domains: vec!["reuters.com".into()],
            excluded_domains: vec![],
        };
        let tool = build_web_search_tool(&params);
        assert_eq!(tool[0]["type"], json!("openrouter:web_search"));
        assert_eq!(tool[0]["parameters"]["max_total_results"], json!(20));
        assert_eq!(tool[0]["parameters"]["allowed_domains"][0], json!("reuters.com"));
        assert!(tool[0]["parameters"].get("excluded_domains").is_none());
    }

    #[test]
    fn chat_body_includes_the_tool() {
        let params = SearchParams {
            engine: "auto".into(),
            max_results: 5,
            max_total_results: 20,
            search_context_size: "medium".into(),
            allowed_domains: vec![],
            excluded_domains: vec![],
        };
        let body = OpenRouterClient::build_chat_body("m", "sys", "usr", &params, 1024);
        assert_eq!(body["tools"][0]["type"], json!("openrouter:web_search"));
        assert_eq!(body["messages"][0]["role"], json!("system"));
    }

    #[test]
    fn citations_are_parsed() {
        let payload = json!({
            "choices": [{
                "message": {
                    "content": "hello",
                    "annotations": [
                        { "type": "url_citation", "url_citation": { "url": "https://x.test/a", "title": "A" } },
                        { "type": "other" }
                    ]
                }
            }]
        });
        let citations = parse_citations(&payload);
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0].url, "https://x.test/a");
        assert_eq!(citations[0].title.as_deref(), Some("A"));
    }

    #[test]
    fn content_extraction_handles_strings_and_arrays() {
        let string = json!({"choices":[{"message":{"content":"hi"}}]});
        assert_eq!(extract_content(&string).unwrap(), "hi");
        let array = json!({"choices":[{"message":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}]});
        assert_eq!(extract_content(&array).unwrap(), "ab");
    }
}
