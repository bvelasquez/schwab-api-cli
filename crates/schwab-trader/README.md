# schwab-trader

Equity swing/intraday trading agent for the [Charles Schwab Trader API](https://developer.schwab.com/) — rules-driven scan, LLM selection, post-fill OCO brackets, broker reconciliation, and paper simulation.

Published by **[Soki Creative](https://soki-creative.com)**. Companion to [`schwab-api-cli`](https://crates.io/crates/schwab-api-cli) (options agent + core `schwab` CLI).

Install:

```bash
cargo install schwab-trader
```

The `schwab-trader` binary is on your PATH after install. Requires Schwab OAuth via `schwab auth login` from `schwab-api-cli`.

## ⚠️ USE AT YOUR OWN RISK — EXPERIMENTAL SOFTWARE

This crate is **experimental** and under active development. It can place **real orders** when run with `--trust --yes`. Bugs, API changes, LLM misjudgments, and misconfiguration can cause **financial loss**.

- **Not financial, investment, tax, or legal advice**
- **No affiliation** with Charles Schwab & Co., Inc.
- **No warranty** — provided “AS IS” under the MIT License
- **You are solely responsible** for every order, compliance, and loss

Before live trading:

```bash
schwab disclaimer accept --yes   # from schwab-api-cli
```

Prefer `--dry-run` or `--simulate` until you understand every flag and rules file.

## Quick start

```bash
# Auth (shared with schwab-api-cli)
schwab auth login

# Validate rules
schwab-trader rules validate rules/trader-rules.example.yaml --json

# Paper trading one tick
schwab-trader agent run rules/my-trader.yaml --simulate --once --json
```

## Documentation

- [Trader rules & playbook](https://github.com/bvelasquez/schwab-api-cli/blob/main/docs/TRADER_RULES.md)
- [Live rollout checklist](https://github.com/bvelasquez/schwab-api-cli/blob/main/docs/TRADER_ROLLOUT.md)
- [Full project README](https://github.com/bvelasquez/schwab-api-cli)

## License

MIT — see [LICENSE](https://github.com/bvelasquez/schwab-api-cli/blob/main/LICENSE).
