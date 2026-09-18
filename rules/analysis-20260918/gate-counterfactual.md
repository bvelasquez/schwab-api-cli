# What the live swing bot's own decision grid says about its entry gate

**Run:** 2026-09-18 · `scripts/analyze_gate_counterfactual.py` (read-only) ·
artifact JSON `rules/analysis-20260918/gate-counterfactual.json`

**Provenance**
- journal `rules/trader-journal-trader-swing-9947.jsonl` — 12,078 lines, 2026-06-29T18:08Z → 2026-09-18T19:59Z
- rules `rules/trader-swing-9947.yaml` sha256 `81236a093a3c…a97790` (**frozen** — LLM veto/adaptation off)
- daily cache `rules/.backtest-cache-trader-swing-9947.json` — `fetched_at` 2026-09-18T19:38Z, 188 symbols, bars through 2026-09-17
- git `d053d8e`

## Method

The agent journals every tick's `scan` block: `candidates` (passed the entry gate) and
`rejected` (with a reason). That grid is a natural experiment. Rows are deduplicated to **one
observation per (symbol, day)** — a persistent candidate reappears on every tick it stays true,
so the raw 169,455 rejected rows are **not** a sample size. Precedence: ADMITTED > rejection.

Returns are measured from the **decision-minute price the bot actually saw**
(`technical_context.last`) and, separately, from the decision day's close so the horizon is
comparable to SPY. CIs are reported two ways: t-based, and a **symbol-clustered bootstrap**
(resampling symbols, 4,000 draws) — repeated/overlapping observations on the same name are not
independent.

900 deduplicated observations over the window, in 11 classes.

## Results

| class | n | +1d | +3d | +5d | +10d | +20d | excess @+20d |
|---|---|---|---|---|---|---|---|
| **ADMITTED** (passed gate) | 71 | −0.88 | −1.17 | −1.28 | −1.92 | **−1.93** | −2.62 |
| **gate:trend_below_sma** (rejected: below SMA20/50) | 405 | **+0.32** | **+0.68** | +0.54 | +1.12 | **+3.76** | **+2.17** |
| gate:rsi_band (RSI outside 48–65) | 124 | −0.16 | −1.01 | −1.27 | −1.53 | **−3.94** | **−4.21** |
| gate:dist_from_high (>3% from 52w high) | 53 | +0.09 | −0.01 | −0.51 | −0.55 | −2.35 | −3.67 |
| gate:price_floor (below min price) | 62 | −0.30 | −0.15 | −0.65 | −1.82 | **−4.74** | −3.79 |
| risk:stop_geometry (stop < 1.5×ATR) | 24 | −1.76 | −3.57 | −4.24 | **−5.00** | −6.63 | −5.64 |
| portfolio:already_open (held already) | 43 | −0.08 | −0.28 | +0.21 | +2.04 | +6.20 | +3.66 |
| portfolio:group_cap (symbol-group cap) | 24 | −0.47 | −1.22 | −1.19 | −2.52 | −1.96 | −5.51 |
| blocked_symbol (separate mechanism) | 90 | −1.34 | −0.54 | +0.12 | — | — | — |

Mean forward return, %, from the decision day's close. Full CIs in the JSON.

## Findings

1. **The trend filter looks inverted in this window.** Setups the bot *rejects for being below
   SMA20/SMA50* went on to **+3.76% at +20d (win 63.3%)** and beat SPY (+2.17pp), while the 71
   setups it **admitted** returned **−1.93%** and lost to SPY (−2.62pp). At +1d the rejected
   cohort's excess is +0.17pp with *both* CI styles excluding zero; the admitted set's excess is
   −0.89pp with the t-CI excluding zero. Gap between the two cohorts ≈ **5.7pp over 20 days**.
   Mechanism: in a rising tape, below-trend names are the dip/mean-reversion candidates, and the
   gate systematically skips them in favour of extended ones.
2. **The other gates earn their keep.** RSI-band rejections (−3.94% @+20d, symbol-clustered CI
   [−6.65, −1.48]), distance-from-high rejections, the price floor, and the ATR stop-geometry
   rejection (rejected names fell −5.00% @+10d, −6.63% @+20d, clustered CI excludes zero) all
   identify losers. Only the trend filter has the sign wrong.
3. **The portfolio blocks are not the problem.** `group_cap` blocks look correct (−1.96%);
   `already_open` setups (n=27 with a full +20d) returned +6.20% — i.e. the sleeve paid an
   opportunity cost for not adding to held winners, but n is small and truncated.
4. **API failures are journaled as entry rejections.** 16 × `API error 401`, 5 × `429`, 3 × `500`
   appear as `rejected` reasons. A data outage silently becomes "no trade" and pollutes the
   decision grid. Worth a distinct outcome value so it never counts as a gate decision.

## What this does NOT establish

- **Not walk-forward.** One window (Jun 29 – Sep 18, 2026), one regime. Per the audit standard this
  is a *hypothesis generator*, not a validated rule change.
- **Truncation is severe at the long horizons.** The cache ends 2026-09-17, so 209 of 405
  below-SMA observations and 30 of 71 admitted observations have no +20d print. Those figures come
  from the earlier part of the window only — the tail of the window is unmeasured.
- **Clustered CIs are wider and mostly include zero** for the 10d/20d admitted set. The robust
  claim is the short-horizon one (+1d/+3d, both CI styles) plus the *sign* of the 20-day gap.
- The rejected cohorts are not random samples of the universe; they are the names the gate
  happened to see. Cross-cohort comparison controls for holding period and (via excess) for beta,
  not for selection.

## Proposed next experiment (needs a decision, not yet run)

Walk-forward test of the trend gate on the daily engine, one variable at a time, same cache and
window, `--no-learn`:
1. control: current frozen rules
2. arm A: `require_above_sma` off / relaxed (both SMA gates)
3. arm B: mean-reversion arm — admit below-SMA names with the same RSI band + a dip trigger

Then the intraday arbiter: replay on Schwab 1-minute bars. The data path is confirmed and free —
`schwab market history --frequency-type minute --frequency 1` returns the whole reachable window
per symbol in one call, and the floor is a **rolling ~45 days (currently 2026-08-04, i.e. 33
trading days)** regardless of how far back you ask. Note the engine gap found while building this:
`MarketCtx::Replay` serves **daily** bars for *any* requested config, so intraday series are absent
from the backtest path entirely — the minute replay needs the cache plumbed through, not just new
flags.
