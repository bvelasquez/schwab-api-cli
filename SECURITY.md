# Security

## Reporting

Report a vulnerability privately to the maintainer (GitHub security advisories on [bvelasquez/schwab-api-cli](https://github.com/bvelasquez/schwab-api-cli), or the address on the crates.io publisher profile). Please do not open a public issue for a secret leak or an order-placement bug until a fix is published.

## What this project stores

| Data | Where | Mode |
|------|--------|------|
| OAuth access and refresh tokens | platform config dir `tokens.json` | file `0600`, directory `0700` |
| Trading limits | `safety.json` in that config dir | `0600` |
| Agent state and journals | next to your local rules file | `0600` |

The CLI never logs token values. Do not commit any of those files, `.env`, or a rules file that contains an account `hashValue`.

## Never commit

- `.env` (app key and secret, Telegram, OpenRouter, market-data keys)
- `tokens.json`, `safety.json`
- Personal rules YAML (`rules/options-pilot*.yaml`, `rules/trader-swing*.yaml`, `rules/trader-intraday*.yaml`)
- Runtime state, journals, pid files, logs, scorecards, chain snapshots
- Backups of those files (`*.bak*`, `*stale*`)

Copy `rules/options-rules.example.yaml` and `rules/trader-rules.example.yaml` and fill in your own account hash locally.

## OAuth

Schwab refresh tokens last about seven days from the interactive login (`login_at`). Calling the refresh endpoint rotates the access token and does not extend that lifetime. Only one process should refresh a given login. A second refresher revokes the first token.

## Scope

This is experimental trading software. A bug can send a real order when you pass `--trust --yes`. Review [the disclaimer](README.md#disclaimer) before connecting a live account.
