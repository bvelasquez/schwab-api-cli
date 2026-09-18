# Freeze the LLM layer's decision authority — applied 2026-09-18

**Status:** applied on jarvis, hot-reloaded, parsed view verified. Runtime effect confirms on the
next market session.

## Why

`live-loop-diagnosis.md` §4 established that the paper bot was not running its rules file:

| observation (2026-06-29 → 2026-09-17) | value |
|---|---|
| `effective_playbook.active_profile_source` | **llm 75%** / baseline 25% |
| candidates blocked as `llm_veto_or_missing_review` | **245 of 651 (38%)** |
| `learn_every_ticks` / `learn_min_closed_trades` | 6 (~9 min) / 3 |
| adapted params persisted to the state file? | **no** — they drift within each session |
| `elevated_vol` profile RSI override | forces 50–65, so the approved floor of 48 never reached 39% of ticks |

Consequence: no static backtest can model the bot, and no live result is attributable to the
rules file — the live P&L was a mixture over many LLM-chosen parameter sets. Freezing is the
precondition for measuring anything at all.

## What changed (jarvis `rules/trader-swing-9947.yaml`)

`5caf6d70f637…` → `81236a093a3c…` (sha256, 4 lines). Backup on jarvis:
`rules/trader-swing-9947.yaml.bak-20260918-151607-freeze-llm`

| toggle | before | after | code path |
|---|---|---|---|
| `llm.veto_entries` | true | **false** | `runner.rs:995` → `llm.rs:549` (`veto=false` ⇒ always approved) |
| `llm.allow_rule_adaptation` | true | **false** | `rules.rs:1343`, `learn.rs:47` |
| `simulation.allow_rule_adaptation` | true | **false** | `rules.rs:112` (`backtest_learn_enabled`) |
| `adaptation.llm_profile_select` | true | **false** | `adaptation.rs:212` |

`simulation.allow_rule_adaptation` and `llm_profile_select` are included because both are LLM
steering of the effective config, not decoration — leaving either on would have kept the
measurement mixture intact.

## Deliberately NOT changed

- `llm.enabled: true` — the LLM still runs for web research, monitoring, and journaling
  (`rule_patch_proposed` events remain as a research trail). It simply has no authority.
- `adaptation.enabled: true` + `regime_auto_select: true` — deterministic regime adaptation is
  the mechanism the approved ×4 sizing change was built around (the regime profile overrides
  were scaled ×4 to preserve the risk-appetite ratios). Turning it off would change the tested
  configuration rather than freeze it.

## Verification performed

- `scripts/apply_freeze_llm.py --check` on both hosts (section-scoped, assert-exactly-one):
  only the 4 intended lines, no other `allow_rule_adaptation` touched.
- Post-write `yaml.safe_load` re-parse; `schwab-trader rules validate` → `valid: true`.
- `schwab-trader rules show --json` (the binary's own parsed view):
  `llm.veto_entries=false`, `llm.allow_rule_adaptation=false`,
  `simulation.allow_rule_adaptation=false`, `adaptation.llm_profile_select=false`,
  `llm.enabled=true`.
- SIGHUP reload from the operator Mac (`scripts/jarvis-rules-reload.sh` → `status: success`,
  pid 1075487); the process ticked 10 s later (`last_tick 2026-09-18T22:16:17Z`), so the
  reload did not restart or destabilise it.

## Still to verify (next session)

The after-close ticks are `skipped` (market closed), so no `effective_playbook` is computed
until the next session. First real check: `active_profile_source` must read `regime`, not
`llm`, and `veto_entries=false` must reach `effective_playbook`. The state currently carries
the last LLM-set label (`low_vol_trend`, src `llm`), which is stale rather than live.

## Measurement implication

At the observed 3.23 trades/week, ~8 weeks of frozen config are needed to accumulate another
36 closed trades — the sample size at which the current expectancy CI sits at
[−8.30, +7.14] per trade. Only after that is any rule change measurable live. The arbiter for
entry logic in the meantime is the intraday replay, not daily grid sweeps.

## Rollback

```sh
ssh jarvis 'cd ~/projects/schwabinvestbot && cp rules/trader-swing-9947.yaml.bak-20260918-151607-freeze-llm rules/trader-swing-9947.yaml'
./scripts/jarvis-rules-reload.sh     # run from the Mac
```