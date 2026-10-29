#!/usr/bin/env bash
# Nightly options strategy-lab evidence: slippage calibration, benchmarks, promotion gate.
# Read-only with respect to trading. Reads rules/ state, journals, and recorded chains;
# writes only rules/analysis/ and rules/benchmarks/. Runs every step even when one fails,
# then exits nonzero so systemd marks the unit failed.
# PAUSED 2026-10-23: options are archived; schwab-lab.timer is disabled. Resume: docs/OPTIONS_STRATEGY_LAB.md.
set -u
here="$(cd "$(dirname "$0")" && pwd)"
repo="${SCHWAB_REPO:-$(dirname "$here")}"
py="${PYTHON:-python3}"
status=0
run() {
  echo "== $*"
  if ! "$@"; then
    echo "FAILED: $*" >&2
    status=1
  fi
}
run "$py" "$here/calibrate_slippage.py" --repo "$repo"
run "$py" "$here/benchmarks.py" --repo "$repo"
run "$py" "$here/options_promotion_gate.py" --repo "$repo"
exit "$status"
