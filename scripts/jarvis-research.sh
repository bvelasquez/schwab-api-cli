#!/usr/bin/env bash
# After-close research pass. Writes journals and proposals. Does not edit
# production rules and does not restart the swing agent.
set -euo pipefail

repo="${SCHWAB_REPO:-$HOME/projects/schwabinvestbot}"
cd "$repo"
export PATH="$HOME/.cargo/bin:$PATH"

if [[ -f rules/trader-swing.yaml ]]; then
  rules=rules/trader-swing.yaml
elif [[ -f rules/trader-swing-9947.yaml ]]; then
  rules=rules/trader-swing-9947.yaml
else
  echo "missing rules/trader-swing.yaml; research pass skipped" >&2
  exit 0
fi

echo "==> research outcomes"
schwab-trader research outcomes --rules-file "$rules" --output rules/candidate-outcomes.jsonl --json

echo "==> IC eval (a fail is a result, not a crash)"
set +e
python3 scripts/llm_ic_eval.py --decisions rules/llm-signal-journal.jsonl --outcomes rules/candidate-outcomes.jsonl --json > rules/llm-ic-report.json
ic_status=$?
set -e
echo "IC eval exit $ic_status"

echo "==> research agent proposals"
python3 scripts/research_agent.py --outcomes rules/candidate-outcomes.jsonl --ic rules/llm-ic-report.json --dest rules/arms/proposed
echo "done. No production rules were changed."
