# Changelog

## Unreleased

`agent-trader`. Research-paper arm (Public.com quotes, web search, zero orders).

- Phase 3 of `docs/RESEARCH_ARM_PLAN.md` landed. `agent-trader research run` drives the loop: forecast, local paper fills, bounded playbook patches, and forward-return scoring.
- Paper book: slot sizing from simulated equity, ask/bid fills with `slippage_bps`, stop/target/ATR-trailing/time/thesis exits, a one-day re-entry cooldown after a stop or thesis exit, and a `max_daily_loss_usd` kill switch that survives restarts.
- Entry guard reads RSI(14) from the playbook band; indicators see only bars before the decision date.
- Playbook patches are versioned to `rules/research/<file>.versions/` with an `active.json` pointer; `--apply-patches` activates, `research rollback` reverts. Only the `playbook` subtree is read back, so an overlay cannot change account, mode, gate, universe or credentials.
- Forecast scoring writes `scores-<arm>.jsonl` and a scorecard: hit rate, mean rank IC, day-clustered t, and the 20-day stage-B gate from `rules/analysis/llm-ic-eval.json`.
- Paper dashboard: the research book renders as a normal book, and the Research tab shows paper equity, return versus SPY, scorecard, patch note and per-tick history.
- New `deploy/systemd/user/agent-trader-research.service.example`. Paper only: `--trust`/`--yes` are rejected on the research path.

## 0.1.9 — 2026-10-13

`schwab-api-cli` and `schwab-trader`. Same library crates as 0.1.8 (`schwab-api-cli-core` 0.1.2, `schwab-api-cli-market-data` 0.1.1).

- Options agent searches the chain for the short strike, records the bid and ask at entry, and can use a HAR-RV forecast or a VIX term-structure pause. Both gates stay off unless a rules file turns them on.
- `schwab-trader dashboard` serves the paper books page: options payoff and health, and a swing chart of entry, stop, and target for each open share.

## 0.1.8 — 2026-10-08

`schwab-api-cli` and `schwab-trader`. Same library crates as 0.1.7 (`schwab-api-cli-core` 0.1.2, `schwab-api-cli-market-data` 0.1.1).

- Dependency patches: `rustls` 0.23.45 (RUSTSEC-2026-0285), `webbrowser` 1.2.4 (RUSTSEC-2026-0257), and transitive `quinn-proto` 0.11.19 (RUSTSEC-2026-0185).
- Minimum supported Rust is 1.85, matching `webbrowser` 1.2.2+.

## 0.1.7 — 2026-10-08

`schwab-api-cli` and `schwab-trader`. Requires `schwab-api-cli-core` 0.1.2. `schwab-api-cli-market-data` stays 0.1.1.

- OAuth token files are written mode `0600` in a mode `0700` directory. `safety.json`, agent state, journals, and agent logs that can hold account data are mode `0600`. Existing loose files are tightened on the next read or write.
- Refresh-token age is tracked from the interactive login (`login_at`), not from the last access-token refresh. The refresh token still expires about seven days after login.
- Equity trader: shadow arms (paper comparison, no orders), capital ledger that reserves options risk on a shared account, and `agent run --background`.
- Options agent: degraded-quote handling holds a `dte_close` while the short leg is still far out of the money; roll and execution gates from the September audit.
- Public tree no longer contains operator host notes, account identifiers, or website-login automation. Paper agents on another machine are documented in `docs/REMOTE_PAPER_HOST.md`.

## 0.1.6 — 2026-07-31

Range-aware stock targets and regime-mismatch option exits.

## 0.1.5 — 2026-07-24

Options backtest harness.

## 0.1.4 — 2026-07-09

Thesis deterioration exits.

## 0.1.3 — 2026-07-06

CLI and trader publish alignment.

## 0.1.2

`schwab-api-cli-core` only (this release): owner-only credential files and `login_at`.

## 0.1.1 — 2026-06-29

`schwab-api-cli-core` and `schwab-api-cli-market-data`: `Tokens::obtained_at` and refresh lifetime helpers.

## 0.1.0 — 2026-06-25

Initial crates.io release.
