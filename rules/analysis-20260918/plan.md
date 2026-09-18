# Plan: can the "not extended" (pullback) entry condition be validated, and does it pay?

**Date:** 2026-09-18 (rev 2) · **Rules under test:** `rules/trader-swing-9947.yaml` sha256 `81236a093a3c…` (frozen: 4 LLM toggles off)
**Cache:** `rules/analysis-20260918/.backtest-cache-arm-*.json` sha256 `48bfa9c784b693ae02e2711b…` — **byte-identical for every arm** (copied, not re-fetched), fetched 2026-09-18T19:38:14Z, from 2024-06-30, to 2026-07-28, 188 symbols; universe `rules/universe/sp100-liquid.yaml` sha `f0f43d92…` (51 names)
**Binaries:** `schwab-trader`/`schwab` 0.1.6, rebuilt from `e1003ef` on Mac **and** jarvis 2026-09-18 (see rev-2 root cause)

## The hypothesis and why it is live

The 2-year row-level screen (2026-09-17, `selection-study.md`) found the discriminating
condition is **not being extended**: admitted setups with close < SMA9 → f10 +2.04% (t=6.26)
vs close ≥ SMA9 → +0.35% (t=1.51); day-paired +1.50pp (t=2.96), OOS +2.02pp (t=2.87), live window
+3.06pp (t=2.94), positive in 12/14 months. Independently, this session's live-journal
counterfactual (n=900 deduped decisions, day-level paired, market drift cancelled —
`gate-counterfactual.md`) found the *opposite* sign for what the current gate admits: admitted vs
rejected-below-SMA was −0.87pp @+1d (CI [−1.58,−0.15], 10/33 dates) and −5.96pp @+20d
(CI [−10.88,−1.03], 3/20 dates). Two datasets, same story: extension loses.

The production lever exists in the engine: `entry.require_below_sma: Vec<u32>` (rules.rs:263,
evaluated at technical.rs:264-276), snapshot field `above_sma_9` (technical.rs:23,151,176),
default `vec![]` = inert, unit test `accepts_shallow_pullback_below_sma9` (technical.rs:716).

## Rev-2 root cause: the deployed binary predated the feature (the gates were NEVER inert)

Rev 1 of this document claimed the backtest's SMA gates were structurally inert. **That was
wrong.** The controlled arms were inert because the *installed binaries* on jarvis were dated
2026-09-16 07:33/07:34, one day **before** `require_below_sma` was added in `bef2b4c`
(2026-09-17). `EntryConfig` carries `#[serde(default)]` and no `deny_unknown_fields`, so the
installed binary **silently dropped** the unknown YAML key and the field kept its default `[]`.

Evidence chain (each step reproducible):
1. `schwab-trader rules show rules/analysis-20260918/arm-Z.yaml --json` on jarvis listed
   `playbook.entry` keys as `[max_new_entries_per_day, max_positions, max_spread_pct,
   min_avg_volume_20d, min_price_usd, position_size, require_above_sma, rsi_14_range]` —
   `require_below_sma` **absent from the parsed output** while present at YAML line 49.
   `rules show` echoes parsed rules, so it is ground truth for what the engine will enforce.
2. `git log -S require_below_sma` → `bef2b4c` (2026-09-17). `ls -la ~/.cargo/bin/` → 2026-09-16.
3. After `make install` on both hosts, `rules show` reports `C: below=[]`, `Z: below=[20]`.
4. Re-run of the probe matrix: **`arm-Z` → 0 fills, 0 exits in both windows.** Acceptance test
   passes; the gate is evaluated and binds exactly as the source says.

So the rev-1 lesson stands in a different form: **a null A/B is a binary-staleness candidate, not
a finding.** A stale deploy also silently weakens the *live* paper agent, which runs the same-day binary.

## Corrected gate-probe results (byte-identical cache, one variable, `--fresh --no-learn`)

| arm | entry gates | W1 fills / net$ / exp$ | W2 fills / net$ / exp$ |
|---|---|---|---|
| **C** | above `[20,50]` (live) | 29 / +428.99 / 15.89 | 25 / −96.85 / −4.04 |
| **P** | above `[20,50]` + below `[9]` | 14 / +264.17 / 20.32 | 16 / −132.22 / −8.81 |
| **P3** | below `[9]` only | 14 / +264.17 / 20.32 | 16 / −132.22 / −8.81 |
| **P2** | above `[50]` + below `[9]` | 22 / +237.70 / 11.32 | 27 / −113.12 / −4.52 |
| **Y** | above `[200]` | 32 / −259.45 / −8.11 | 29 / −156.02 / −5.38 |
| **Z** | above `[20,50]` + below `[20]` (contradictory) | **0 / 0 / 0** | **0 / 0 / 0** |

Two structural findings:
- **Deleting a key does not unset it.** `arm-P3` (no `require_above_sma` in the file) behaves
  exactly like `arm-P`, because a missing key falls back to `EntryConfig::default()` =
  `require_above_sma: [20,50]`. Only *changing* a value is observable; a "remove the gate" arm
  proves nothing.
- **`require_below_sma [9]` dominates `require_above_sma [20,50]`** (P ≡ P3 to the trade), i.e.
  everything below SMA9 is already below SMA20 in this sample.

**Statistical read (`gate-probe-stats.json`): every per-trade expectancy CI spans zero and no
Welch test is significant.** W1: C 15.89 [−15.71,+47.49] vs P 20.32 [−20.44,+61.08], diff +4.43,
t=0.18 (crit 2.05). W2: C −4.04 [−43.70,+35.63] vs P −8.81 [−69.20,+51.57], diff −4.78, t=−0.14.
The backtest changes the *trade set* by half and cannot distinguish the expectancy at n=13-32.
It is a fidelity instrument, not an arbiter.

## Consequence for prior work — studies that must be re-run

The `bef2b4c` gate was unborn in the binary during **every** backtest run before 2026-09-18, so:
- `analysis-20260917/selection-study.md`'s row-level screen, its `bta-cohort.yaml` pullback arm,
  and `bt/pullback_*.txt` are **not** measurements of the pullback condition;
- the p2-pullback-vs-p2-base fill difference (3 vs 6) was a cache/name artifact, not the gate.
Their *row-level* numbers (computed from the cached bars, not the engine) may survive — that is a
separate question from whether the engine applied the key — but any claim about an **arm** must
be re-run on a binary built from ≥ `bef2b4c`.

## Second, independent data defect: the "2-year" cache is not 2 years for most names

`rules/.backtest-cache-trader-swing-9947.json` holds 188 symbols but only **47** have bars from
2024-07-01; the other **141** start between 2025-03-03 and 2026-07-22. Any "2-year, N-symbol"
screen therefore pooled 47 full-history names with 141 short-history ones. An aligned cache is a
precondition for re-running the selection study, not just for A/B arms.

## Plan (rev 2)

**Step 0 — DONE.** `aligned.yaml` (same rules/universe as `arm-C`) prefetched `--from 2024-06-30
--to 2026-07-28 --force`: **58/58 symbols, 520 bars each, all starting 2024-07-01, zero gaps >5
calendar days**, `fetched_at 2026-09-18T23:44:33Z`, cache sha256 `5b722a08c9ae8049…`. This proves
the raggedness was a **fetch artifact, not an API limit** — an explicit `--from/--to` + `--force`
gets uniform 2-year coverage.

Replication on that cache (`gate-probes-aligned.json`): **every arm reproduces the ragged-cache
result to the last decimal** (C W1 27 closed / +428.9911870125044 both times). Cause, verified:
the 11 universe names missing pre-2025-07-28 in the old cache (DUK GLD KO NEE NEM PEP PG SO USMV
XLP XLU — all defensives) gain 250 W1 bars in the aligned cache but were never admitted, and the
symbols that *did* trade are value-identical across the two caches (0 differences on 520 common
bars × 11 names). So the gate finding is robust to the cache defect; **the ragged panel only
matters for studies that pooled the other ~130 names** (the selection study), which still needs
re-derivation on an aligned panel.

**Step 1 — DONE (accepted).** Instrument rebuilt and validated: `arm-Z` = 0 trades. Also note the
live paper agents kept the Sep-16 image in memory; the new binary takes effect on their next
restart (the watchdog does this on a stale tick). Deliberate restart is Barry's call — it changes
what the running paper test is executing, though the live rules file itself uses no key that
`bef2b4c` touched.

**Step 2 — DONE for the gate question** (table above). No arm shows a significant per-trade
effect.

**Step 3 — the arbiter is the live journal, not the backtest.** Weight stays with the n≈900
day-paired counterfactual (`gate-counterfactual.md`): it is the only instrument with power, and
its sign (extension loses, admitted < rejected by 5.96pp @+20d) agrees with the row-level screen.
Per skill rule: under ~100 trades the backtest differences are noise unless the mechanism is
independently plausible — here the mechanism *is* plausible (a pullback filter halves churn), so
the honest statement is "mechanism real, effect size unmeasured".

**Step 4 — go-live gate (needs Barry's OK, hard rule 3).** The one-line diff to
`rules/trader-swing-9947.yaml` on jarvis (`+ require_below_sma: [9]`), judged over ≥20 closed
live trades. Not before Step 0/3 are finished, because the row-level screen behind it was computed
on the ragged panel.

**Parallel — freeze verification.** Post-freeze `entry_attempts` should carry no
`llm_veto_or_missing_review` (245 pre-freeze) and `active_profile_source` should read `regime`.

## What would falsify this plan

If, on a rebuilt binary and an aligned cache, the live-journal counterfactual's sign flips or its
CI straddles zero, the pullback hypothesis is dead and the plan becomes "change nothing until
≥100 live trades under the frozen config". The backtest cannot kill it (no power) — only the live
journal or a much longer aligned walk-forward can.
