# Trade plans

Multi-step rebalances for the Schwab CLI. Plans are **YAML or JSON** files that an LLM (or you) produces; the CLI **validates** every step against `safety.json` and **executes** them sequentially.

## Quick start

```bash
# Discovery for agents
schwab plan schema --json
schwab plan prompt --json

# Validate + dry-run (no orders sent)
schwab plan validate plans/9947-sgov-to-jpst-rebalance.yaml
schwab plan run plans/9947-sgov-to-jpst-rebalance.yaml --dry-run --json

# Live (requires explicit trust)
schwab plan run plans/9947-sgov-to-jpst-rebalance.yaml --trust --yes --json

# Resume from a step
schwab plan run plans/9947-sgov-to-jpst-rebalance.yaml --from-step step-04-buy-jpst-1 --trust --yes
```

## LLM workflow

1. Read `schwab portfolio summary --json`, `schwab safety show --json`, `schwab accounts numbers --json`.
2. Split trades so **each step** stays under `max_trade_value_usd`, `max_shares_per_order`, and `max_trade_pct_of_equity`.
3. Use **limit** orders with explicit `limit_price` (refresh before live run).
4. Order steps: **sell before buy** when rotating cash.
5. Write a plan file under `plans/` matching the schema.
6. Run `schwab plan validate` then `--dry-run` before live execution.

Run `schwab plan prompt --json` for the full machine-readable prompt, rules, and YAML template.

## Files in this folder

| File | Purpose |
|------|---------|
| `TRADE_PLAN.md` | Human + LLM reference (field definitions, examples) |
| `example-sgov-to-jpst-rebalance.yaml` | Example staged SGOV → JPST rotation (placeholders) |

## Design principles

- **Plans are data, not code** — auditable, diffable, reviewable before `--trust --yes`.
- **CLI is the enforcement layer** — safety.json limits apply per step regardless of what the LLM wrote.
- **Human in the loop** — default safe mode; autonomous execution only with `--trust --yes`.
