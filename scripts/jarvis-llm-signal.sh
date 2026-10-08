#!/usr/bin/env bash
# Premarket named + blinded scores. No-op without an OpenRouter key.
set -euo pipefail

repo="${SCHWAB_REPO:-$HOME/projects/schwabinvestbot}"
cd "$repo"
export PATH="$HOME/.cargo/bin:$PATH"

if [[ -z "${OPENROUTER_API_KEY:-}" ]]; then
  echo "OPENROUTER_API_KEY unset; premarket scorer skipped"
  exit 0
fi

if [[ -f rules/trader-swing.yaml ]]; then
  rules=rules/trader-swing.yaml
elif [[ -f rules/trader-swing-9947.yaml ]]; then
  rules=rules/trader-swing-9947.yaml
else
  echo "missing rules/trader-swing.yaml; scorer skipped" >&2
  exit 0
fi

schwab-trader research signal --rules-file "$rules" --json
