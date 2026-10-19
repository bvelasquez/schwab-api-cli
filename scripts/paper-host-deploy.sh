#!/usr/bin/env bash
# Deploy schwab + schwab-trader to a remote paper host and restart paper systemd user units.
#
# Run from an operator machine with SSH to the paper host. Do not run from CI.
# Never passes --trust or --yes.
#
# Env:
#   PAPER_HOST_SSH         SSH target (default: paper-host — set Host/User in ~/.ssh/config)
#   PAPER_HOST_REPO        Clone path on the paper host (optional; default ~paper/projects/schwabinvestbot)
#   PAPER_HOST_RUN_USER    Account that owns systemd user units (default: jarvis when SSH is root)
#   PAPER_HOST_ALLOW_GENERIC_UNITS  Set to 1 to deploy generic units even when the host
#                          already has account-specific units (e.g. -8709/-9947).
#
# OAuth: Schwab allows one refresh-token owner per app login. Refresh tokens on
# exactly one host. A second long-running process that refreshes will revoke the first.

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
PAPER_HOST_ALLOW_GENERIC_UNITS="${PAPER_HOST_ALLOW_GENERIC_UNITS:-0}"

ssh -o BatchMode=yes "${PAPER_HOST_SSH}" \
  env REPO="${PAPER_HOST_REPO}" \
      RUN_USER="${PAPER_HOST_RUN_USER}" \
  bash -s <<REMOTE
set -euo pipefail
$(cat "${WRAP_FILE}")
paper_host_reexec_if_root

# Refuse to clobber a host that already has account-specific paper units (for
# example schwab-options-8709.service / schwab-swing-9947.service on Jarvis).
# Those hosts run their own deploy script; the generic units below would point
# at rules files that do not exist and would trip the watchdog.
allow_generic_units="${PAPER_HOST_ALLOW_GENERIC_UNITS}"
unit_dir="\${XDG_CONFIG_HOME:-\$HOME/.config}/systemd/user"
if [[ "\$allow_generic_units" != "1" ]]; then
  shopt -s nullglob
  host_units=( "\$unit_dir"/schwab-options-*.service "\$unit_dir"/schwab-swing-*.service )
  shopt -u nullglob
  if (( \${#host_units[@]} > 0 )); then
    echo "refusing: this host already has account-specific paper units:" >&2
    printf '  %s\n' "\${host_units[@]##*/}" >&2
    echo "Use the host's own deploy script instead (for example scripts/jarvis-deploy.sh)," >&2
    echo "or set PAPER_HOST_ALLOW_GENERIC_UNITS=1 to override." >&2
    exit 1
  fi
fi

repo="\${REPO:-\$HOME/projects/schwabinvestbot}"
export PATH="\${HOME}/.cargo/bin:\${PATH}"
cd "\$repo"

echo "==> git fetch/pull --ff-only origin main"
git fetch origin main
git pull --ff-only origin main

echo "==> cargo install schwab + schwab-trader"
cargo install --path crates/schwab-cli --force
cargo install --path crates/schwab-trader --force

echo "==> install systemd user units"
unit_dir="\${XDG_CONFIG_HOME:-\$HOME/.config}/systemd/user"
mkdir -p "\$unit_dir"
cp -f deploy/systemd/user/schwab-options.service "\$unit_dir/"
cp -f deploy/systemd/user/schwab-swing.service "\$unit_dir/"
cp -f deploy/systemd/user/schwab-bot-watchdog.service "\$unit_dir/"
cp -f deploy/systemd/user/schwab-bot-watchdog.timer "\$unit_dir/"
cp -f deploy/systemd/user/schwab-scorecard.service "\$unit_dir/"
cp -f deploy/systemd/user/schwab-scorecard.timer "\$unit_dir/"
cp -f deploy/systemd/user/schwab-chain-snapshot.service "\$unit_dir/"
cp -f deploy/systemd/user/schwab-chain-snapshot.timer "\$unit_dir/"
cp -f deploy/systemd/user/schwab-outcomes.service "\$unit_dir/"
cp -f deploy/systemd/user/schwab-outcomes.timer "\$unit_dir/"
cp -f deploy/systemd/user/schwab-llm-signal.service "\$unit_dir/"
cp -f deploy/systemd/user/schwab-llm-signal.timer "\$unit_dir/"
cp -f deploy/systemd/user/schwab-llm-agent.service "\$unit_dir/"
cp -f deploy/systemd/user/schwab-llm-agent.timer "\$unit_dir/"
cp -f deploy/systemd/user/schwab-research-agent.service "\$unit_dir/"
cp -f deploy/systemd/user/schwab-research-agent.timer "\$unit_dir/"
cp -f deploy/systemd/user/schwab-earnings-calendar.service "\$unit_dir/"
cp -f deploy/systemd/user/schwab-earnings-calendar.timer "\$unit_dir/"
cp -f deploy/systemd/user/schwab-dashboard.service "\$unit_dir/"

chmod +x scripts/paper-host-watchdog.sh

systemctl --user daemon-reload
systemctl --user enable schwab-options.service schwab-swing.service
systemctl --user enable --now schwab-bot-watchdog.timer schwab-scorecard.timer schwab-chain-snapshot.timer \
  schwab-outcomes.timer schwab-llm-signal.timer schwab-llm-agent.timer \
  schwab-research-agent.timer schwab-earnings-calendar.timer
systemctl --user enable --now schwab-dashboard.service
systemctl --user restart schwab-options.service schwab-swing.service

echo "==> versions"
command -v schwab
schwab --version
command -v schwab-trader
schwab-trader --version

echo "==> systemctl --user status"
systemctl --user --no-pager --full status schwab-options.service schwab-swing.service || true

echo "==> pgrep smoke"
pgrep -af 'schwab agent run' || echo "(no schwab agent run process)"
pgrep -af 'schwab-trader agent run' || echo "(no schwab-trader agent run process)"
REMOTE
