//! Telegram notifications for schwab-trader (entries, exits, major state changes).

use anyhow::Result;
use serde_json::Value;

use schwab_cli::notify::TelegramNotifier;

use crate::agent::llm::TraderLlmReview;
use crate::reconcile::ReconcileReport;
use crate::rules::{NotifyConfig, TraderRules};

pub fn telegram_from_rules(notify: &NotifyConfig) -> Result<Option<TelegramNotifier>> {
    TelegramNotifier::from_env(&notify.telegram.to_cli_config())
}

pub async fn notify_entry_attempt(
    tg: Option<&TelegramNotifier>,
    rules: &TraderRules,
    attempt: &Value,
) {
    let Some(tg) = tg else { return };
    if !tg.wants_actions() {
        return;
    }
    let status = attempt
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let inner = attempt.get("attempt").unwrap_or(attempt);
    let symbol = inner
        .get("symbol")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let qty = inner
        .get("quantity")
        .and_then(|v| v.as_f64())
        .map(|q| format!("{q:.2}"))
        .unwrap_or_else(|| "?".into());
    let price = inner
        .get("limit_price")
        .and_then(|v| v.as_f64())
        .map(|p| format!("${p:.2}"))
        .unwrap_or_else(|| "?".into());

    let (title, detail) = match status {
        "filled" => (
            "BUY FILLED",
            format!("{symbol} ×{qty} @ {price}"),
        ),
        "simulated" => (
            "SIM BUY",
            format!("{symbol} ×{qty} @ {price}"),
        ),
        "submitted" => (
            "BUY WORKING",
            inner
                .get("reason")
                .and_then(|v| v.as_str())
                .map(|r| format!("{symbol} ×{qty} @ {price} — {r}"))
                .unwrap_or_else(|| format!("{symbol} ×{qty} @ {price} — awaiting fill")),
        ),
        "dry_run" => return,
        "skipped" => return,
        _ => return,
    };
    let _ = tg
        .send(&format!("schwab-trader [{}]\n{title}\n{detail}", rules.trader_id))
        .await;
}

pub async fn notify_closure_exits(
    tg: Option<&TelegramNotifier>,
    rules: &TraderRules,
    exits: &[Value],
    simulate: bool,
) {
    let Some(tg) = tg else { return };
    if !tg.wants_actions() || exits.is_empty() {
        return;
    }
    for exit in exits {
        let symbol = exit
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let reason = exit
            .get("exit_reason")
            .or_else(|| exit.get("reason"))
            .and_then(|v| v.as_str())
            .unwrap_or("exit");
        let price = exit
            .get("fill_price")
            .or_else(|| exit.get("exit_price"))
            .and_then(|v| v.as_f64())
            .map(|p| format!(" @ ${p:.2}"))
            .unwrap_or_default();
        let prefix = if simulate { "SIM SELL" } else { "SELL" };
        let _ = tg
            .send(&format!(
                "schwab-trader [{}]\n{prefix} {symbol}\n{reason}{price}",
                rules.trader_id
            ))
            .await;
    }
}

pub async fn notify_reconcile_report(
    tg: Option<&TelegramNotifier>,
    rules: &TraderRules,
    report: &ReconcileReport,
) {
    let Some(tg) = tg else { return };
    if !tg.wants_actions() {
        return;
    }
    for sym in &report.adopted_positions {
        let _ = tg
            .send(&format!(
                "schwab-trader [{}]\nPOSITION ADOPTED\n{sym} (reconcile)",
                rules.trader_id
            ))
            .await;
    }
    for sym in &report.oco_filled {
        let _ = tg
            .send(&format!(
                "schwab-trader [{}]\nOCO EXIT\n{sym} (stop or target filled)",
                rules.trader_id
            ))
            .await;
    }
    for sym in &report.removed_positions {
        let _ = tg
            .send(&format!(
                "schwab-trader [{}]\nPOSITION CLOSED\n{sym} (gone at broker)",
                rules.trader_id
            ))
            .await;
    }
    for mismatch in &report.mismatches {
        let ty = mismatch
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("mismatch");
        if ty == "oco_canceled" {
            let sym = mismatch
                .get("symbol")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let _ = tg
                .send(&format!(
                    "schwab-trader [{}]\n⚠ OCO CANCELED\n{sym} — check bracket",
                    rules.trader_id
                ))
                .await;
        }
    }
}

pub async fn notify_trading_halted(
    tg: Option<&TelegramNotifier>,
    rules: &TraderRules,
    reason: &str,
) {
    let Some(tg) = tg else { return };
    if !tg.wants_actions() {
        return;
    }
    let _ = tg
        .send(&format!(
            "schwab-trader [{}]\n⚠ TRADING HALTED\n{reason}",
            rules.trader_id
        ))
        .await;
}

pub async fn notify_profile_change(
    tg: Option<&TelegramNotifier>,
    rules: &TraderRules,
    from: &Option<String>,
    to: &Option<String>,
    reason: &Option<String>,
) {
    let Some(tg) = tg else { return };
    if !tg.wants_actions() {
        return;
    }
    let _ = tg
        .send(&format!(
            "schwab-trader [{}]\nPROFILE {}\n{} → {}\n{}",
            rules.trader_id,
            "CHANGE",
            from.as_deref().unwrap_or("(none)"),
            to.as_deref().unwrap_or("(none)"),
            reason.as_deref().unwrap_or("")
        ))
        .await;
}

pub async fn notify_at_open(
    tg: Option<&TelegramNotifier>,
    rules: &TraderRules,
    playbook: Option<&Value>,
) {
    let Some(tg) = tg else { return };
    if !tg.wants_actions() {
        return;
    }
    let Some(pb) = playbook else { return };
    let commentary = pb
        .get("market_commentary")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if commentary.is_empty() {
        return;
    }
    let preview: String = commentary.chars().take(400).collect();
    let suffix = if commentary.chars().count() > 400 {
        "…"
    } else {
        ""
    };
    let _ = tg
        .send(&format!(
            "schwab-trader [{}]\nMARKET OPEN\n{preview}{suffix}",
            rules.trader_id
        ))
        .await;
}

pub async fn notify_llm_alerts(
    tg: Option<&TelegramNotifier>,
    rules: &TraderRules,
    review: &TraderLlmReview,
) {
    let Some(tg) = tg else { return };
    if !tg.wants_actions() {
        return;
    }
    let urgent_positions = review
        .positions
        .iter()
        .filter(|p| p.urgency.eq_ignore_ascii_case("high"))
        .count();
    if review.risk_alerts.is_empty() && urgent_positions == 0 {
        return;
    }
    let alerts = if review.risk_alerts.is_empty() {
        review
            .positions
            .iter()
            .filter(|p| p.urgency.eq_ignore_ascii_case("high"))
            .map(|p| format!("{}: {}", p.position_id, p.reasoning))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        review.risk_alerts.join("\n")
    };
    let _ = tg
        .send(&format!(
            "schwab-trader [{}]\nLLM ALERT ({phase})\n{alerts}",
            rules.trader_id,
            phase = review.phase
        ))
        .await;
}

pub async fn notify_rule_adaptation(
    tg: Option<&TelegramNotifier>,
    rules: &TraderRules,
    patch_count: usize,
) {
    if !rules.notify.telegram.notify_on_rule_adaptation {
        return;
    }
    let Some(tg) = tg else { return };
    let _ = tg
        .send(&format!(
            "schwab-trader [{}]\nRULE PATCH\n{patch_count} change(s) applied",
            rules.trader_id
        ))
        .await;
}

pub async fn notify_tick_summary(
    tg: Option<&TelegramNotifier>,
    rules: &TraderRules,
    session: &str,
    open: usize,
    trades_today: u32,
) {
    let Some(tg) = tg else { return };
    if !tg.wants_tick_summary() {
        return;
    }
    let _ = tg
        .send(&format!(
            "schwab-trader [{}]\ntick · {session} · {open} open · {trades_today} trades today",
            rules.trader_id
        ))
        .await;
}
