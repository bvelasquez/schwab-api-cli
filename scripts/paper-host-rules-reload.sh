#!/usr/bin/env bash
# SIGHUP-reload paper agent rules on the remote paper host (process stays up).
#
# Run from an operator machine. Do not run from CI.
# Never passes --trust or --yes.
#
# Env:
#   PAPER_HOST_SSH         SSH target (default: paper-host)
#   PAPER_HOST_REPO        Clone path on the paper host (optional; default ~paper/projects/schwabinvestbot)
#   PAPER_HOST_RUN_USER    Account that owns systemd user units (default: jarvis when SSH is root)
#   PAPER_OPTIONS_RULES    Rules path relative to the repo (default: rules/options-pilot-8709.yaml)
#   PAPER_SWING_RULES      Rules path relative to the repo (default: rules/trader-swing-9947.yaml)

set -euo pipefail

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  sed -n '2,16p' "$0"
  exit 0
fi

if [[ "$*" == *"--trust"* || "$*" == *"--yes"* ]]; then
  echo "refusing: this script must never pass --trust or --yes" >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WRAP_FILE="${SCRIPT_DIR}/lib/paper-host-ssh-wrap.sh"

PAPER_HOST_SSH="${PAPER_HOST_SSH:-paper-host}"
PAPER_HOST_REPO="${PAPER_HOST_REPO:-}"
PAPER_HOST_RUN_USER="${PAPER_HOST_RUN_USER:-}"
PAPER_OPTIONS_RULES="${PAPER_OPTIONS_RULES:-rules/options-pilot-8709.yaml}"
PAPER_SWING_RULES="${PAPER_SWING_RULES:-rules/trader-swing-9947.yaml}"

ssh -o BatchMode=yes "${PAPER_HOST_SSH}" \
  env REPO="${PAPER_HOST_REPO}" \
      OPTIONS_RULES="${PAPER_OPTIONS_RULES}" \
      SWING_RULES="${PAPER_SWING_RULES}" \
      RUN_USER="${PAPER_HOST_RUN_USER}" \
  bash -s <<REMOTE
set -euo pipefail
$(cat "${WRAP_FILE}")
paper_host_reexec_if_root

repo="\${REPO:-\$HOME/projects/schwabinvestbot}"
cd "\$repo"

export PATH="\${HOME}/.cargo/bin:\${PATH}"
command -v schwab >/dev/null || { echo "schwab not on PATH (checked ~/.cargo/bin)" >&2; exit 1; }
command -v schwab-trader >/dev/null || { echo "schwab-trader not on PATH (checked ~/.cargo/bin)" >&2; exit 1; }

echo "==> schwab agent reload \${OPTIONS_RULES}"
schwab agent reload "\${OPTIONS_RULES}"

echo "==> schwab-trader agent reload \${SWING_RULES}"
schwab-trader agent reload "\${SWING_RULES}"
REMOTE
