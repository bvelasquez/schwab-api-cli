//! Telegram notifications for schwab-trader (entries, exits, major state changes).

use anyhow::Result;
use serde_json::Value;

use schwab_cli::notify::TelegramNotifier;
use schwab_cli::trade_audio::{self, TradeAudioEvent};

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
    let status = attempt
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    match status {
        "filled" | "simulated" => trade_audio::speak(TradeAudioEvent::EntryOpened),
        "submitted" => trade_audio::speak(TradeAudioEvent::EntryWorking),
        _ => {}
    }

    let Some(tg) = tg else { return };
    if !tg.wants_actions() {
        return;
    }
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
            "Bought (live)",
            format!("{symbol} — {qty} shares @ {price}"),
        ),
        "simulated" => (
            "Bought (paper trade)",
            format!("{symbol} — {qty} shares @ {price}"),
        ),
        "submitted" => (
            "Buy order working",
            inner
                .get("reason")
                .and_then(|v| v.as_str())
                .map(|r| format!("{symbol} — {qty} shares @ {price}\n{r}"))
                .unwrap_or_else(|| {
                    format!("{symbol} — {qty} shares @ {price}\nWaiting for fill at Schwab.")
                }),
        ),
        "dry_run" => return,
        "skipped" => return,
        _ => return,
    };
    let _ = tg
        .send(&format!("schwab-trader [{}]\n{title}\n{detail}", rules.trader_id))
        .await;
}

/// Plain-language line for mechanical / thesis exit codes (journal + Telegram).
pub fn exit_reason_plain_english(reason: &str) -> &'static str {
    match reason {
        "stop_loss" => "Stop-loss hit — cut the loss.",
        "profit_target" => "Profit target reached — taking gains.",
        "time_stop" => "Held long enough — closed on the time rule.",
        "eod_flatten" => "End of day — closed before the bell.",
        "overnight_flatten" => "No overnight hold — closed at the open.",
        "oco_filled" => "Bracket order filled at Schwab (stop or target).",
        "manual_close_all" => "Closed manually (close-all).",
        "thesis_profit_giveback" => "Gave back too much profit from the peak — closed.",
        "thesis_below_sma" => "Price fell below the trend line — closed.",
        "thesis_rs_deterioration" => "Momentum vs the market weakened — closed.",
        "thesis_regime" => "Market regime turned choppy — closed while still green.",
        _ if reason.starts_with("thesis_") => "Trade thesis broke — closed.",
        _ => "Position closed.",
    }
}

fn closure_action(exit: &Value) -> Option<&str> {
    exit.get("action").and_then(|v| v.as_str())
}

fn is_position_closure_event(exit: &Value) -> bool {
    exit.get("exit_reason")
        .or_else(|| exit.get("reason"))
        .and_then(|v| v.as_str())
        .is_some()
}

fn format_usd(price: f64) -> String {
    format!("${price:.2}")
}

fn format_closure_telegram(exit: &Value, simulate: bool) -> Option<(String, String)> {
    let symbol = exit.get("symbol").and_then(|v| v.as_str()).unwrap_or("?");

    if let Some(action) = closure_action(exit) {
        match action {
            "sim_exit_plan_tightened" | "exit_plan_tightened" => {
                let old = exit
                    .get("old_profit_limit")
                    .and_then(|v| v.as_f64())
                    .map(format_usd);
                let new = exit
                    .get("new_profit_limit")
                    .and_then(|v| v.as_f64())
                    .map(format_usd);
                let detail = match (old, new) {
                    (Some(o), Some(n)) => format!(
                        "{symbol} — still holding. Profit target trimmed {o} → {n} (recent volatility / range)."
                    ),
                    _ => format!(
                        "{symbol} — still holding. Profit target was tightened to match current conditions."
                    ),
                };
                let title = if simulate {
                    "Profit target adjusted (paper)".to_string()
                } else {
                    "Profit target adjusted".to_string()
                };
                return Some((title, detail));
            }
            "sim_trailing_stop_tightened" | "trailing_stop_tightened" => {
                let new_stop = exit
                    .get("new_stop")
                    .and_then(|v| v.as_f64())
                    .map(format_usd);
                let detail = match new_stop {
                    Some(s) => format!(
                        "{symbol} — still holding. Trailing stop raised to {s} to protect gains."
                    ),
                    None => format!("{symbol} — still holding. Trailing stop was raised."),
                };
                let title = if simulate {
                    "Stop raised (paper)".to_string()
                } else {
                    "Stop raised at Schwab".to_string()
                };
                return Some((title, detail));
            }
            _ => {}
        }
    }

    if !is_position_closure_event(exit) {
        return None;
    }

    let reason_code = exit
        .get("exit_reason")
        .or_else(|| exit.get("reason"))
        .and_then(|v| v.as_str())
        .unwrap_or("exit");
    let why = exit_reason_plain_english(reason_code);

    let price = exit
        .get("fill_price")
        .or_else(|| exit.get("exit_price"))
        .and_then(|v| v.as_f64());
    let price_line = price
        .map(|p| format!(" @ {}", format_usd(p)))
        .unwrap_or_default();

    let pnl_line = exit
        .get("pnl_usd")
        .and_then(|v| v.as_f64())
        .zip(exit.get("pnl_pct").and_then(|v| v.as_f64()))
        .map(|(usd, pct)| format!("\nP&L: {}{:.2} ({:+.1}%)", if usd >= 0.0 { "+" } else { "" }, usd, pct))
        .or_else(|| {
            exit.get("pnl_usd")
                .and_then(|v| v.as_f64())
                .map(|usd| format!("\nP&L: {}{:.2}", if usd >= 0.0 { "+" } else { "" }, usd))
        })
        .unwrap_or_default();

    let title = if simulate {
        "Sold (paper trade)".to_string()
    } else {
        "Sold (live)".to_string()
    };
    let detail = format!("{symbol}{price_line}\n{why}{pnl_line}");
    Some((title, detail))
}

pub async fn notify_closure_exits(
    tg: Option<&TelegramNotifier>,
    rules: &TraderRules,
    exits: &[Value],
    simulate: bool,
) {
    if exits.is_empty() {
        return;
    }
    for exit in exits {
        if is_position_closure_event(exit) {
            let reason = exit
                .get("exit_reason")
                .or_else(|| exit.get("reason"))
                .and_then(|v| v.as_str())
                .unwrap_or("exit");
            trade_audio::speak_exit_reason(reason);
        }
    }

    let Some(tg) = tg else { return };
    if !tg.wants_actions() {
        return;
    }
    for exit in exits {
        let Some((title, detail)) = format_closure_telegram(exit, simulate) else {
            continue;
        };
        let _ = tg
            .send(&format!(
                "schwab-trader [{}]\n{title}\n{detail}",
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
    for _sym in &report.oco_filled {
        trade_audio::speak(TradeAudioEvent::ExitBracket);
    }

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
        let _ = sym;
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
    trade_audio::speak(TradeAudioEvent::AlertHalted);
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

pub async fn notify_agent_degraded(
    tg: Option<&TelegramNotifier>,
    rules: &TraderRules,
    class: &str,
    err: &str,
    consecutive_failures: u32,
    simulate: bool,
) {
    // Avoid Telegram spam: first failure, then every 10th.
    if consecutive_failures > 1 && consecutive_failures % 10 != 0 {
        return;
    }
    let Some(tg) = tg else { return };
    if !tg.wants_actions() {
        return;
    }
    let mode = if simulate { "SIM" } else { "LIVE" };
    let hint = if class == "auth_fatal" {
        "\nFix: schwab auth login — agent will resume automatically"
    } else {
        "\nAgent staying up; backing off and retrying"
    };
    let short: String = err.chars().take(280).collect();
    let _ = tg
        .send(&format!(
            "schwab-trader [{}] [{mode}]\n⚠ AGENT DEGRADED ({class}) ×{consecutive_failures}\n{short}{hint}",
            rules.trader_id
        ))
        .await;
}

pub async fn notify_agent_recovered(
    tg: Option<&TelegramNotifier>,
    rules: &TraderRules,
    after_failures: u32,
) {
    let Some(tg) = tg else { return };
    if !tg.wants_actions() {
        return;
    }
    let _ = tg
        .send(&format!(
            "schwab-trader [{}]\n✓ AGENT RECOVERED\nafter {after_failures} failure(s) — exits armed again",
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
    open_position_count: usize,
) {
    let Some(tg) = tg else { return };
    if !tg.wants_actions() {
        return;
    }
    if let Some(pb) = playbook {
        let commentary = pb
            .get("market_commentary")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !commentary.is_empty() {
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
            return;
        }
    }
    let body = if open_position_count > 0 {
        format!(
            "schwab-trader [{}]\nMARKET OPEN\n{open_position_count} open position(s) — mechanical rules active.",
            rules.trader_id
        )
    } else {
        format!(
            "schwab-trader [{}]\nMARKET OPEN\nFlat — scanning for entries.",
            rules.trader_id
        )
    };
    let _ = tg.send(&body).await;
}

pub async fn notify_llm_alerts(
    tg: Option<&TelegramNotifier>,
    rules: &TraderRules,
    review: &TraderLlmReview,
) {
    let urgent_positions = review
        .positions
        .iter()
        .filter(|p| p.urgency.eq_ignore_ascii_case("high"))
        .count();
    if review.risk_alerts.is_empty() && urgent_positions == 0 {
        return;
    }
    trade_audio::speak(TradeAudioEvent::AlertRisk);

    let Some(tg) = tg else { return };
    if !tg.wants_actions() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plan_tighten_reads_as_hold_not_sell() {
        let exit = json!({
            "symbol": "XLP",
            "action": "sim_exit_plan_tightened",
            "old_profit_limit": 89.67,
            "new_profit_limit": 89.48,
        });
        let (title, detail) = format_closure_telegram(&exit, true).unwrap();
        assert!(title.contains("paper"));
        assert!(detail.contains("still holding"));
        assert!(detail.contains("89.67"));
        assert!(!title.to_lowercase().contains("sold"));
    }

    #[test]
    fn sim_exit_includes_plain_reason_and_pnl() {
        let exit = json!({
            "symbol": "XLE",
            "exit_reason": "thesis_profit_giveback",
            "exit_price": 61.5,
            "pnl_usd": 4.2,
            "pnl_pct": 1.1,
        });
        let (title, detail) = format_closure_telegram(&exit, true).unwrap();
        assert!(title.contains("paper"));
        assert!(detail.contains("peak"));
        assert!(detail.contains("+4.20"));
    }

    #[test]
    fn unknown_adjustment_without_exit_reason_is_skipped() {
        assert!(format_closure_telegram(
            &json!({ "symbol": "X", "action": "something_else" }),
            true
        )
        .is_none());
    }
}
