# Trader rollout checklist

Use this before enabling live capital on account 9947.

## Prerequisites

- [ ] `schwab auth login` — shared token store
- [ ] `safety.json` — `allow_conditional_orders: true` (required for OCO brackets)
- [ ] `schwab-trader rules validate rules/trader-swing-9947.yaml --json` passes
- [ ] `adaptation.live_auto_apply: false` in rules YAML (default)

## Phase 1 — Simulation soak (2+ weeks)

```bash
schwab-trader agent run rules/trader-swing-9947.yaml --simulate --once --json
schwab-trader sim stats --rules-file rules/trader-swing-9947.yaml --json
```

Verify in tick output:

- `reconcile_report` present (empty in sim)
- `drawdown` tracks sleeve equity
- `monitoring.unbracketed_count` stays 0
- `closure_exits` fire correctly on time stops

## Phase 2 — Dry-run with exit evaluation (1 week)

```bash
schwab-trader agent run rules/trader-swing-9947.yaml --dry-run --once --json
```

Dry-run now evaluates exits (`closure_exits` with `dry_run: true`) without placing orders.

## Phase 3 — Live with reduced sleeve

1. Lower `capital.fixed_sleeve_cap_usd` to $500 temporarily
2. Run one tick with approval:

```bash
schwab disclaimer accept --yes
schwab-trader agent run rules/trader-swing-9947.yaml --trust --yes --once --json
```

3. Confirm `reconcile_report` matches broker after restart
4. Confirm `last_fill_to_bracket_seconds` < `place_bracket_within_seconds`

## Phase 4 — Full sleeve

- Restore `fixed_sleeve_cap_usd: 4000`
- Do **not** run swing + intraday agents simultaneously on 9947 (sibling sleeve guard reduces cap)
- Enable `adaptation.live_auto_apply: true` only after 10+ closed sim/live trades with positive expectancy

## Monitoring alerts (tick JSON)

| Field | Action if non-zero |
|-------|-------------------|
| `monitoring.unbracketed_count` | Halt entries; fix bracket manually |
| `monitoring.reconcile_mismatch_count` | Review journal; check broker positions |
| `monitoring.trading_halted_reason` | Resolve before new entries |
| `drawdown.halted` | Wait for recovery or manual review |

## Never

- Run live without reconciliation (now automatic every tick)
- Manually flatten while OCO is working (agent cancels OCO first for time/EOD exits only)
- Enable `live_auto_apply` before sim soak completes
