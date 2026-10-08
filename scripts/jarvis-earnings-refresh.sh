#!/usr/bin/env bash
# Confirmed earnings dates. No-op without an FMP key. Does not edit the
# production playbook; it only rewrites the calendar file the rules point at.
set -euo pipefail

repo="${SCHWAB_REPO:-$HOME/projects/schwabinvestbot}"
cd "$repo"
export PATH="$HOME/.cargo/bin:$PATH"

if [[ -z "${FMP_API_KEY:-}" ]]; then
  echo "FMP_API_KEY unset; earnings refresh skipped"
  exit 0
fi

if [[ -f rules/trader-swing.yaml ]]; then
  rules=rules/trader-swing.yaml
elif [[ -f rules/trader-swing-9947.yaml ]]; then
  rules=rules/trader-swing-9947.yaml
else
  echo "missing rules/trader-swing.yaml; earnings refresh skipped" >&2
  exit 0
fi

schwab-trader research earnings-refresh --rules-file "$rules" --json
