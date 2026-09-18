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

# A non-interactive SSH shell does not get the interactive PATH, so the
# installed CLIs are not on it by default -- without this the script dies
# with "schwab: command not found" and silently fails to reload anything.
export PATH="$HOME/.cargo/bin:$PATH"
command -v schwab-trader >/dev/null || { echo "schwab-trader not on PATH (checked ~/.cargo/bin)" >&2; exit 1; }

echo "==> schwab agent reload"
schwab agent reload rules/options-pilot-8709.yaml

echo "==> schwab-trader agent reload"
schwab-trader agent reload rules/trader-swing-9947.yaml
REMOTE
