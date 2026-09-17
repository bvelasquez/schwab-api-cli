# Selection study — shell A of item 3 (2-year daily cache, no engine port)

**Date:** 2026-09-17 · **Data:** `rules/.backtest-cache-trader-swing-9947.json` (173 symbols, 557 daily bars,
2024-06-30 → 2026-09-17). Scripts: `/tmp/study_selection.py`, `study_window.py`, `study_edge.py`, `study_paired.py`.
Definitions are documented at the top of each script (Wilder RSI14, simple MAs, 252-session high, RS vs SPY).

## Correction to the earlier live-loop diagnosis

The journal-based read said "the gate has negative edge". Over 2 years that is **wrong**:

| gate core (rsi 48–65, >sma20, >sma50, ≥3% below 52w hi, rs30>0) | n | f5 | f10 |
|---|---|---|---|
| pre-live window (2024-06-30 → 2026-06-28) | 1650–1802 | +0.77% (t=4.7) | +1.11–1.22% (t=5.5) |
| **inside** the live window (2026-06-29 → 2026-09-17) | 416 | −0.39% (t=−1.4) | −0.48/−0.80% (t≈−1.5) |

The live window is a **negative stretch that is not statistically significant** (t≈−1.4, CI spans zero), while SPY was
**+2.91%** over the same weeks — i.e. the momentum-near-highs style underperformed, and the gate's excess over SPY in
that window was −1.8pp/10d. So: correct statement = *the gate has a small positive edge over 2 years, no edge over just
holding the index, and it went through a bad 11-week style window.*

## What actually separates winners from losers: not being extended

Day-level **paired** test, market drift cancels (each date, mean f10 of pullback names minus extension names):

| population | n rows | f10 | t | win |
|---|---|---|---|---|
| admitted **and close < SMA9** (shallow pullback) | 672 | **+2.04%** | +6.26 | 56.7% |
| admitted **and close ≥ SMA9** (extension) | 1332 | +0.35% | +1.51 | 49.9% |

- Paired: **+1.50pp, se 0.51, t=+2.96, n_days=208, 95% CI [+0.51, +2.50]**
- Splits: in-sample +0.85pp (t=1.18, n=92) · out-of-sample **+2.02pp (t=2.87)** · live window **+3.06pp (t=2.94)**.
- Positive in 12 of 14 months; in 2026-07 — the month the live bot bled −2.44% excess — the pullback subset was +0.71%.

Mechanism is not RSI: tightening the RSI band (48–55) gave only +0.27% OOS vs the pullback's +1.71%. It is about *where
in the short-term move you buy*. The gate requires above SMA20/50 (uptrend) but says nothing about being extended above
SMA9, so it systematically buys the extension.

## The stop is eating the edge (second lever)

Walk the next 10 sessions, stop −4.5% / target +8.5% (the live-effective levels), n=2004 admitted rows:
**stop fires first 39.7%, target first 19.6%, neither 40.8%.** The stop fires twice as often as the target.

| | pure 10-session hold | with the −4.5%/+8.5% walk |
|---|---|---|
| admitted set, pre-live | +1.11% (t=5.45) | +0.54% (t=4.50) |
| pullback subset | +2.06% | +0.62% |
| day-level paired pullback − extension | +1.50pp (t=2.96) | +0.55pp (t=1.73) |

The stop costs ~0.5pp/trade and halves the pullback advantage. Fixing the entry without widening the stop leaves
most of the gain on the table; the two must be tested together.

## Other variants screened (rejected)

- `rs30 > 10`: strong pre-live (+1.86% f10, e10 +1.60%) but **negative in the live window** (−2.33%) and negative in
  5 of 14 months — a high-beta momentum tilt that fails in exactly the windows the bot already fails in. Not a fix.
- `rs30 > 10 AND pullback`: best pre-live (+4.01% f10, t=5.96, e10 +3.75%, n=251) but only n=18 in the live window.
  Worth an engine test, not worth leading with.
- Distance-from-52w-high bands, ATR% bands, RSI band shifts: no consistent second-period edge.

## Caveats

Twenty conditions were screened; the pullback condition is the one that survived both periods, month-by-month
stability, and a drift-cancelling paired test — but it is still one dataset with overlapping windows, so the t-stats
overstate significance. It is also weaker in the in-sample half (t=1.18) than out-of-sample. **Nothing here has been
run through the real engine** — that is the required next step, and it needs a schema addition (`require_below_sma`),
since the engine can require *above* SMA periods but has no "must be below" analogue.
