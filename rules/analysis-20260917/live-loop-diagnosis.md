# Live-loop diagnosis — trader-swing-9947 (jarvis paper sim)

**Date:** 2026-09-17 · **Window:** 2026-06-29 → 2026-09-17 (51 trading days, 11,187 ticks)
**Source:** `rules/trader-journal-trader-swing-9947.jsonl` on jarvis (199 MB), canonical fill records.
All numbers below are reproducible from the extraction scripts in `/tmp` (see `SKILL.md` → journal corpus).

## Headline

The paper sleeve realized **−$20.89 over 36 closed trades** (mean **−1.11%/trade**, 95% CI [−2.72%, +0.50%], 12W/24L, WR 33%).
Exits are behaving; **the entry selection rule has no edge — it has negative edge**, and the deficit is entirely at entry.

## 1. The gate's admitted opportunity set has negative forward expectancy

Population: every (symbol, trading day) the live gates admitted (journal `candidates`/`attempt` rows carry the
live bid/ask). Only **71 distinct opportunities in 51 days** (≈1.4/day) — the bot is signal-starved and takes
nearly everything that passes (47 signals → 45 fills).

Forward return from the first admissible signal's ask, to the daily close at horizon h:

| horizon | n | mean | 95% CI | win |
|---|---|---|---|---|
| +1d | 65 | −0.98% | [−1.86, −0.10] | 43% |
| +2d | 64 | −1.13% | [−2.24, −0.03] | 36% |
| +3d | 64 | −1.29% | [−2.52, −0.06] | 38% |
| +5d | 62 | −1.55% | [−3.00, −0.10] | 42% |
| +10d | 60 | −2.06% | [−4.00, −0.12] | 32% |
| +20d | 48 | −4.42% | [−7.75, −1.09] | 38% |

Every CI excludes zero on the negative side; expectancy degrades monotonically with holding time.
Median actual hold = 5,730 min (**14.7 trading days**), which lands right on the +10d row — the independent
forward-return estimate and the realized P&L agree, so this is a property of the selection rule, not of the exits.

Selection rule as configured: RSI 48–65 ∧ above SMA20/50 ∧ within 3% of the 52w high ∧ RS vs SPY > 0.
Buying RSI-momentum names sitting within a few percent of their highs produced negative 1–20 day forward
returns across this window.

## 2. Exits are fine — breakeven is 46%, the bot delivers 33%

22 stop_loss (61% of exits), 8 profit_target, 4 thesis_rs_deterioration, 2 thesis_regime.
Avg win +4.83%, avg loss −4.09%, payoff 1.18 → breakeven WR 46% vs actual 33%.
With payoff > 1 and a 13-point WR deficit, the fix has to be entry quality, not stop/target tuning.

## 3. Entry timing: real but minor, and NOT fixed by deferring

Dollar P&L by entry clock time is **size-confounded** — position cost grew 6× mid-window ($122 → $720),
so compare size-neutral `pnl_pct`:

| entry bucket (ET) | n | mean pnl_pct | win | Jul only (n) | Aug–Sep only (n) |
|---|---|---|---|---|---|
| 09:30–10:00 | 18 | **−2.42%** | 17% | −4.29% (10) | −0.08% (8) |
| 10:00–11:00 | 9 | +1.26% | 67% | +1.26% (8) | +1.21% (1) |
| 11:00–13:00 | 7 | −0.45% | 43% | −1.64% (2) | +0.02% (5) |
| 13:00–16:00 | 2 | −2.36% | 0% | −4.66% (1) | −0.06% (1) |

The opening-bell cohort is the worst bucket, but its loss is a **July phenomenon** (−4.29%/trade, 10% WR);
in Aug–Sep it is flat (−0.08%, n=8) and dollar-positive. Unpaired diff vs the rest ≈ 2.6pp, p≈0.11 — suggestive, not established.

**Deferring the entry does not help.** Paired test on the same 71 opportunities (enter at first signal vs first
signal ≥ 10:00 ET): only 12 opportunities were re-admitted later the same day, and the paired difference in
+5d return is **+0.25% ± 0.41 (t=0.6)**; median wait to re-admission 27 min, price drift −0.29%.
An entry-time gate would mostly *skip* trades (same negative expectancy), not improve them.

## 4. Structural finding: the live bot is not running the rules file

Each tick journals an `effective_playbook` with `active_profile_source`. Over the window:
`src` = llm 75% / baseline 25%; regime profile = low_vol_trend 54.7%, elevated_vol 39.3%, baseline 6.0%.
Effective baseline (last tick): PT **7.5** (LLM-adapted down from the yaml's 8.5), stop 5.5, 3 entries/day, rsi 50→48.

- `llm.allow_rule_adaptation: true`, `learn_every_ticks: 6` (≈9 min), `learn_min_closed_trades: 3`,
  bounds ±1.0 PT / ±0.15 risk, applied live, **not persisted** to the state file → parameters drift within each session.
- `veto_entries: true` → **245 of 651 admissible candidates (38%) were blocked as `llm_veto_or_missing_review`**.
- Profile overrides set their own RSI: `elevated_vol` forces 50–65, so the approved playbook floor of 48 does
  **not** reach elevated_vol ticks (39% of the window).

Consequence: no static backtest can model the live bot, and live results are a mixture over many parameter sets.
For measurable results the config has to be frozen (adaptation + veto off) in the measuring variant.

## 5. Actions taken 2026-09-17

- RSI floor 50.0 → 48.0 applied in `rules/trader-swing-9947.yaml` on **jarvis** (backup
  `trader-swing-9947.yaml.bak-20260917-rsi48`) and on the Mac mirror; SIGHUP reload sent (pid 2096423, `status: success`).
  Anchored single-field edit, verified in-file.
- Rules hot-reload confirmed working: the Sep-9 edit propagated to the live effective playbook on 2026-09-10 13:33.
