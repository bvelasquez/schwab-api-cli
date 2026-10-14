# agent-trader

Broker-neutral CLI beside the frozen Schwab stack. A plan, options rules file, or swing file names the account. That account selects Schwab or Public. One process can drive both.

The frozen `schwab` and `schwab-trader` binaries are unchanged. See [AGENT_TRADER_FREEZE.md](AGENT_TRADER_FREEZE.md).

## Binaries

| Binary | Role |
|---|---|
| `agent-trader` | Accounts, portfolio, quotes, chains, equity plans, options book |
| `agent-trader-swing` | Equity swing book: sleeve, thesis exit, bracket or post-fill OCO |

```bash
cargo install --path crates/agent-trader-cli --force
cargo install --path crates/agent-trader-swing --force
```

Config lives in `~/Library/Application Support/agent-trader` (macOS) or `~/.config/agent-trader` (Linux). `agent-trader import-schwab` copies `safety.json` once. Schwab tokens stay where `schwab auth login` put them.

## Accounts

`schwab:HASH` or `public:ACCOUNT_ID`. A bare hash is Schwab, so existing plan files still parse.

```yaml
accounts:
  - hash: "<schwab hash>"
    broker: schwab
    enabled: true
  - id: "<public account id>"
    broker: public
    enabled: true
```

`PUBLIC_ACCOUNT_ID` fills in when a Public account id is omitted. `data_source: public` on a Schwab account (or the reverse) uses that backend for quotes and chains. The override is printed. It is never a silent fallback.

## What each backend is used for

| Playbook | Schwab | Public |
|---|---|---|
| Equity plan | Limit and market shares, preview, wait for fill | Same, plus notional size. Preflight is the dry run |
| Put-credit vertical | Chain greeks, price from the legs, NET_CREDIT order | Chain greeks, strategy quote when the call succeeds, credit sent as a negative limit |
| Iron condor / custom spread | Up to 4 legs | Up to 6 legs, at most one equity leg |
| Exit bracket | Separate OCO after the entry fill | `orderClass: BRACKET` on the entry when `native_bracket` is set |
| Session gate | Schwab market-hours endpoint | Local US equity calendar (no hours endpoint) |
| Paper | `--simulate` fills locally | Same. Public has no sandbox |

Live orders on either backend require `--trust --yes`. `safety.json` still applies. Public also requires `public_license_ack: true` (personal, non-commercial use). Cash accounts send `useMargin: false`.

## Options book

`agent-trader options tick --rules rules/options-rules.example.yaml --dry-run`

The mechanical book ports entry selection, profit target, the stop as a multiple of credit (`stop_loss_pct: 250` means 2.5× credit), DTE flatten, thesis OTM, giveback, roll-or-close, and flatten. Chain scans are cached for 30 seconds and paced at 10 requests/second on Public.

`entry_policy.require_llm_proceed: true` pauses live entries. The OpenRouter advisor, Telegram, TUI, and backtest lab stay on the frozen `schwab` binary. Exits still run. `--dry-run` and `--simulate` still print the candidate they would open.

`agent-trader options replay --chain chain.json --today 2026-10-13` repeats that selection with no network. The same chain returns the same spread.

## Swing book

`agent-trader-swing shadow --rules rules/trader-rules.example.yaml` reads buying power and quotes, reconciles open positions, and records the entry it would send. A live tick buys inside `capital.fixed_sleeve_cap_usd`, leaves `min_cash_floor_usd`, and attaches a bracket when the backend supports one. Schwab gets a separate OCO after the entry. A position that falls `exits.invalidation_pct` through its average price is a thesis sell.

## Continuous workflows

`agent-trader-swing run` keeps a rules file awake. Each workflow has a 5-field cron, a timezone, an optional inclusive `active_between` window, and a straight list of steps. A step reads the broker (`quote`, `bars`, `session`, `portfolio`, `open_orders`, option `expirations` / `chain` / `greeks`, Public `strategy_quote`), computes `sma`, `ema`, `rsi`, `atr`, `spread_pct`, `change_pct`, or `position`, or places one order. `ema` is a year of daily bars plus the EMA. A short history binds nothing and the order is skipped.

```bash
agent-trader-swing workflow schema --json
agent-trader-swing workflow prompt --json
agent-trader-swing workflow validate --rules rules/workflows/example-msft-dip.yaml
agent-trader-swing run --rules rules/workflows/example-msft-dip.yaml --once --dry-run
```

Checks are `gt`, `gte`, `lt`, `lte`, `eq`, `between`, `session_open`, `buying_power_at_least`, `position_qty`, `min_days_since_fill`, and `spread_pct_at_most`. Names in a check must be earlier steps. Orders are equity (shares or `notional_usd`, market or limit), an entry `bracket` when the broker has `native_bracket`, `exit_bracket`, `cancel`, `wait`, or an explicit `spread`. A spread does not scan for a new position. `safety.json` still blocks option orders while those flags are off.

`agent-trader capabilities` is the source for `fractional`, `native_bracket`, `max_legs`, and `strategy_quote`. Notional orders require `fractional` (Public). Schwab has no native bracket, so the exit is a following `exit_bracket` step. The runner stores the cron slot and the last fill under the agent-trader config directory. A second wake in the same slot does not send another order. `--dry-run` prints the order and does not call `place`.

`rules/workflows/example-msft-dip.yaml` buys $250 of MSFT at 09:30 through 15:30 ET on weekdays when price is above the 200-day EMA and at or below the 20-day EMA, and at least 7 days have passed since the last fill.

`agent-trader-workflows.service` on Jarvis is the workflow agent. It paper-runs every file in `rules/workflows/`, writes `rules/agent-trader-workflow-<stem>.json`, and never calls `place`. The paper dashboard Workflows tab reads that book. Adding a playbook is writing the file. A live order is still `agent-trader-swing run --rules <file> --trust --yes`.

## Canary

Read-only. This does not submit an order.

```bash
agent-trader canary public:<account id>
agent-trader canary schwab:<hash>
```

The Public test account behind `PUBLIC_API_KEY` was unfunded when this stack was added, so `funded` is false and a live order must wait. Preflight may reject for buying power. That rejection is the result, not a cue to retry with a real order.

## Cutover

Do this per arm, after the parity checks below, and only when the legacy pid for that account is gone.

1. `cargo test -p broker-schwab --test parity` — Schwab order JSON matches the frozen builders.
2. `agent-trader options replay` on a saved chain — selection is stable.
3. Run one paper arm with `agent-trader options tick --rules <arm> --simulate` next to the frozen agent and diff the decisions.
4. `agent-trader canary` on a small funded account. Then a one-share plan with `--trust --yes` only when you intend to trade.
5. Point a new systemd unit at `agent-trader`. Examples are `deploy/systemd/user/agent-trader-options.service.example` and `agent-trader-swing.service.example`. Do not disable the `schwab-*` units until that arm is done.
6. Delete `crates/schwab-cli` and `crates/schwab-trader` only after every arm has moved and `scripts/agent-trader-cutover-check.sh` reports no live legacy pid. The script does not delete anything.

`scripts/agent-trader-cutover-check.sh` runs the parity tests and lists live `agent-*.pid` / `trader-*.pid` files. A live pid is a failed check.
