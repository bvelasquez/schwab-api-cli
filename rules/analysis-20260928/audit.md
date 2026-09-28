# Swing + options paper agents — results audit

**Date:** 2026-09-28 (Mon, pre-open) · **Host:** jarvis · **Binaries:** installed 2026-09-21 08:02/08:04 (`e3e8b0b`)
**Windows:** swing-9947 2026-06-29 → 2026-09-28 (archive `analysis-20260925/…archived-20260925` + live journal);
options-8709 v4.2 paper 2026-08-28 → 2026-09-28 (`agent-sim-journal-options-pilot-8709.jsonl`).

Verdict: **neither agent shows positive ROI.** Swing is statistically flat and trails SPY; options v4.2 is
negative-EV by construction. The larger defect is that no instrument has power: ~13 swing and ~2 option
trades per month cannot arbitrate rule changes, and the LLM learn loop that was meant to adapt was inert.

## 1. Swing (schwab-trader, $4,000 sleeve)

| metric | value |
|---|---|
| closed trades | 38 (14 wins, 37%) |
| mean pnl / trade | −0.62% (sd 5.27, CI95 ≈ [−2.3, +1.1]) |
| realized | +$32.60 (sizes grew over time; early AMD trades were tiny) |
| sleeve equity | $4,000 → $3,990 |
| SPY (benchmark_last) | 741.0 → 771.35 (+4.1%) |
| avg deployment | ≈ 33% of sleeve |
| exits | stop_loss 22 (−4.16%), profit_target 10 (+7.05%), thesis_rs 4 (−1.17%), thesis_regime 2 (+1.11%) |
| avg win / avg loss | +5.32% / −4.09% |

- **Underperforms buy-and-hold and exposure-matched SPY** (33% × 4.1% ≈ +1.35% ≈ $54 vs −$10).
- **Regime split:** July (elevated_vol; 10/19 trades AMD) ≈ −$66; Aug→ (low_vol_trend) ≈ +$99 on 18 trades.
- **Gap-through stops:** AMD 07-07 −4.85%, WMT 08-20 −4.44%, ASML 07-07 −2.58% beyond `stop_price`. A live
  stop-limit OCO may not fill at all on those gaps.
- **Correlated doubles:** IBIT + BITO stopped the same day (08-13/14) and targeted the same day (08-21); no
  `crypto` group in `filters.symbol_groups`, no mega-cap-tech group.
- **Hindsight blocklist:** `blocked_symbols` = recent losers (ASML, CAT, MU, JPM…) — curve fitting.
- **Post-freeze starvation:** 1 entry in 7 sessions. Top scan rejections (live journal since 09-25):
  below SMA 20 (556), blocked_symbol (190), already_open (95), semicap group cap (82), RSI out of range,
  RS < 0, near 52w high.
- **Geometry bug:** `profit_target_recent_range_cap` clamps the range ceiling with `pct.max(0.0)`
  (`capital.rs` ≈322), so a name at/above its 60d high gets a **0% target** → `reward/risk 0.00` rejection.
  With `min_distance_from_52w_high_pct: 3` breakouts are excluded twice.
- **Stale label:** `active_profile_source` still reads `llm` after the 09-18 freeze because
  `apply_regime_profile` returns early when recommended == active (`adaptation.rs` 168-171).
- **Journal:** 217 MB in 3 months (12,271 full `sim_tick_summary` payloads).

## 2. Options (schwab-cli, v4.2 paper)

| date | event | detail |
|---|---|---|
| 08-28 | open QQQ 674/669P 09-30 | credit 0.3895, short Δ −0.158, POP 77%, short **inside 1σ** |
| 09-01 | close "defensive_roll" | debit 0.81, **−$46.10** (−118% of credit); no roll opened — a stop |
| 09-08 | open QQQ 670/665P 10-16 | credit 0.4275 |
| 09-25 | close dte_close | debit 0.65 on `chain_degraded` last-good quotes, **−$25.50** |
| 09-25 | open QQQ 689/684P 10-30 | credit 0.3515; mark 0.68 (underwater, peak −93%) |

Realized **−$71.60**, 0/2 wins.

- **Negative EV by construction.** Credits 7-8% of width; 60% target nets ≈ $20 after 5% slippage; losses
  $25-75 (max $460). Breakeven win rate ≈ 67-80% ≈ entry POP 77-80%. The v4.1 backtest (18 trades, 15 wins,
  +$346) averaged $43 wins on $0.75+ credits — v4.2 halved the win and was never backtested.
- **Roll mislabel:** `try_defensive_roll` closes first, then searches; a failed search leaves the close
  journaled as `defensive_roll`, undercounting stops.
- **Open-bell execution:** every entry and exit at 09:30 ET.
- **Unreachable promotion gates** (`OPTIONS_PILOT_V4_RECOVERY.md` Phase 4): "avg win ≥ avg loss" is
  impossible for a 15Δ credit spread; "zero entries inside 1σ" contradicts v4.2 disabling that gate; ≥15 round
  trips at ~1 per 2-3 weeks is 8+ months.
- **Monitoring bug:** `agent-health.py` printed `cumulative_realized_pnl_usd` (0.0 in simulate by design);
  the ledger is `sim.realized_pnl_usd` (−71.60).
- The Aug-5 `agent-state-options-pilot-8709.json` (cumulative −$211.98) is the retired v3 live state.

## 3. Cross-cutting

- Backtest per-trade CIs span zero at n=13-32 (`analysis-20260918/plan.md`); live paper is slower still.
- LLM learn: 531 patches proposed, 1 applied (`analysis-20260921`). "Dynamic improvement" existed in name only.
- No benchmark-relative, exposure-adjusted, or expectancy-with-CI reporting.
- `market_cache.to` header never advances on per-symbol refresh (shows 2026-07-28; bars are current).

## 4. Actions

Phase 1 (measurement, behavior-neutral): health script, profile-source label, roll label, exit MFE/MAE +
stop-gap slippage, rejection `reason_code`, cache header, journal slimming/rotation.
Phase 2 (monitoring): daily scorecard + drift alerts (timer), **shadow arms** — alternative rule sets run on
the same live tick snapshots with their own sim ledgers, compared day-paired.
Phase 3 (strategy; shadow first, promotion needs Barry's OK): swing pullback SMA9, breakout-allowed
(range-cap floor), correlation groups, open-timing; options credit/width ≥10-12% or 20-30Δ/50% arms, open-bell
and degraded-quote gates, rewritten promotion gates. Options backtests: `options/RESULTS.md`.

## Reproduce

All numbers are from read-only inline Python over the journals/state files named above, run on jarvis
(`ssh jarvis 'cd ~/projects/schwabinvestbot/rules && python3 - <<EOF … EOF'`). `scripts/scorecard.py`
recomputes the swing/options tables.
