#!/bin/bash
# Walk-forward arm test: arm-C (live frozen rules) vs arm-P (+ require_below_sma [9]).
# Same cache bytes, same window, same flags, one changed variable. Backtest only, --no-learn.
export PATH="$HOME/.cargo/bin:$PATH"
set -a; . "$HOME/.config/environment.d/schwab-paper.conf"; set +a
cd "$HOME/projects/schwabinvestbot" || exit 1

# The arm rules files live in a subdir, so the relative candidate_pool_file path must
# resolve there too — byte-identical copy so the universe matches the live config.
mkdir -p rules/analysis-20260918/universe
if [ ! -f rules/analysis-20260918/universe/sp100-liquid.yaml ]; then
  cp -p rules/universe/sp100-liquid.yaml rules/analysis-20260918/universe/sp100-liquid.yaml
fi
shasum -a 256 rules/universe/sp100-liquid.yaml rules/analysis-20260918/universe/sp100-liquid.yaml | cut -c1-20,66-
echo "host=$(hostname) rev=$(git rev-parse --short HEAD) started=$(date -u +%FT%TZ)"

run() {
  arm=$1; from=$2; to=$3; tag=$4
  echo ">>> arm-$arm $tag $from..$to"
  schwab-trader backtest run --rules-file "rules/analysis-20260918/arm-$arm.yaml" \
    --from "$from" --to "$to" --fresh --no-learn -j > "/tmp/arm$arm-$tag.json" 2> "/tmp/arm$arm-$tag.err"
  rc=$?
  cp -p "rules/analysis-20260918/trader-backtest-journal-arm-$arm.jsonl" "/tmp/trades-arm$arm-$tag.jsonl" 2>/dev/null
  echo "    exit=$rc json_bytes=$(wc -c < /tmp/arm$arm-$tag.json) trades_file_bytes=$(wc -c < /tmp/trades-arm$arm-$tag.jsonl 2>/dev/null)"
}

run C 2024-07-01 2025-06-30 W1
run P 2024-07-01 2025-06-30 W1
run C 2025-07-01 2026-06-26 W2
run P 2025-07-01 2026-06-26 W2
run C 2026-06-29 2026-07-28 W3
run P 2026-06-29 2026-07-28 W3
echo "done=$(date -u +%FT%TZ)"