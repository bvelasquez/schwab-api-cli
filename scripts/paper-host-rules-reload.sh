#!/usr/bin/env bash
# SIGHUP-reload paper agent rules on the remote paper host (process stays up).
#
# Run from an operator machine. Do not run from CI.
# Never passes --trust or --yes.
#
# Env:
#   PAPER_HOST_SSH      SSH target (default: paper-host)
#   PAPER_HOST_REPO     Clone path on the paper host (default: $HOME/projects/schwabinvestbot)
#   PAPER_OPTIONS_RULES Rules path relative to the repo (default: rules/options-pilot.yaml)
#   PAPER_SWING_RULES   Rules path relative to the repo (default: rules/trader-swing.yaml)

set -euo pipefail

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  sed -n '2,14p' "$0"
  exit 0
fi

if [[ "$*" == *"--trust"* || "$*" == *"--yes"* ]]; then
  echo "refusing: this script must never pass --trust or --yes" >&2
  exit 1
fi

PAPER_HOST_SSH="${PAPER_HOST_SSH:-paper-host}"
PAPER_HOST_REPO="${PAPER_HOST_REPO:-}"
PAPER_OPTIONS_RULES="${PAPER_OPTIONS_RULES:-rules/options-pilot.yaml}"
PAPER_SWING_RULES="${PAPER_SWING_RULES:-rules/trader-swing.yaml}"

ssh -o BatchMode=yes "${PAPER_HOST_SSH}" env \
  REPO="${PAPER_HOST_REPO}" \
  OPTIONS_RULES="${PAPER_OPTIONS_RULES}" \
  SWING_RULES="${PAPER_SWING_RULES}" \
  bash -s <<'REMOTE'
set -euo pipefail

repo="${REPO:-$HOME/projects/schwabinvestbot}"
cd "$repo"

# A non-interactive SSH shell does not get the interactive PATH.
export PATH="$HOME/.cargo/bin:$PATH"
command -v schwab >/dev/null || { echo "schwab not on PATH (checked ~/.cargo/bin)" >&2; exit 1; }
command -v schwab-trader >/dev/null || { echo "schwab-trader not on PATH (checked ~/.cargo/bin)" >&2; exit 1; }

echo "==> schwab agent reload ${OPTIONS_RULES}"
schwab agent reload "${OPTIONS_RULES}"

echo "==> schwab-trader agent reload ${SWING_RULES}"
schwab-trader agent reload "${SWING_RULES}"
REMOTE
