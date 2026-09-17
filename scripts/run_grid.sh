#!/bin/bash
# Run a backtest grid: bash scripts/run_grid.sh <arm> [<arm> ...]
# Each arm must have rules/trader-swing-<arm>-9947.yaml. Runs both windows, fresh state.
cd /Users/barryvelasquez/projects/schwabinvestbot || exit 1
OUT=rules/analysis-20260917/bt
mkdir -p "$OUT"
for arm in "$@"; do
  for spec in "2024-06-30|2026-06-28|pre" "2026-06-29|2026-09-17|live"; do
    from=${spec%%|*}; rest=${spec#*|}; to=${rest%%|*}; lbl=${rest##*|}
    target/debug/schwab-trader backtest run \
      --rules-file "rules/trader-swing-${arm}-9947.yaml" \
      --from "$from" --to "$to" --fresh --no-learn --simulate --yes \
      > "$OUT/${arm}_${lbl}.txt" 2>&1
    echo "$arm/$lbl exit=$?"
  done
done
echo "grid done"
