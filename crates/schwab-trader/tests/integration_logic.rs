//! Logic integration tests (no live Schwab API).

use schwab_trader::agent::state::{SwingPosition, TraderState};
use schwab_trader::closure::{exit_reason_for_position, is_manual_exit_reason};
use schwab_trader::commands::scan_cmd::candidate_score;
use schwab_trader::entry::compute_entry_quantity;
use schwab_trader::learn::adaptation_allowed;
use schwab_trader::orders::{build_exit_oco_order, OcoStatus};
use schwab_trader::rules::TraderRules;
use schwab_trader::technical::{passes_entry_filters, TechnicalSnapshot};

fn sample_snap() -> TechnicalSnapshot {
    TechnicalSnapshot {
        symbol: "NVDA".into(),
        last: 100.0,
        bid: Some(99.8),
        ask: Some(100.2),
        spread_pct: Some(0.4),
        sma_9: Some(98.0),
        sma_20: Some(95.0),
        sma_50: Some(90.0),
        rsi_14: Some(55.0),
        atr_14: Some(2.5),
        volume_sma_20: Some(2_000_000.0),
        relative_volume: Some(1.5),
        above_sma_9: Some(true),
        above_sma_20: Some(true),
        above_sma_50: Some(true),
        intraday: false,
        history_features: None,
    }
}

#[test]
fn missing_indicators_fail_closed() {
    let rules = TraderRules::default();
    let mut snap = sample_snap();
    snap.rsi_14 = None;
    let reason = passes_entry_filters(&snap, &rules.playbook.entry, &rules.technical, &rules);
    assert!(reason.is_some());
    assert!(reason.unwrap().contains("rsi"));
}

#[test]
fn live_adaptation_blocked_by_default() {
    let rules = TraderRules::default();
    assert!(!adaptation_allowed(false, false, &rules, false));
}

#[test]
fn oco_managed_exits_are_not_manual() {
    assert!(!is_manual_exit_reason("stop_loss"));
    assert!(!is_manual_exit_reason("profit_target"));
    assert!(is_manual_exit_reason("eod_flatten"));
}

#[test]
fn oco_present_skips_quote_stop() {
    let rules = TraderRules::default();
    let pos = SwingPosition {
        position_id: "NVDA|2026-06-29".into(),
        symbol: "NVDA".into(),
        account_hash: "hash".into(),
        quantity: 5.0,
        entry_price: 100.0,
        opened_at: chrono::Utc::now(),
        stop_price: 96.0,
        profit_limit: 108.0,
        stop_risk_usd: 20.0,
        market_value_usd: 500.0,
        oco_order_id: Some("999".into()),
        exit_plan_version: 1,
        ..Default::default()
    };
    assert!(exit_reason_for_position(&rules, &pos, 90.0).is_none());
}

#[test]
fn trading_halt_blocks_via_state() {
    let rules = TraderRules::default();
    let mut state = TraderState::default();
    state.trading_halted_reason = Some("unbracketed position: NVDA".into());
    assert!(state.entry_block_reason(&rules).is_some());
}

#[test]
fn oco_order_structure_has_two_legs() {
    let order = build_exit_oco_order("AAPL", 10.0, 110.0, 95.0, 94.5, "GTC");
    let children = order
        .get("childOrderStrategies")
        .and_then(|v| v.as_array())
        .unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(order.get("orderStrategyType").and_then(|v| v.as_str()), Some("OCO"));
}

#[test]
fn blocked_symbol_check() {
    let mut rules = TraderRules::default();
    rules.playbook.filters.blocked_symbols = vec!["GME".into()];
    assert!(rules.is_blocked_symbol("gme"));
    assert!(!rules.is_blocked_symbol("AAPL"));
}

#[test]
fn candidate_ranking_orders_by_score() {
    let good = sample_snap();
    let weak = TechnicalSnapshot {
        rsi_14: Some(15.0),
        relative_volume: Some(0.5),
        spread_pct: Some(1.5),
        ..sample_snap()
    };
    assert!(candidate_score(&good) > candidate_score(&weak));
}

#[test]
fn entry_quantity_zero_on_tiny_budget() {
    let rules = TraderRules::default();
    let qty = compute_entry_quantity(&rules, 500.0, 480.0, 50.0);
    assert!(qty < 1.0);
}

#[test]
fn rules_normalize_fills_default_profiles() {
    let mut rules = TraderRules::default();
    rules.trader_id = "test".into();
    rules.accounts = vec![schwab_trader::rules::TraderAccount {
        hash: "abc".into(),
        label: None,
        r#type: schwab_trader::rules::AccountType::Margin,
        enabled: true,
    }];
    rules.adaptation.profiles.clear();
    rules.normalize_adaptation();
    assert!(rules.adaptation.profiles.contains_key("low_vol_trend"));
    assert!(rules.adaptation.profiles.contains_key("high_vol_chop"));
}

#[test]
fn high_vol_profile_blocks_entries_via_effective_rules() {
    let mut rules = TraderRules::default();
    rules.trader_id = "test".into();
    rules.accounts = vec![schwab_trader::rules::TraderAccount {
        hash: "abc".into(),
        label: None,
        r#type: schwab_trader::rules::AccountType::Margin,
        enabled: true,
    }];
    rules.adaptation = schwab_trader::rules::AdaptationConfig::default_swing();
    let mut state = TraderState::default();
    state.active_profile = Some("high_vol_chop".into());
    let effective = schwab_trader::adaptation::effective_rules(&rules, &state);
    assert_eq!(effective.playbook.entry.max_new_entries_per_day, 0);
}

#[test]
fn oco_status_variants_exist() {
    let _ = OcoStatus::Working;
    let _ = OcoStatus::FilledExit;
}
