#!/usr/bin/env bash
# Stop existing schwab / schwab-trader watch processes matching a pgrep -f pattern.
# Sourced by option-start.sh / trader-sim-start.sh for clean restarts.
#
# Usage: stop_matching_watch '<pgrep -f pattern>'

stop_matching_watch() {
  local pattern="$1"
  local pids
  local still

  # -f matches full argv; pattern should include binary + rules file to avoid
  # killing unrelated shells/editors that merely mention the yaml path.
  pids="$(pgrep -f "$pattern" 2>/dev/null || true)"
  if [[ -z "${pids}" ]]; then
    return 0
  fi

  echo "Stopping existing watch (pattern=${pattern}):"
  # shellcheck disable=SC2086
  ps -p ${pids} -o pid=,etime=,command= 2>/dev/null || true
  # shellcheck disable=SC2086
  kill ${pids} 2>/dev/null || true

  # Brief grace period for TUI cleanup / state flush.
  sleep 1
  still="$(pgrep -f "$pattern" 2>/dev/null || true)"
  if [[ -n "${still}" ]]; then
    echo "Force-killing stubborn PIDs: ${still}"
    # shellcheck disable=SC2086
    kill -9 ${still} 2>/dev/null || true
    sleep 0.3
  fi
}
