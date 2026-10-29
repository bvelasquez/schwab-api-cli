# Measurement epochs

`current` numbers must never mix with lifetime numbers. Every book keeps a
ledger at `rules/epochs.json`; only events whose timestamp falls inside the
active epoch count toward current P&L, win rate, exposure, or promotion.

```
scripts/epoch.py status  --book swing-9947
scripts/epoch.py close   --book swing-9947 --reason "strategy change: ..."
scripts/epoch.py open    --book swing-9947 --rules-file trader-swing-9947.yaml \
                         --strategy-version adaptive-v2 --close-reason "..."
```

Lifetime totals live in `rules/lifetime/<book>.json` and are only produced
from **closed** epochs (`scripts/epochs.py: write_lifetime`). Nothing writes
lifetime from live journals.

## Drift

`epochs.py` hashes the rules file. Change the file and the active epoch is
marked `drift: true`; `scripts/scorecard.py` and `scripts/promotion_gate.py`
then refuse to treat the results as comparable (`STRATEGY DRIFT` alert / gate
refusal). This is deliberate: a strategy change invalidates the sample.

A rules edit that happens before any trade in the window may be absorbed by
amending the epoch's `strategy_fingerprint` and recording a `notes` entry —
that is only honest while the window is empty.

## The 2026-10-23 reset

Pre-reset results were archived to `rules/archive/swing-reset-2026-10-23/`
and recorded as the closed epoch `swing-9947-legacy`
(2026-07-01 → 2026-10-23): 47 closes, −$162.98, WR 38.3%, PF 0.72, −4.07%.
The archived journal is a partial slice; the authoritative totals came from
`scorecard-before-reset.json` in that directory. They are retired context,
never "current".

## Deploying changes to jarvis

Edit locally, then:

```
scripts/deploy-jarvis.sh --dry-run          # show what would change
scripts/deploy-jarvis.sh                    # copy only
scripts/deploy-jarvis.sh --restart          # copy + reload running agents
scripts/deploy-jarvis.sh --rollback         # newest backup (or --ts <stamp>)
```

rsync rather than git because the live host holds runtime state (positions,
journals, shadow arms, token mirrors) that must never be overwritten. A tar
backup of `rules/ scripts/ deploy/` lands in `~/backups/schwabinvestbot/`
before anything is written. Rust changes need `--build`.
