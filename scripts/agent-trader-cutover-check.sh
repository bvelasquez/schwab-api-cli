#!/usr/bin/env bash
# Parity and double-trading check for the agent-trader cutover.
# This script does not delete the frozen Schwab crates.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

echo "payload parity and engine tests"
cargo test -p broker-core -p broker-schwab -p broker-public -p agent-trader-cli -p agent-trader-swing --offline -- --skip live_readonly

echo
echo "legacy pids"
busy=0
shopt -s nullglob
for pidfile in rules/agent-*.pid rules/trader-*.pid; do
  pid="$(tr -d '[:space:]' < "$pidfile" || true)"
  if [[ -n "${pid}" ]] && kill -0 "$pid" 2>/dev/null; then
    echo "LIVE $pidfile pid $pid"
    busy=1
  fi
done

if [[ "$busy" -ne 0 ]]; then
  echo
  echo "cutover blocked: a frozen schwab or schwab-trader process is still running"
  exit 1
fi

echo "no live legacy pid. crates were not deleted; remove them only after every arm has moved."
