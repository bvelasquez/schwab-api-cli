# Options strategy lab (paper only)

Nine paper arms (`O0`..`O8`) run beside the 8709 pilot so that each rule change is tested against a
control instead of argued about. Everything here is paper or read-only. Nothing in the lab changes
`rules/options-pilot-8709.yaml`, places an order, or needs the operator's approval to run.

## Arms

Each arm changes one thing from `O0`. The hypothesis and the result that would falsify it are in the
header comment of `scripts/render-options-arms.py` and show up on the dashboard.

| Arm | Change from O0 | Role |
| --- | --- | --- |
| O0 | none (floors 0.30 / 6%, one slot) | control |
| O1 | second slot, SPY distance relaxed | does a ladder keep per-trade edge |
| O2 | $10 wide, credit floor 0.80 / 8% | fixed trading cost per credit dollar |
| O3 | 21-30 DTE, close at 10 | negative control (more turnover) |
| O4 | delta 0.20-0.25, relaxed distance gates | sensitivity |
| O5 | 55-70 DTE, close at 30 | longer cycle |
| O6 | stop effectively off | does the stop help |
| O7 | IV/RV gate uses a HAR-RV forecast (`rv_estimator: har`) | forecast vs trailing vol |
| O8 | pause when VIX / VIX3M >= 0.90 (`regime.term_structure`) | term-structure stress filter |

Arms are generated, not hand edited:

```bash
python3 scripts/render-options-arms.py          # rewrites rules/arms/options/o*.yaml (idempotent)
./scripts/install-options-arms.sh [o7 o8 ...]   # fills in the account hash, installs units, restarts named arms
```

The generated templates carry `__ACCOUNT_HASH__`. The real hash never enters git.

## Reading the results

Arms that open the same spread on the same day are one bet. Use **distinct trades**, not entries.
The scorecard prints both. With one slot each and the same entry gates, most arms are clones of O0
on most days, so expect effective n to stay far below nominal n.

Nightly (`schwab-lab.timer`, 16:35 ET, after the scorecard), `scripts/lab-nightly.sh` writes:

| File | From | Answers |
| --- | --- | --- |
| `rules/scorecards/latest.json` | `scripts/scorecard.py` | per-agent results, effective n, drift alerts |
| `rules/analysis/slippage-calibration.json` | `scripts/calibrate_slippage.py` | what a fill costs against recorded quotes |
| `rules/benchmarks/latest.json` | `scripts/benchmarks.py` | return on max risk, on the IRA, delta-equivalent alpha, put-write benchmark |
| `rules/analysis/options-promotion-gate.json` | `scripts/options_promotion_gate.py` | which arm, if any, has earned a look |

The dashboard (`schwab-trader dashboard`, port 8791) shows all of it under **Options > Evidence status**.
A file older than 36 hours is marked stale. A missing file says "not generated yet".

## Promotion gate

An arm reports `pass` only when all of these hold. It reports `insufficient` when the data is too
thin to judge, and `fail` when there is enough data and a criterion is unmet.

- 40 weekdays between its first entry and its last journal event
- 20 closed trades the control did not also take
- expectancy per trade with a bootstrap 95% CI above zero
- difference from O0 with a bootstrap 95% CI above zero
- profit factor at least 1.2
- max drawdown of realized P/L no worse than O0's
- Benjamini-Hochberg adjusted p-value at most 0.10 across all tested arms

A pass is a prompt to look, not a production change. Applying anything to 8709 needs the operator's OK,
and live money is a separate decision after paper proof.

## What the benchmarks do and do not say

- **Delta-equivalent P/L** is first order: net delta at entry times the underlying move over the hold.
  Gamma and vega land in the alpha term. Treat alpha as a rough split, not a factor model.
- **Put-write** is a cash-secured SPY short put (25 delta and at-the-money), entered once per cycle,
  filled at 0.66 of the half spread, settled from the last recorded price on expiry day, with the
  collateral charged at a T-bill rate. The IRA size (`BENCH_IRA_EQUITY_USD`), T-bill rate
  (`BENCH_TBILL_PCT`) and fill fraction (`BENCH_FILL_FRACTION`) are manual constants, labelled as such
  in the output.
- Chain recording started 2026-09-28. Until there are many settled cycles, the put-write numbers are a
  working harness, not a result.

## Fill cost

`calibrate_slippage.py` prices 12-20 delta $5-wide verticals from recorded chains at the midpoint and at
the natural price, and reports what crossing half or 0.66 of the spread costs. Since 2026-10-12 each
entry signal also records `market_context.leg_quotes` (both legs' bid and ask, mid and natural credit)
at the moment it is priced, so paper fills can be compared with the quote that existed at fill time.
Older entries are compared with the nearest snapshot, which is up to 15 minutes late and is labelled that
way. Do not change `fill_slippage_pct` on 8709 from the older comparison.

## Signal gates added for the lab (default off)

- `entry_rules.vertical.rv_estimator: har` swaps trailing 20-day vol for a fixed-weight HAR-RV forecast
  in the `min_iv_rv_ratio` gate. Weights are 0.4 daily, 0.3 weekly (5 day), 0.3 monthly (22 day) on
  variance, not fitted. Needs 23 closes. Close-to-close data stands in for intraday realized variance,
  so the daily term is noisy.
- `regime.term_structure: { far_symbol: $VIX3M, pause_ratio_at_or_above: 0.90 }` pauses new entries when
  the front-to-3-month ratio is high. If the gate is set and a quote is missing, entries pause.

## LLM policy

The lab gives the LLM no trade authority. Public evidence for LLM-directed options trading is weak, and
the arms run with the LLM off. If an LLM is added later it should be a constrained input (for example a
catalyst classifier scored with a Brier score in shadow), never the thing that sizes or places the trade.

## Tests

```bash
python3 -m unittest discover -s scripts/tests
cargo test --workspace
```
