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

### Why local `cargo build` passes but `cargo publish` fails

`cargo build` in the workspace uses **path dependencies** (`path = "../schwab-api"`), so all crates compile against your latest local source.

`cargo publish` verifies the crate tarball using **crates.io versions** for dependencies (the `version = "…"` field in `Cargo.toml`). If `schwab-api-cli` references APIs added in local `schwab-api-cli-core` but crates.io still has `0.1.0`, publish fails even though the workspace builds.

**Fix:** bump and publish dependencies first (`schwab-api-cli-core` → `schwab-api-cli-market-data` → `schwab-api-cli` → `schwab-trader`), keeping `version =` constraints in sync.

Current minimum versions for the trader stack:

| Package | Notes |
|---------|--------|
| `schwab-api-cli-core` ≥ 0.1.1 | `Tokens::obtained_at`, `refresh_expires_in_seconds()` |
| `schwab-api-cli-market-data` ≥ 0.1.1 | depends on core 0.1.1 |
| `schwab-api-cli` ≥ 0.1.2 | `RuntimeConfig::suppress_tick_output` |
| `schwab-trader` ≥ 0.1.2 | depends on api-cli 0.1.2 |
