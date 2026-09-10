#!/usr/bin/env bash
# SIGHUP-reload paper agent rules on Jarvis (process stays up).
#
# Run from an operator Mac. Do not run from CI.
# Never passes --trust or --yes.
#
# Env:
#   JARVIS_SSH   SSH target (default: jarvis)
#   JARVIS_REPO  Clone path on Jarvis (default: $HOME/projects/schwabinvestbot)

set -euo pipefail

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  sed -n '2,12p' "$0"
  exit 0
fi

if [[ "$*" == *"--trust"* || "$*" == *"--yes"* ]]; then
  echo "refusing: this script must never pass --trust or --yes" >&2
  exit 1
fi

JARVIS_SSH="${JARVIS_SSH:-jarvis}"
JARVIS_REPO="${JARVIS_REPO:-}"

ssh -o BatchMode=yes "${JARVIS_SSH}" env REPO="${JARVIS_REPO}" bash -s <<'REMOTE'
set -euo pipefail

repo="${REPO:-$HOME/projects/schwabinvestbot}"
cd "$repo"

echo "==> schwab agent reload"
schwab agent reload rules/options-pilot-8709.yaml

echo "==> schwab-trader agent reload"
schwab-trader agent reload rules/trader-swing-9947.yaml
REMOTE
