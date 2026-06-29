# Publishing to crates.io

Crate names on crates.io (the names `schwab-api` and `schwab-cli` are taken by other projects):

| Directory | crates.io package | Binary / lib |
|-----------|-------------------|--------------|
| `crates/schwab-api` | `schwab-api-cli-core` | lib `schwab_api` |
| `crates/schwab-market-data` | `schwab-api-cli-market-data` | lib `schwab_market_data` |
| `crates/schwab-cli` | **`schwab-api-cli`** | bin `schwab` |
| `crates/schwab-trader` | **`schwab-trader`** | bin `schwab-trader` |

Users install with:

```bash
cargo install schwab-api-cli
cargo install schwab-trader
```

## API token (copy from UI, not download)

1. https://crates.io/settings/tokens → **New Token** → copy once
2. `cargo login` and paste

Or set `CRATES_IO_TOKEN` in GitHub Actions secrets (already configured).

## First publish (local or CI)

```bash
bash scripts/publish-crate.sh schwab-api-cli-core
bash scripts/wait-for-crate.sh schwab-api-cli-core
bash scripts/publish-crate.sh schwab-api-cli-market-data
bash scripts/wait-for-crate.sh schwab-api-cli-market-data
bash scripts/publish-crate.sh schwab-api-cli
bash scripts/wait-for-crate.sh schwab-api-cli
bash scripts/publish-crate.sh schwab-trader
```

## CI

Push to `main` (with version bump) or run **Publish crates.io** manually (`workflow_dispatch`).

Requires `CRATES_IO_TOKEN` repository secret.
