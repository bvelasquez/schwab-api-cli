# Swing / workflow review, 2026-10-23

Context: options are paused (archived to `rules/archive/options-2026-10-23/` on Jarvis). This reviews the
equity side before testing new strategies. Data: Jarvis `trader-state-trader-swing-9947.json` (sim closes,
2026-07-01 to 2026-10-23), `scorecards/latest.json`, `agent-trader-public-swing-paper.json`, workflows,
and the candidate-outcomes study.

## Books now running

| Book | Rules | Sleeve | State |
| --- | --- | --- | --- |
| Schwab swing sim 9947 | `rules/trader-swing-9947.yaml` (SMA20/50 pullback, RSI 48-65, PT 7.5%, stop 5.5%, ATR caps, 4 slots) | $4,000 | 47 closes, equity $3,837 (-4.1%) |
| Public swing paper | `rules/agent-trader-public-swing-paper.yaml` (AAPL/MSFT/NVDA, PT 8%, stop 4%, 3 slots) | $4,000 | 3 closes, 0 wins, realized -$185, equity $3,889 |
| Workflows | `rules/workflows/example-msft-dip.yaml` (MSFT dip above EMA200) | n/a | one example workflow only |
| Shadow arms | 19 arms on 9947 (research-*, big-base, breakout-ok, corr-groups, low-vol-only, pullback-sma9) | per arm | no closed trades recorded yet |

## What the 47 closes say

| Slice | n | Win rate | Mean P/L % | Net USD |
| --- | --- | --- | --- | --- |
| All | 47 | 38% | -0.66 (95% CI -2.08 to +0.79) | -$163 |
| stop_loss exits | 28 | 11% | -4.0 | -$533 |
| profit_target exits | 13 | 100% | +6.4 | +$394 |
| thesis_rs_deterioration | 4 | 0% | -1.2 | -$26 |
| Regime `elevated_vol` | 22 | 23% | -2.65 | -$231 |
| Regime `low_vol_trend` | 20 | 60% | +1.53 | +$75 |
| Regime `neutral` | 5 | 20% | -0.7 | -$5 |

- Profit factor 0.72. Average win +4.85%, average loss -4.09%, so break-even win rate is about 46%. Actual is 38%.
- 60% of exits are stops, and stops hit fast (4.2 days average hold). July averaged 2.6-day holds and -2.3% per
  trade. This looks like entries into noise followed by a stop, not a trend following through.
- Regime is the clearest signal: the book makes money in `low_vol_trend` and loses in `elevated_vol`. The
  `elevated_vol` profile is trading anyway, and it carries the entire loss.
- Last 20 trades: expectancy +0.62%, so recent behavior is better than the full sample. Still inside noise.
- Versus SPY: -3.65 points excess (SPY -0.42% over the window). Deploys 62% on average, so exposure-matched
  excess is -3.8 points. The strategy is not earning its deployment.
- Single names such as AMD (8 trades, 1-day holds), ASML, CAT, AMZN, WMT are the worst per-trade; ETFs and
  crypto proxies (IBIT, BITO, XLE) are better. n is small per symbol.

## Candidate-outcomes study (scripts/candidate_outcomes.py, 277 symbol-days)

| Gate result | n | Mean hypothetical P/L % |
| --- | --- | --- |
| ADMITTED | 13 | -2.13 |
| below_sma rejects | 127 | +0.64 |
| spread_too_wide rejects | 11 | +5.42 |
| rs_below_min rejects | 9 | +1.32 |
| rsi_out_of_range rejects | 55 | -1.91 |

The gates are not sorting winners from losers: the admitted group is the worst-performing bucket. The study
uses overlapping daily windows and is small, so it is a flag, not proof.

## Problems found

1. No baseline. There is no SPY buy-and-hold or trend-filter book running with the same sleeve, so "is
   swing working" has only an exposure-matched ratio to lean on.
2. Entry regime is not gating entries. `elevated_vol` is a trade-everything profile that loses money.
3. The 5.5%/7.5% stop/target on ATR-capped names makes the stop closer than the target in noise terms; most
   stops hit inside 5 days.
4. Public swing paper is tiny (3 trades, 3 symbols, fixed 8%/4%) and not comparable with 9947.
5. Workflows: only one example, no results to compare. They are the right place for rule-based ETF strategies.
6. Shadow arms (19) share the same signals as production and have no closes yet, so they add little until
   the candidate strategies are different, not variants of one pullback rule.
7. `capital.options_risk.rules_files` in `trader-swing-9947.yaml` points at archived options files; it now
   falls back to `fallback_reserve_usd: 500`. Remove that block when options stay paused for good.

## Baseline definition for all comparisons

- Sleeve $4,000, start date 2026-10-26 (next session) for paper; for backtests the first date in the window.
- Costs: 5 bps per side slippage on ETFs and mega caps, 10 bps on others; no commissions (Schwab/Public).
- Baseline A: SPY buy-and-hold. Baseline B: SPY above 200-day SMA else SGOV.
- Report each strategy vs both: return, CAGR, max drawdown, win rate, profit factor, trades, exposure,
  excess return vs SPY, and an out-of-sample split (last 1/3 of the window).
