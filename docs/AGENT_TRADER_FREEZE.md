# Schwab stack freeze

The crates and binaries that talk to Schwab today stay in place until an arm is cut over:

- `crates/schwab-api`
- `crates/schwab-market-data`
- `crates/schwab-cli` (`schwab`)
- `crates/schwab-trader` (`schwab-trader`)

Critical bug fixes are the only edits allowed there. New broker work lives in `broker-core`, `broker-schwab`, `broker-public`, `agent-trader-cli`, and `agent-trader-swing`.

Installed `schwab` and `schwab-trader` binaries, and the systemd units under `deploy/systemd/user/schwab-*`, keep running the frozen stack. Nothing in the new stack replaces those units.

## Double-trading guard

Old and new engines must never manage the same account at the same time.

The new stack stores its own state under the `agent-trader` config directory (`~/Library/Application Support/agent-trader` on macOS, `~/.config/agent-trader` on Linux). It does not write `rules/agent-state-*.json` or `rules/trader-state-*.json`.

Before a live order, `agent-trader` and `agent-trader-swing`:

1. Read `agent-*.pid` / `trader-*.pid` next to the rules file (or `AGENT_TRADER_LEGACY_RULES_DIR`).
2. If that pid is alive, read the matching `agent-state-*.json` or `trader-state-*.json`.
3. If the file contains the target account id, refuse the order.
4. Take an exclusive lock file `locks/{broker}-{account}.lock` in the new state directory. A second `agent-trader` process for that account is refused the same way.

`--simulate` and `--dry-run` do not place orders. They still record decisions in the new state directory, not in the legacy files.

`agent-trader guard check --account <id>` prints the decision without trading.

## Retirement

Delete the frozen crates only after the cutover checklist in [AGENT_TRADER.md](AGENT_TRADER.md) is done and no legacy pid is alive. `scripts/agent-trader-cutover-check.sh` refuses to pass while those pids are running. It does not delete anything.
