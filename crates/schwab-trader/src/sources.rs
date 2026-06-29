//! Fetch user-configured data feeds (URLs, APIs, RSS) for LLM context.

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::header::{AUTHORIZATION, HeaderName, HeaderValue};
use serde::Serialize;
use serde_json::{json, Value};

use crate::rules::{DataFeedSource, FeedAuth, TraderRules};

const USER_AGENT_VALUE: &str = "schwab-trader/0.1 (+https://github.com/schwabinvestbot)";

#[derive(Debug, Clone, Serialize)]
pub struct FeedFetchResult {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub url: String,
    pub ok: bool,
    pub status_code: Option<u16>,
    pub content_type: Option<String>,
    pub byte_count: usize,
    pub content: Option<String>,
    pub error: Option<String>,
    pub fetched_at: DateTime<Utc>,
}

pub async fn fetch_feeds_for_phase(rules: &TraderRules, phase: &str) -> Vec<FeedFetchResult> {
    let feeds = rules.feeds_for_phase(phase);
    let mut out = Vec::with_capacity(feeds.len());
    for feed in feeds {
        out.push(fetch_one(feed).await);
    }
    out
}

pub async fn fetch_all_enabled(rules: &TraderRules) -> Vec<FeedFetchResult> {
    let mut out = Vec::new();
    for feed in rules.sources.feeds.iter().filter(|f| f.enabled) {
        out.push(fetch_one(feed).await);
    }
    out
}

pub fn catalog_json(rules: &TraderRules, phase: Option<&str>) -> Value {
    let feeds: Vec<Value> = rules
        .sources
        .feeds
        .iter()
        .filter(|f| {
            f.enabled
                && phase
                    .map(|p| f.applies_to_phase(p))
                    .unwrap_or(true)
        })
        .map(|f| {
            json!({
                "id": f.id,
                "label": if f.label.is_empty() { &f.id } else { &f.label },
                "kind": f.kind,
                "url": f.url,
                "phases": f.phases,
                "has_auth": f.auth.is_some(),
            })
        })
        .collect();
    json!({ "feeds": feeds, "count": feeds.len() })
}

pub fn feeds_context_json(phase: &str, results: &[FeedFetchResult]) -> Value {
    json!({
        "phase": phase,
        "fetched_at": Utc::now().to_rfc3339(),
        "instructions": "Use source_feeds.fetched content as primary ground truth. Cite feed id in web_insights and candidate reasoning.",
        "fetched": results.iter().map(feed_result_json).collect::<Vec<_>>(),
    })
}

pub fn attach_feeds_to_context(
    mut context: Value,
    rules: &TraderRules,
    phase: &str,
    results: &[FeedFetchResult],
) -> Value {
    if let Some(obj) = context.as_object_mut() {
        obj.insert(
            "source_feed_catalog".to_string(),
            catalog_json(rules, Some(phase)),
        );
        if !results.is_empty() {
            obj.insert(
                "source_feeds".to_string(),
                feeds_context_json(phase, results),
            );
        }
    }
    context
}

async fn fetch_one(feed: &DataFeedSource) -> FeedFetchResult {
    let label = if feed.label.is_empty() {
        feed.id.clone()
    } else {
        feed.label.clone()
    };
    let mut base = FeedFetchResult {
        id: feed.id.clone(),
        label,
        kind: feed.kind.clone(),
        url: feed.url.clone(),
        ok: false,
        status_code: None,
        content_type: None,
        byte_count: 0,
        content: None,
        error: None,
        fetched_at: Utc::now(),
    };

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(feed.timeout_seconds.max(3)))
        .user_agent(USER_AGENT_VALUE)
        .build()
    {
        Ok(c) => c,
        Err(err) => {
            base.error = Some(format!("http client: {err}"));
            return base;
        }
    };

    let mut req = client.get(&feed.url);
    req = match apply_auth(req, feed.auth.as_ref()) {
        Ok(r) => r,
        Err(err) => {
            base.error = Some(err);
            return base;
        }
    };
    for (k, v) in &feed.headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            req = req.header(name, value);
        }
    }

    let response = match req.send().await {
        Ok(r) => r,
        Err(err) => {
            base.error = Some(format!("request failed: {err}"));
            return base;
        }
    };

    base.status_code = Some(response.status().as_u16());
    base.content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(err) => {
            base.error = Some(format!("read body: {err}"));
            return base;
        }
    };

    base.byte_count = bytes.len();
    if !base.status_code.is_some_and(|c| (200..300).contains(&c)) {
        base.error = Some(format!("HTTP {}", base.status_code.unwrap_or(0)));
        return base;
    }

    let raw = String::from_utf8_lossy(&bytes).to_string();
    let processed = match feed.kind.trim().to_ascii_lowercase().as_str() {
        "api" => format_api_content(&raw),
        "rss" => extract_rss_text(&raw),
        _ => strip_html_noise(&raw),
    };
    base.content = Some(truncate_utf8(&processed, feed.max_bytes.max(512)));
    base.ok = true;
    base
}

fn apply_auth(
    req: reqwest::RequestBuilder,
    auth: Option<&FeedAuth>,
) -> std::result::Result<reqwest::RequestBuilder, String> {
    let Some(auth) = auth else {
        return Ok(req);
    };
    let token = std::env::var(&auth.token_env).map_err(|_| {
        format!(
            "env var `{}` not set (required for feed auth)",
            auth.token_env
        )
    })?;
    if token.trim().is_empty() {
        return Err(format!("env var `{}` is empty", auth.token_env));
    }
    match auth.kind.trim().to_ascii_lowercase().as_str() {
        "bearer" => Ok(req.header(AUTHORIZATION, format!("Bearer {}", token.trim()))),
        "header" => {
            let name = auth
                .header_name
                .as_deref()
                .ok_or_else(|| "auth.header_name required".to_string())?;
            Ok(req.header(name, token.trim()))
        }
        other => Err(format!("unknown auth kind `{other}`")),
    }
}

fn format_api_content(raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.to_string()),
        Err(_) => raw.to_string(),
    }
}

fn extract_rss_text(xml: &str) -> String {
    let mut items = Vec::new();
    for chunk in xml.split("<item>").skip(1) {
        let title = extract_xml_tag(chunk, "title");
        let desc = extract_xml_tag(chunk, "description");
        if title.is_some() || desc.is_some() {
            items.push(format!(
                "{} — {}",
                title.unwrap_or_default(),
                desc.unwrap_or_default()
            ));
        }
        if items.len() >= 25 {
            break;
        }
    }
    if items.is_empty() {
        strip_html_noise(xml)
    } else {
        items.join("\n")
    }
}

fn extract_xml_tag(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&close)? + start;
    let inner = &block[start..end];
    Some(strip_html_noise(inner).trim().to_string())
}

fn strip_html_noise(html: &str) -> String {
    let mut out = String::with_capacity(html.len().min(16_384));
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => {
                out.push(ch);
            }
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated]", &s[..end])
}

fn feed_result_json(r: &FeedFetchResult) -> Value {
    json!({
        "id": r.id,
        "label": r.label,
        "kind": r.kind,
        "url": r.url,
        "ok": r.ok,
        "status_code": r.status_code,
        "content_type": r.content_type,
        "byte_count": r.byte_count,
        "error": r.error,
        "content": r.content,
        "fetched_at": r.fetched_at.to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::TraderRules;

    #[test]
    fn rss_extracts_items() {
        let xml = r#"
        <rss><channel>
          <item><title>Market rally</title><description>Futures up</description></item>
          <item><title>Fed speak</title><description>Watch Powell</description></item>
        </channel></rss>"#;
        let text = extract_rss_text(xml);
        assert!(text.contains("Market rally"));
        assert!(text.contains("Fed speak"));
    }

    #[test]
    fn truncate_respects_utf8() {
        let s = "hello world";
        assert_eq!(truncate_utf8(s, 100), s);
        let t = truncate_utf8(s, 8);
        assert!(t.contains('…'));
    }

    #[test]
    fn phase_filter_defaults() {
        let mut rules = TraderRules::default();
        rules.sources.feeds.push(DataFeedSource {
            id: "test".into(),
            label: String::new(),
            enabled: true,
            kind: "url".into(),
            url: "https://example.com/news".into(),
            phases: vec![],
            auth: None,
            headers: Default::default(),
            max_bytes: 1000,
            timeout_seconds: 5,
        });
        assert!(rules.feeds_for_phase("premarket_digest").len() == 1);
        assert!(rules.feeds_for_phase("learn").is_empty());
    }
}
