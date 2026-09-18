# Risk-budget change applied to the jarvis paper bot — 2026-09-17

**Venue rule (Barry): the live/paper test runs ONLY on jarvis.** This MacBook is source code only.
Everything below was measured and deployed on jarvis; the local copies of `rules/*-9947.yaml` are stale
and must never be pushed up (they are gitignored, so `git pull` does not carry them either).

## What changed

`rules/trader-swing-9947.yaml` on jarvis, approved by Barry:

| field | before | after |
|---|---|---|
| `playbook.entry.position_size.risk_per_trade_pct` | 0.75 | **3.0** |
| `playbook.entry.position_size.max_position_pct` | 18.0 | **50.0** |
| `risk.max_portfolio_heat_pct` | 8.0 | **12.0** |
| adaptation profile `elevated_vol` | 0.55 | 2.2 |
| adaptation profile `low_vol_trend` | 0.9 | 3.6 |
| adaptation profile `high_vol_chop` | 0.4 | 1.6 |
| `llm.adaptation_bounds.risk_per_trade_pct.max` | 1.2 | 4.8 |
| `llm.adaptation_bounds.max_delta_per_change` | 0.15 | 0.6 |

Profiles scale ×4 to keep the regime risk ratios intact; the clamp minimum stays 0.3 so the LLM can still
de-risk. `max_drawdown_halt_pct 10`, `max_positions 4`, RSI `[48,65]`, PT 7.5, trail 3.0 are unchanged.

**Why all eight and not the base:** `adaptation.enabled` + `regime_auto_select` means the regime profile's
`overrides` replace the base size, and `llm.adaptation_bounds` clamps it. A base-only edit is cosmetic in
3 of 4 regimes.

## Evidence (jarvis geometry, both windows)

Arms generated from jarvis's live file (`scripts/gen_jr_arms.py`: `j-ctl` verbatim, `j-r3` scaled), run **on
jarvis** with `--fresh --no-learn --simulate --yes`. Raw output in `rules/analysis-20260917/bt/`.

| window | arm | P&L | ROI | max DD | win rate |
|---|---|---|---|---|---|
| 2024-06-30 → 2026-06-28 | j-ctl | +$131.33 | 3.57% | 3.51% | 53.85% |
| 2024-06-30 → 2026-06-28 | **j-r3** | **+$416.93** | **11.55%** | **9.82%** | 53.85% |
| 2026-06-29 → 2026-09-17 | j-ctl | +$71.66 | 1.64% | 0.70% | 60.0% |
| 2026-06-29 → 2026-09-17 | **j-r3** | **+$238.88** | **5.41%** | **2.61%** | 60.0% |

3.2× / 3.3× P&L, **win rate identical** in both windows → pure sizing effect, no signal change.

## Files / rollback

- Applier (dry-run capable, asserts each pattern matches once): `scripts/apply_risk_budget.py --check`
- Arms generator: `scripts/gen_jr_arms.py`
- Backup on jarvis: `rules/trader-swing-9947.yaml.bak-20260917-180457-riskbudget`
- Rollback: restore that backup and `schwab-trader agent reload rules/trader-swing-9947.yaml` (SIGHUP)

## Open risks

1. Pre-window drawdown 9.82% vs the 10% `max_drawdown_halt_pct` — a bad sequence trips the kill-switch,
   which halts permanently. Raising the halt to 12% needs Barry's approval.
2. The underlying edge is thin (~$417/2yr on the $4k sleeve); sizing amplifies losses equally.
3. **The daily backtest does not model the live intraday loop** — the live sim's 36 closed trades were
   −$20.89. Real-world 4× sizing scales the true tick-level edge, which these tables do not measure.
