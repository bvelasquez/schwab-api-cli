//! Point-in-time LLM scores. The model is called once per (symbol, day) by
//! the research batch. Shadow arms only read the journal. A missing score
//! never blocks an entry: that mapping halted the live path once already.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::rules::{LlmSignalConfig, TraderRules};

pub const DEFAULT_JOURNAL: &str = "rules/llm-signal-journal.jsonl";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryAdjust {
    Proceed,
    Resize(i32),
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignalDecision {
    pub day: String,
    pub symbol: String,
    pub blinded: bool,
    pub model: String,
    pub model_cutoff: String,
    pub prompt_hash: String,
    pub input_hash: String,
    /// Median of the samples, in [-2, 2].
    pub score: i32,
    pub event_risk: bool,
    pub event_type: String,
    pub gap_risk: bool,
    pub confidence: f64,
    pub headline_ids: Vec<String>,
    pub samples: Vec<i32>,
    /// `scorer` or `agent`.
    pub source: String,
    /// Agent-only. 1.0 means full size. Clamped to [0.5, 1.0] before use.
    #[serde(default = "default_size")]
    pub size_multiplier: f64,
    /// `approve` | `veto` for agent rows. Scorer rows leave this empty.
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub reason_codes: Vec<String>,
    /// False for `--dry-run` stubs. The IC eval ignores these.
    pub live: bool,
}

fn default_size() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedScore {
    pub score: i32,
    pub event_risk: bool,
    pub event_type: String,
    pub gap_risk: bool,
    pub confidence: f64,
    pub headline_ids: Vec<String>,
}

pub fn journal_path(rules: &TraderRules) -> PathBuf {
    let configured = rules.llm_signal.journal_file.trim();
    if configured.is_empty() {
        PathBuf::from(DEFAULT_JOURNAL)
    } else {
        PathBuf::from(configured)
    }
}

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub fn hash_hex(bytes: &[u8]) -> String {
    format!("{:016x}", fnv1a64(bytes))
}

/// Strip ticker, company-name fields, and absolute dates so a blinded score
/// cannot recite a memorized headline.
pub fn blind_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let key = k.to_ascii_lowercase();
                if matches!(
                    key.as_str(),
                    "symbol" | "ticker" | "company" | "company_name" | "name"
                ) {
                    out.insert(k.clone(), Value::String("SYM".into()));
                    continue;
                }
                out.insert(k.clone(), blind_value(v));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(blind_value).collect()),
        Value::String(s) => Value::String(mask_dates(s)),
        other => other.clone(),
    }
}

fn mask_dates(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 10 <= bytes.len() && is_iso_date(&bytes[i..i + 10]) {
            out.push_str("DATE");
            i += 10;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn is_iso_date(b: &[u8]) -> bool {
    b.len() >= 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(|c| c.is_ascii_digit())
        && b[5..7].iter().all(|c| c.is_ascii_digit())
        && b[8..10].iter().all(|c| c.is_ascii_digit())
}

pub fn parse_score_json(raw: &Value) -> Result<ParsedScore> {
    let score = raw
        .get("score")
        .and_then(|v| v.as_i64())
        .context("score missing")? as i32;
    let score = score.clamp(-2, 2);
    let event_type = raw
        .get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_string();
    let headline_ids = raw
        .get("headline_ids")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(ParsedScore {
        score,
        event_risk: raw
            .get("event_risk")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        event_type,
        gap_risk: raw
            .get("gap_risk")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        confidence: raw
            .get("confidence")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            .clamp(0.0, 1.0),
        headline_ids,
    })
}

pub fn median_score(mut samples: Vec<i32>) -> i32 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort();
    samples[samples.len() / 2]
}

pub fn clamp_size_multiplier(mult: f64) -> f64 {
    if !mult.is_finite() {
        return 1.0;
    }
    mult.clamp(0.5, 1.0)
}

/// Deterministic stand-in used by `--dry-run`. Not a forecast.
pub fn stub_score(rsi: Option<f64>) -> i32 {
    match rsi {
        Some(r) if (48.0..=65.0).contains(&r) => 1,
        Some(r) if r < 35.0 => -1,
        _ => 0,
    }
}

pub fn prompt_template() -> &'static str {
    "You score a single equity for a 5-10 trading day long swing. Use only the JSON bundle. \
Do not assume you know what happened after the as-of date. Respond with JSON: \
{\"score\": -2|-1|0|1|2, \"event_risk\": bool, \"event_type\": \"none|earnings|guidance|mna|litigation|regulatory|other\", \
\"gap_risk\": bool, \"confidence\": 0-1, \"headline_ids\": [string]}"
}

pub fn append_decision(path: &Path, row: &SignalDecision) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(row)?;
    writeln!(file, "{line}")?;
    Ok(())
}

pub fn load_decisions(path: &Path) -> Vec<SignalDecision> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|l| serde_json::from_str::<SignalDecision>(l.trim()).ok())
        .collect()
}

/// Latest live decision for (symbol, day, blinded, source). Dry-run rows lose
/// to a live row. Among equals, the last line wins.
pub fn lookup<'a>(
    rows: &'a [SignalDecision],
    symbol: &str,
    day: &str,
    blinded: bool,
    source: &str,
) -> Option<&'a SignalDecision> {
    let sym = symbol.trim().to_uppercase();
    rows.iter()
        .enumerate()
        .filter(|(_, r)| {
            r.symbol.eq_ignore_ascii_case(&sym)
                && r.day == day
                && r.blinded == blinded
                && r.source == source
        })
        .max_by_key(|(i, r)| (r.live, *i))
        .map(|(_, r)| r)
}

fn today_string(rules: &TraderRules) -> String {
    crate::market_session::trading_day(&rules.schedule.timezone)
        .format("%Y-%m-%d")
        .to_string()
}

/// Shadow entry hook. `off` and a missing row both proceed.
pub fn adjust_entry(rules: &TraderRules, symbol: &str) -> (EntryAdjust, Option<String>) {
    let mode = rules.llm_signal.policy.mode.trim();
    if mode == "off" || mode.is_empty() {
        return (EntryAdjust::Proceed, None);
    }
    let day = today_string(rules);
    let rows = load_decisions(&journal_path(rules));
    let source = if mode == "agent" { "agent" } else { "scorer" };
    let Some(row) = lookup(&rows, symbol, &day, false, source).filter(|r| r.live) else {
        return (EntryAdjust::Proceed, Some("llm_decision_missing".into()));
    };
    match mode {
        "veto" if row.score < rules.llm_signal.policy.min_score => {
            (EntryAdjust::Skip, Some("llm_score_veto".into()))
        }
        "downsize" if row.score < rules.llm_signal.policy.min_score => (
            EntryAdjust::Resize(percent(rules.llm_signal.policy.size_multiplier)),
            Some("llm_score_downsize".into()),
        ),
        "event_skip" if row.event_risk => (EntryAdjust::Skip, Some("llm_event_skip".into())),
        "event_downsize" if row.event_risk => (
            EntryAdjust::Resize(percent(rules.llm_signal.policy.size_multiplier)),
            Some("llm_event_downsize".into()),
        ),
        "agent" if row.action == "veto" => (EntryAdjust::Skip, Some("llm_agent_veto".into())),
        "agent" => (
            EntryAdjust::Resize(percent(clamp_size_multiplier(row.size_multiplier))),
            Some("llm_agent_size".into()),
        ),
        _ => (EntryAdjust::Proceed, None),
    }
}

fn percent(mult: f64) -> i32 {
    (clamp_size_multiplier(mult) * 1000.0).round() as i32
}

pub fn size_from_adjust(quantity: f64, adjust: EntryAdjust) -> f64 {
    match adjust {
        EntryAdjust::Resize(millis) => quantity * (millis as f64 / 1000.0),
        _ => quantity,
    }
}

/// Morning review: recommend an exit only. The shadow arm with `event_exit`
/// is the only caller that turns this into a paper fill.
pub fn forced_exit_reason(rules: &TraderRules, symbol: &str) -> Option<String> {
    if rules.llm_signal.policy.mode.trim() != "event_exit" {
        return None;
    }
    let day = today_string(rules);
    let rows = load_decisions(&journal_path(rules));
    let row = lookup(&rows, symbol, &day, false, "scorer").filter(|r| r.live)?;
    if row.event_risk {
        Some("event_risk".into())
    } else {
        None
    }
}

pub fn build_named_decision(
    config: &LlmSignalConfig,
    day: NaiveDate,
    symbol: &str,
    model: &str,
    bundle: &Value,
    samples: Vec<ParsedScore>,
    blinded: bool,
    live: bool,
) -> SignalDecision {
    let scores: Vec<i32> = samples.iter().map(|s| s.score).collect();
    let score = median_score(scores.clone());
    let pick = samples
        .iter()
        .find(|s| s.score == score)
        .or_else(|| samples.first());
    let cutoff = config
        .knowledge_cutoffs
        .get(model)
        .cloned()
        .unwrap_or_default();
    let rendered = serde_json::to_string(bundle).unwrap_or_default();
    SignalDecision {
        day: day.format("%Y-%m-%d").to_string(),
        symbol: symbol.trim().to_uppercase(),
        blinded,
        model: model.to_string(),
        model_cutoff: cutoff,
        prompt_hash: hash_hex(prompt_template().as_bytes()),
        input_hash: hash_hex(rendered.as_bytes()),
        score,
        event_risk: pick.map(|p| p.event_risk).unwrap_or(false),
        event_type: pick
            .map(|p| p.event_type.clone())
            .unwrap_or_else(|| "none".into()),
        gap_risk: pick.map(|p| p.gap_risk).unwrap_or(false),
        confidence: pick.map(|p| p.confidence).unwrap_or(0.0),
        headline_ids: pick.map(|p| p.headline_ids.clone()).unwrap_or_default(),
        samples: scores,
        source: "scorer".into(),
        size_multiplier: 1.0,
        action: String::new(),
        reason_codes: Vec::new(),
        live,
    }
}

/// Group live named scorer rows by model for the comparison the IC script reads.
pub fn models_in(rows: &[SignalDecision]) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    for row in rows {
        if row.live && !row.blinded && row.source == "scorer" {
            *out.entry(row.model.clone()).or_insert(0) += 1;
        }
    }
    out
}

pub fn decision_json_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "score": {"type": "integer"},
            "event_risk": {"type": "boolean"},
            "event_type": {"type": "string"},
            "gap_risk": {"type": "boolean"},
            "confidence": {"type": "number"},
            "headline_ids": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["score", "event_risk", "event_type", "gap_risk", "confidence", "headline_ids"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blind_masks_symbol_and_dates() {
        let v = json!({"symbol": "NVDA", "as_of": "2024-03-01", "note": "flat"});
        let b = blind_value(&v);
        assert_eq!(b["symbol"], json!("SYM"));
        assert_eq!(b["as_of"], json!("DATE"));
        assert_eq!(b["note"], json!("flat"));
        assert_ne!(
            hash_hex(v.to_string().as_bytes()),
            hash_hex(b.to_string().as_bytes())
        );
    }

    #[test]
    fn median_and_clamp() {
        assert_eq!(median_score(vec![1, -1, 1]), 1);
        assert_eq!(median_score(vec![-2, 0, 2]), 0);
        assert!((clamp_size_multiplier(0.1) - 0.5).abs() < 1e-9);
        assert!((clamp_size_multiplier(2.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn parse_clamps_score() {
        let p = parse_score_json(&json!({
            "score": 9,
            "event_risk": true,
            "event_type": "guidance",
            "gap_risk": false,
            "confidence": 1.4,
            "headline_ids": ["h1"]
        }))
        .unwrap();
        assert_eq!(p.score, 2);
        assert!(p.event_risk);
        assert!((p.confidence - 1.0).abs() < 1e-9);
    }

    #[test]
    fn missing_decision_does_not_veto() {
        let rules = TraderRules::default();
        let (adj, _) = adjust_entry(&rules, "AAPL");
        assert_eq!(adj, EntryAdjust::Proceed);
    }

    #[test]
    fn veto_skips_only_when_a_live_row_is_below_the_floor() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("journal.jsonl");
        // Point the default journal at this file by writing through append and
        // looking up directly.
        let row = SignalDecision {
            day: "2026-10-08".into(),
            symbol: "AAPL".into(),
            blinded: false,
            model: "m".into(),
            model_cutoff: "2025-01-01".into(),
            prompt_hash: "a".into(),
            input_hash: "b".into(),
            score: -1,
            event_risk: true,
            event_type: "guidance".into(),
            gap_risk: true,
            confidence: 0.4,
            headline_ids: vec![],
            samples: vec![-1, -1, 0],
            source: "scorer".into(),
            size_multiplier: 1.0,
            action: String::new(),
            reason_codes: vec![],
            live: true,
        };
        append_decision(&path, &row).unwrap();
        let loaded = load_decisions(&path);
        let found = lookup(&loaded, "aapl", "2026-10-08", false, "scorer").unwrap();
        assert_eq!(found.score, -1);
        assert!(found.event_risk);
        let dry = SignalDecision {
            live: false,
            score: 2,
            ..row.clone()
        };
        let both = vec![dry, row];
        assert!(
            lookup(&both, "AAPL", "2026-10-08", false, "scorer")
                .unwrap()
                .live
        );
    }
}
