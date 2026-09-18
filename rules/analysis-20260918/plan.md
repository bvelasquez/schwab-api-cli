# Plan: can the "not extended" (pullback) entry condition be validated, and does it pay?

**Date:** 2026-09-18 · **Rules under test:** `rules/trader-swing-9947.yaml` sha256 `81236a093a3c…` (frozen: 4 LLM toggles off)
**Cache:** `rules/.backtest-cache-trader-swing-9947.json` sha256 `48bfa9c7…`, fetched 2026-09-18T19:38:14Z, from 2024-06-30, to 2026-07-28 (bars end 2026-07-28), 188 symbols, universe `rules/universe/sp100-liquid.yaml` sha `f0f43d92…` (51 names)

## The hypothesis and why it is live

The 2-year row-level screen (2026-09-17, `selection-study.md`) found the discriminating
condition is **not being extended**: admitted setups with close < SMA9 → f10 +2.04% (t=6.26)
vs close ≥ SMA9 → +0.35% (t=1.51); day-paired +1.50pp (t=2.96, CI [+0.51,+2.50]), OOS +2.02pp
(t=2.87), live window +3.06pp (t=2.94), positive in 12/14 months.
Independently, this session's live-journal counterfactual (n=900 deduped decisions, day-level
paired, drift cancelled — `gate-counterfactual.md`) found the *opposite* sign for what the
current gate admits: admitted vs rejected-below-SMA was −0.87pp @+1d (CI [−1.58,−0.15], 10/33
dates) and −5.96pp @+20d (CI [−10.88,−1.03], 3/20 dates). Two different datasets, same story:
extension loses.

The production lever for this exists in the engine already: `entry.require_below_sma: Vec<u32>`
(rules.rs:263, evaluated at technical.rs:264-276), snapshot field `above_sma_9`
(technical.rs:23,151,176), default `vec![]` = inert, with unit test
`accepts_shallow_pullback_below_sma9` (technical.rs:716).

## BLOCKER found before any tuning can be trusted: the backtest does not evaluate the SMA gates

A controlled A/B was run: `rules/analysis-20260918/arm-C.yaml` (byte-identical to the live
frozen rules, sha `81236a093a3c`) vs `arm-P.yaml` (same + `require_below_sma: [9]`), identical
cache bytes, identical universe bytes, `--no-learn`, `--fresh`, `fill_at close`.

Result: **the two arms are identical in every window** — 27 trades / +10.89% ROI (W1 in-sample),
24 / −1.29% (W2 out-of-sample), 2 / +7.22% (W3 live tail); same expectancy, same exit mix, same
max DD, same traded symbols.

Then `arm-Z.yaml` — the live rules plus a **self-contradictory** gate
(`require_above_sma: [20,50]` AND `require_below_sma: [20]`, which must admit nothing) — also
produced **27 trades / +10.89%** in W1 and 24 / −1.29% in W2, i.e. exactly the control.

Conclusion: in the equity backtest path (`schwab-trader backtest run`) the SMA entry gates —
`require_above_sma` *and* `require_below_sma` — are not binding. The gate code itself is
unambiguous (technical.rs:247/252/257 reject on `above_sma_N == Some(false)`;
267/272/277 reject on `== Some(true)`; both no-op on `None`), so the only way a contradictory
pair admits 27 trades is `above_sma_20 == None` in the replay snapshot. Mechanism localized,
not yet pinpointed: the replay's snapshot construction is the prime suspect (indicator periods
never satisfied / fields left `None`), and that is exactly what Step 1 must nail down.
Consequence: **every prior "pullback arm" result in this repo (`analysis-20260917/bt/pullback_*.txt`,
`trader-backtest-journal-*-p2-pullback-*.jsonl`) measured nothing about the pullback
condition.** The p2-pullback-vs-p2-base fill difference (3 vs 6) is explained by different
rules-file names resolving to different caches, not by the gate.

The **live** path does bind, hard: the live loop journals `scan.rejected[].reason` =
"below SMA 20" **32,650 times** and "below SMA 50" **6,208 times** (`payload.scan.rejected`,
judged from the raw journal, not inferred). `entry_attempts` separately records post-gate
blockers — **245 `llm_veto_or_missing_review`** (the veto now frozen) plus re-entry cooldowns.
So live gates on SMA and the backtest does not; that is the mechanical explanation for the
audit's 0/36 live-vs-backtest entry-set overlap.

## Second, independent defect: the "2-year" cache is not 2 years for most names

`rules/.backtest-cache-trader-swing-9947.json` holds 188 symbols but only **47** have bars from
2024-07-01 (those run 2024-07 → 2026-07/09); the other **141** start between 2025-03-03 and
2026-07-22. Any prior screen described as "2-year, N-symbol" therefore pooled 47 full-history
names with 141 short-history ones — including the 2026-09-17 selection study's counts. Its
direction may well survive, but its "2-year / 12-of-14-months / OOS" framing was computed on a
ragged panel and must be re-derived on an aligned cache before it is treated as a stable
property rather than a window artifact.

## Plan

**Step 0 — build an aligned cache.** Prefetch one continuous window for the whole universe so
every symbol covers every test date (today's cache cannot support a per-symbol 2-year claim).
Record its `fetched_at` and sha256. Without this, any A/B is partly a comparison of *which
symbols happened to have data*, not of the rule.

**Step 1 — repair the instrument (code, Mac→jarvis, no live effect).**
Find why the replay/scan path does not bind the SMA gates (same `run_scan_inner` is called, so
the divergence is in the snapshot the replay feeds it, e.g. SMA fields unpopulated in
`MarketCtx::Replay`). Acceptance test, and it is unambiguous: **after the fix, `arm-Z` must
produce 0 trades**; then `arm-C` vs `arm-P` must differ. Until arm-Z reads 0, the backtest is not
a valid instrument for any gate work.

**Step 2 — walk-forward A/B with the repaired instrument** (same cache bytes, same window,
`--no-learn`, `--fresh`, `fill_at close`, one variable per run):
- `arm-C` — live: `require_above_sma [20,50]`
- `arm-P1` — `+ require_below_sma [9]` (note: with `above [20,50]` held, this is nearly
  self-contradictory — a pullback below SMA9 while above SMA20/50 is rare, so this arm is
  expected to be near-inert by construction and is the honest test of "just add the overlay")
- `arm-P2` — `require_above_sma [50]` + `require_below_sma [9]` (allow a dip that holds the
  50-day — the tradable version of the idea)
- `arm-P3` — `require_below_sma [9]` only (pure mean-reversion arm)
- Windows: W1 2024-07-01→2025-06-30 (in-sample), W2 2025-07-01→2026-06-26 (out-of-sample),
  W3 live tail (cache ends 2026-07-28; a full-length W3 needs a re-prefetch).

**Step 3 — read it honestly.** At ~27 trades per window, every CI spans zero (measured:
W1 CI [−14,+45]$/trade). The trade-level backtest is a *sign/fidelity* check, not proof. The
statistical weight stays with the row-level and live-journal instruments, which have 10-30× the
observations. Accept the change only if the sign agrees in W1 **and** W2 and the live-journal
evidence does not contradict.

**Step 4 — go-live gate (needs Barry's OK, hard rule 3).** Only then propose the one-line diff to
`rules/trader-swing-9947.yaml` on jarvis, and judge it over ≥20 closed trades.

**Parallel: verify the freeze took effect.** Post-freeze the live loop's `entry_attempts` should
carry no `llm_veto_or_missing_review` entries (245 pre-freeze) and `active_profile_source` should
read `regime`, not `llm`.

## What would falsify all of this

If `arm-Z` cannot be made to read 0 trades, the backtest's entry gate is structurally different
from the live one and the only usable instrument remains the live journal — in which case the
plan becomes "change nothing until ≥100 live trades with the frozen config" rather than tuning.
