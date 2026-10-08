# Changelog

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
