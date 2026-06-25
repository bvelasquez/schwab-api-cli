# schwab-api-cli

Agent-first CLI for the [Charles Schwab Trader API](https://developer.schwab.com/) — trade plans, options agent, safety guardrails, and LLM-driven workflows.

Published by **[Soki Creative](https://soki-creative.com)**.

Install:

```bash
cargo install schwab-api-cli
```

The `schwab` binary is on your PATH after install.

## ⚠️ USE AT YOUR OWN RISK — EXPERIMENTAL SOFTWARE

This crate is **experimental** and under active development. It can place **real orders** in your brokerage account when run with `--trust --yes`. Bugs, API changes, LLM misjudgments, and misconfiguration can cause **financial loss**.

- **Not financial, investment, tax, or legal advice**
- **No affiliation** with Charles Schwab & Co., Inc.
- **No warranty** — provided “AS IS” under the MIT License
- **You are solely responsible** for every order, compliance, and loss
- **Authors and contributors are not liable** for damages or financial loss

Before live trading:

```bash
schwab disclaimer show
schwab disclaimer accept --yes
```

Prefer `--dry-run` until you understand every flag and config file. Full disclaimer: [repository README](https://github.com/bvelasquez/schwab-api-cli#disclaimer).

## Quick start

```bash
cp .env.example .env   # SCHWAB_APP_KEY, SCHWAB_APP_SECRET
schwab auth login
schwab disclaimer accept --yes
schwab capabilities --json
```

## Documentation

- [Full README & options agent guide](https://github.com/bvelasquez/schwab-api-cli)
- [LLM schema reference](https://github.com/bvelasquez/schwab-api-cli/blob/main/docs/LLM_SCHEMA_REFERENCE.md)
- [Homepage](https://soki-creative.com)

## License

MIT — see [LICENSE](https://github.com/bvelasquez/schwab-api-cli/blob/main/LICENSE).
