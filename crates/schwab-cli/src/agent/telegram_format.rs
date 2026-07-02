//! Plain-language Telegram messages (no raw JSON).

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::agent::llm::{LlmReview, PositionReview};
use crate::agent::state::AgentState;
use crate::rules::TelegramNotifyConfig;

/// Fingerprint for deduplicating routine LLM Telegram updates.
pub fn llm_digest_key(review: &LlmReview) -> String {
    let pos = review
        .position_reviews
        .first()
        .map(|p| {
            format!(
                "{}:{}:{}",
                short_position_label(&p.position_id),
                p.recommendation.to_lowercase(),
                p.urgency.to_lowercase()
            )
        })
        .unwrap_or_else(|| "none".into());
    format!(
        "{}|{}|{}",
        review.phase,
        review.entry_recommendation.to_lowercase(),
        pos
    )
}

pub fn is_llm_urgent(review: &LlmReview) -> bool {
    review.entry_recommendation.eq_ignore_ascii_case("proceed")
        || !review.urgent_close_positions().is_empty()
        || review
            .position_reviews
            .iter()
            .any(|p| p.urgency.eq_ignore_ascii_case("high"))
}

/// Whether to send an LLM review to Telegram (urgent immediately; routine on digest cadence).
pub fn should_send_llm_telegram(
    review: &LlmReview,
    config: &TelegramNotifyConfig,
    state: &AgentState,
    now: DateTime<Utc>,
) -> bool {
    let key = llm_digest_key(review);

    if is_llm_urgent(review) {
        if !config.llm_notify_urgent {
            return false;
        }
        if state.last_telegram_llm_digest_key.as_deref() == Some(key.as_str()) {
            if let Some(at) = state.last_telegram_llm_at {
                if (now - at).num_minutes() < config.llm_urgent_cooldown_minutes as i64 {
                    return false;
                }
            }
        }
        return true;
    }

    if !config.llm_notify_digest || config.llm_digest_interval_minutes == 0 {
        return false;
    }
    if state.last_telegram_llm_digest_key.as_deref() == Some(key.as_str()) {
        return false;
    }
    if let Some(at) = state.last_telegram_llm_at {
        if (now - at).num_minutes() < config.llm_digest_interval_minutes as i64 {
            return false;
        }
    }
    true
}

pub fn record_llm_telegram_sent(state: &mut AgentState, review: &LlmReview, now: DateTime<Utc>) {
    state.last_telegram_llm_at = Some(now);
    state.last_telegram_llm_digest_key = Some(llm_digest_key(review));
}

/// Broker-style status update from an LLM review.
pub fn format_llm_review_telegram(review: &LlmReview, monitored: &[Value]) -> String {
    let mut out = String::new();

    let headline = match review.phase.as_str() {
        "overnight_digest" => "Overnight check-in",
        "selection" => "Options update",
        _ => "Position check-in",
    };
    out.push_str(headline);
    out.push_str("\n\n");

    if let Some(snapshot) = monitored.first() {
        out.push_str(&format_position_snapshot(snapshot));
        out.push('\n');
    }

    for pos in &review.position_reviews {
        out.push_str(&format_position_advice(pos));
        out.push('\n');
    }

    out.push_str(&format_entry_advice(review));
    out.push_str("\n\n");
    out.push_str(&format_what_to_do(review));
    out
}

pub fn format_overnight_telegram(review: &LlmReview) -> String {
    let mut msg = format_llm_review_telegram(review, &[]);
    if !review.risk_alerts.is_empty() {
        msg.push_str("\n\nWatch overnight:");
        for alert in &review.risk_alerts {
            msg.push_str("\n• ");
            msg.push_str(&plain_sentence(alert, 220));
        }
    }
    msg
}

pub fn format_market_open_telegram(playbook: Option<&Value>) -> String {
    let Some(pb) = playbook else {
        return "Market is open. Your agent is watching open positions.".into();
    };
    let mut out = "Market is open.\n\n".to_string();
    if let Some(positions) = pb.get("positions").and_then(|v| v.as_array()) {
        for pos in positions {
            let id = pos
                .get("position_id")
                .and_then(|v| v.as_str())
                .unwrap_or("position");
            let rec = pos
                .get("recommendation")
                .and_then(|v| v.as_str())
                .unwrap_or("watch");
            out.push_str(&format!(
                "Overnight note for {}: {}.\n",
                short_position_label(id),
                plain_rec_label(rec)
            ));
        }
    }
    if let Some(alerts) = pb.get("risk_alerts").and_then(|v| v.as_array()) {
        if !alerts.is_empty() {
            out.push_str("\nHeads up at the open:\n");
            for alert in alerts.iter().take(3) {
                if let Some(s) = alert.as_str() {
                    out.push_str("• ");
                    out.push_str(&plain_sentence(s, 200));
                    out.push('\n');
                }
            }
        }
    }
    out.trim_end().to_string()
}

/// Returns `None` when the action should not be pushed to Telegram (internal/skip).
pub fn format_action_telegram(kind: &str, detail: &Value) -> Option<String> {
    if let Some(fill) = detail.get("fill_status").and_then(|v| v.as_str()) {
        return format_order_telegram(kind, fill, detail);
    }
    if detail.get("exit").is_some() || kind.contains("EXIT") {
        let underlying = detail
            .pointer("/signal/underlying")
            .and_then(|v| v.as_str())
            .unwrap_or("position");
        let reason = detail
            .pointer("/signal/reason")
            .and_then(|v| v.as_str())
            .unwrap_or("rule");
        let reason_plain = match reason {
            "profit_target" => "profit target hit",
            "stop_loss" => "stop loss hit",
            "dte_close" => "approaching expiration",
            "llm_recommendation" => "advisor recommendation",
            other => other,
        };
        return Some(format!("Position closed: {underlying}\nReason: {reason_plain}"));
    }
    None
}

fn format_order_telegram(kind: &str, fill_status: &str, detail: &Value) -> Option<String> {
    if fill_status.eq_ignore_ascii_case("SKIPPED") {
        return None;
    }

    let signal = detail.get("signal").unwrap_or(detail);
    let underlying = signal
        .pointer("/params/underlying")
        .or_else(|| signal.get("underlying"))
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let expiry = signal
        .pointer("/params/expiry")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let short = signal.pointer("/params/short_strike").and_then(|v| v.as_f64());
    let long = signal.pointer("/params/long_strike").and_then(|v| v.as_f64());
    let credit = signal
        .get("estimated_credit")
        .or_else(|| signal.pointer("/params/limit_credit"))
        .and_then(|v| v.as_f64());
    let contracts = signal
        .pointer("/params/contracts")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0)
        .round() as u32;

    let strikes = match (short, long) {
        (Some(s), Some(l)) => format!("${s:.0}/${l:.0}"),
        _ => "spread".into(),
    };

    let status_line = match fill_status {
        "FILLED" => "Trade filled",
        "WORKING" | "ACCEPTED" | "PENDING_ACTIVATION" | "QUEUED" => "Limit order working",
        "REJECTED" | "CANCELED" | "EXPIRED" => "Order not filled",
        _ if kind.contains("REJECTED") => "Order not filled",
        _ => "Order update",
    };

    let mut msg = format!(
        "{status_line}\n{underlying} put spread {strikes}, exp {expiry}"
    );
    if let Some(c) = credit {
        msg.push_str(&format!("\nCredit ~${c:.2}"));
    }
    if contracts > 1 {
        msg.push_str(&format!("\n{contracts} contracts"));
    }
    if fill_status == "REJECTED" || fill_status == "CANCELED" {
        if let Some(note) = detail.get("note").and_then(|v| v.as_str()) {
            msg.push_str(&format!("\n{note}"));
        }
    }
    Some(msg)
}

fn format_position_snapshot(pos: &Value) -> String {
    let underlying = pos
        .get("underlying")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let expiry = pos.get("expiry").and_then(|v| v.as_str()).unwrap_or("?");
    let short = pos
        .pointer("/market_context/short_strike")
        .and_then(|v| v.as_f64());
    let long = pos
        .pointer("/market_context/long_strike")
        .and_then(|v| v.as_f64());
    let px = pos
        .pointer("/market_context/underlying_price")
        .and_then(|v| v.as_f64());
    let profit = pos.get("profit_pct").and_then(|v| v.as_f64());
    let dte = pos
        .get("dte")
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)));

    let strikes = match (short, long) {
        (Some(s), Some(l)) => format!(" (${s:.0}/${l:.0})"),
        _ => String::new(),
    };
    let mut line = format!("{underlying} spread{strikes}, exp {expiry}");
    if let Some(p) = px {
        line.push_str(&format!(" — {underlying} ${p:.2}"));
    }
    if let Some(pnl) = profit {
        line.push_str(&format!(" — {pnl:+.0}% on paper"));
    }
    if let Some(d) = dte {
        line.push_str(&format!(" — {d} days left"));
    }
    line
}

fn format_position_advice(pos: &PositionReview) -> String {
    let label = short_position_label(&pos.position_id);
    let action = plain_rec_label(&pos.recommendation);
    let why = plain_sentence(&pos.reasoning, 240);
    format!("{label}: {action}\n{why}")
}

fn format_entry_advice(review: &LlmReview) -> String {
    let rec = review.entry_recommendation.to_lowercase();
    let headline = match rec.as_str() {
        "proceed" => "New trade: Yes — ready to open if rules allow",
        "defer" => "New trade: Not now",
        "skip" => "New trade: No",
        _ => "New trade: Waiting",
    };
    let why = if review.entry_reasoning.trim().is_empty() {
        "No candidate met our entry rules this round.".into()
    } else {
        plain_sentence(&review.entry_reasoning, 280)
    };
    format!("{headline}\n{why}")
}

fn format_what_to_do(review: &LlmReview) -> String {
    if review.entry_recommendation.eq_ignore_ascii_case("proceed") {
        return "What to do: Review is favorable — the agent may place a limit order if risk limits allow.".into();
    }
    if !review.urgent_close_positions().is_empty() {
        return "What to do: Urgent — review the open position; a close may be warranted.".into();
    }
    if review
        .position_reviews
        .iter()
        .any(|p| p.urgency.eq_ignore_ascii_case("high"))
    {
        return "What to do: Watch closely today — elevated risk on an open spread.".into();
    }
    if review
        .position_reviews
        .iter()
        .any(|p| p.recommendation.eq_ignore_ascii_case("watch"))
    {
        return "What to do: No action required — keep an eye on the position; auto-exit rules are still in control.".into();
    }
    "What to do: Nothing right now — mechanical profit, stop, and expiration rules have not triggered.".into()
}

fn short_position_label(position_id: &str) -> String {
    let parts: Vec<&str> = position_id.split('|').collect();
    if parts.len() >= 3 {
        return format!("{} {}", parts[1], parts[2]);
    }
    position_id.to_string()
}

fn plain_rec_label(rec: &str) -> &'static str {
    match rec.to_lowercase().as_str() {
        "hold" => "Hold",
        "watch" => "Watch",
        "close" => "Consider closing",
        "enter" => "Candidate entry",
        _ => "Review",
    }
}

fn plain_sentence(text: &str, max_chars: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let mut out = String::new();
    for word in collapsed.split_whitespace() {
        if !out.is_empty() {
            if out.chars().count() + 1 + word.chars().count() > max_chars.saturating_sub(1) {
                out.push('…');
                return out;
            }
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::LlmReview;

    fn sample_review() -> LlmReview {
        LlmReview {
            phase: "selection".into(),
            model: "test".into(),
            used_web: false,
            raw: serde_json::json!({}),
            market_commentary: "long technical essay".into(),
            web_insights: vec![],
            position_reviews: vec![PositionReview {
                position_id: "ACC|IWM|2026-07-31|vertical|P280L_P282S".into(),
                recommendation: "hold".into(),
                urgency: "low".into(),
                reasoning: "Small paper loss; stop not hit.".into(),
            }],
            entry_recommendation: "defer".into(),
            entry_reasoning: "Already have IWM exposure; wait for a better setup.".into(),
            risk_alerts: vec!["concentration".into()],
        }
    }

    #[test]
    fn telegram_llm_is_plain_language() {
        let msg = format_llm_review_telegram(&sample_review(), &[]);
        assert!(msg.contains("Options update"));
        assert!(msg.contains("New trade: Not now"));
        assert!(msg.contains("What to do:"));
        assert!(!msg.contains("market_commentary"));
        assert!(!msg.contains("risk_alerts"));
    }

    #[test]
    fn skipped_orders_not_telegrammed() {
        let detail = serde_json::json!({
            "fill_status": "SKIPPED",
            "reason": "max_portfolio_risk_usd exceeded",
            "signal": { "params": { "underlying": "IWM" } }
        });
        assert!(format_action_telegram("ORDER", &detail).is_none());
    }

    #[test]
    fn filled_order_is_plain() {
        let detail = serde_json::json!({
            "fill_status": "FILLED",
            "signal": {
                "params": {
                    "underlying": "IWM",
                    "expiry": "2026-08-07",
                    "short_strike": 281.0,
                    "long_strike": 279.0,
                    "limit_credit": 0.27,
                    "contracts": 1.0
                },
                "estimated_credit": 0.27
            }
        });
        let msg = format_action_telegram("ENTRY FILLED", &detail).unwrap();
        assert!(msg.contains("Trade filled"));
        assert!(msg.contains("IWM"));
        assert!(!msg.contains("{"));
    }

    #[test]
    fn defer_with_alerts_not_urgent() {
        let review = sample_review();
        assert!(!is_llm_urgent(&review));
    }

    #[test]
    fn proceed_is_urgent() {
        let mut review = sample_review();
        review.entry_recommendation = "proceed".into();
        assert!(is_llm_urgent(&review));
    }

    #[test]
    fn digest_dedupes_same_message() {
        let config = TelegramNotifyConfig::default();
        let mut state = AgentState::default();
        let review = sample_review();
        let now = Utc::now();
        assert!(should_send_llm_telegram(&review, &config, &state, now));
        record_llm_telegram_sent(&mut state, &review, now);
        assert!(!should_send_llm_telegram(&review, &config, &state, now));
    }
}
