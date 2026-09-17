# Engine validation of the pullback lead — `require_below_sma` (p2 grid)

**Date:** 2026-09-17 · **Branch:** `feat/require-below-sma` · **Binary:** debug build of `crates/schwab-trader`
**Harness:** `backtest run --fresh --no-learn --simulate --yes --fill-at close`, start $4,000, 58-symbol watchlist
(173-symbol cache; arms share the 2024-06-30→2026-09-17 daily cache), SPY benchmark from the engine itself.

## Why this test
The 2-year daily-bar study (`selection-study.md`) found the admitted set split by the 9-day SMA:
close < SMA9 → +2.04%/10d (t=6.26) vs close ≥ SMA9 → +0.35% (t=1.51); day-level paired, drift cancelled:
**+1.50pp, t=2.96** (significant out-of-sample and in the live window). Lead: buy the shallow pullback
inside the uptrend instead of the extension. Daily bars alone cannot show whether that survives the
engine's exits, sizing, and reward-risk gate — so it was implemented as a rule knob and backtested.

## The change (additive, defaults off)
`playbook.entry.require_below_sma: [9]` — mirror of the existing `require_above_sma`. Empty by default, so
existing rules files are unaffected. Rejects when the symbol is above the listed SMA (`above_sma_N == Some(true)`);
unknown SMA is treated as pass, matching the existing gate's semantics. Unit tests added; full suite 115/115.

## Grid results (all six runs, exit 0)

| arm | window | days | trades | ROI% | SPY% | excess | WR% | expectancy/trade | P&L $ | maxDD% |
|---|---|---|---|---|---|---|---|---|---|---|
| base | pre 2024-06-30→2026-06-28 | 499 | 58 | +10.27 | +33.68 | **−23.41** | 62.1 | +6.86 | +398.16 | 2.49 |
| pullback | pre | 499 | 31 | +5.22 | +33.68 | **−28.46** | 61.3 | +6.33 | +196.09 | 2.78 |
| pullback-widestop | pre | 499 | 9 | −1.01 | +33.68 | −34.69 | 44.4 | −5.44 | −48.96 | 2.62 |
| base | live 2026-06-29→2026-09-17 | 57 | 5 | +1.83 | +2.91 | −1.08 | 60.0 | — | — | — |
| pullback | live | 57 | 2 | +2.15 | +2.91 | −0.76 | 100.0 | +42.59 | +85.18 | 0.70 |
| pullback-widestop | live | 57 | **0** | 0.00 | +2.91 | −2.91 | — | — | — | — |

## Findings

1. **The gate works end-to-end** — not just in unit tests. Recomputing SMA9 from the same cache the engine read:
   pullback arm **32/32** filled entries are at/below SMA9; control arm **29/59** are above it. The rule field
   reaches the decision path and binds.

2. **The daily-bar signal does not survive the engine.** Per-trade expectancy is unchanged
   (base +$6.86 vs pullback +$6.33; WR 62.1% vs 61.3%). The gate only halves the trade count (58→31), so total
   P&L halves ($398→$196) and ROI drops (+10.27%→+5.22%). The 2-year daily study measured a 10-day
   *unconditional* hold; the engine exits on a ~4–5% ATR-capped stop, trailing, thesis, and time stop, and those
   exits capture none of the pullback's advantage — consistent with the study's own finding that the tight stop
   halves the effect (+1.50pp → +0.55pp, t=1.73).

3. **The wider-stop arm is invalid, not negative evidence.** Scaling the exit geometry 1.5x (stop 5.5→8.0,
   PT 8.5→12.0, ATR multiples 2.0→3.0 / 2.5→3.75, horizon 1.0→1.5) kept `target/stop ≥ 1.25` satisfied by
   construction, yet admission collapsed to 9 trades in 2 years and **0 in the live window**. The geometry sits
   on the R:R gate boundary, so the scaled arm chokes on the cap interactions (`min_stop_atr_multiple`, the
   recent-range target cap). The stop hypothesis is **untested**, not refuted.

4. **The strategy loses to buy-and-hold in both windows** (−23.4pp pre, −1.1pp live). That is the dominant
   fact, and entry-gate tuning does not move it.

5. **The frozen backtest trades ~6x less often than the live bot did** (58 trades/499 days = 0.12/day vs the live
   journal's ~0.7/day). The live frequency came from the adaptive/LLM layer — exactly the entries that bled.

## Recommendation
Do **not** promote `require_below_sma` on this evidence. Entry selection is not where the result is lost.
The remaining leverage is exposure/frequency and default stance (cash vs deployed), or accepting the index as
the benchmark to beat. If the stop hypothesis is to be tested, it needs a deliberate geometry design that
decouples the stop from the R:R gate — not a proportional scale of the existing numbers.

## Reproduce
```
target/debug/schwab-trader backtest run --rules-file rules/trader-swing-p2-base-9947.yaml \
  --from 2024-06-30 --to 2026-06-28 --fresh --no-learn --simulate --yes
```
(`*-9947.yaml` arms and `.backtest-cache-*` are gitignored; copies of the live rules with only the arm
differences applied.)
