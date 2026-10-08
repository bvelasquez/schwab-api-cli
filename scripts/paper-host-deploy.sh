#!/usr/bin/env bash
# Deploy schwab + schwab-trader to a remote paper host and restart paper systemd user units.
#
# Run from an operator machine with SSH to the paper host. Do not run from CI.
# Never passes --trust or --yes.
#
# Env:
#   PAPER_HOST_SSH   SSH target (default: paper-host — set Host/User in ~/.ssh/config)
#   PAPER_HOST_REPO  Clone path on the paper host (default: $HOME/projects/schwabinvestbot)
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

PAPER_HOST_SSH="${PAPER_HOST_SSH:-paper-host}"
PAPER_HOST_REPO="${PAPER_HOST_REPO:-}"

ssh -o BatchMode=yes "${PAPER_HOST_SSH}" env REPO="${PAPER_HOST_REPO}" bash -s <<'REMOTE'
set -euo pipefail

repo="${REPO:-$HOME/projects/schwabinvestbot}"
export PATH="$HOME/.cargo/bin:$PATH"
cd "$repo"

echo "==> git fetch/pull --ff-only origin main"
git fetch origin main
git pull --ff-only origin main

echo "==> cargo install schwab + schwab-trader"
cargo install --path crates/schwab-cli --force
cargo install --path crates/schwab-trader --force

echo "==> install systemd user units"
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
mkdir -p "$unit_dir"
cp -f deploy/systemd/user/schwab-options.service "$unit_dir/"
cp -f deploy/systemd/user/schwab-swing.service "$unit_dir/"
cp -f deploy/systemd/user/schwab-bot-watchdog.service "$unit_dir/"
cp -f deploy/systemd/user/schwab-bot-watchdog.timer "$unit_dir/"
cp -f deploy/systemd/user/schwab-scorecard.service "$unit_dir/"
cp -f deploy/systemd/user/schwab-scorecard.timer "$unit_dir/"
cp -f deploy/systemd/user/schwab-chain-snapshot.service "$unit_dir/"
cp -f deploy/systemd/user/schwab-chain-snapshot.timer "$unit_dir/"

chmod +x scripts/paper-host-watchdog.sh

systemctl --user daemon-reload
systemctl --user enable schwab-options.service schwab-swing.service
systemctl --user enable --now schwab-bot-watchdog.timer schwab-scorecard.timer schwab-chain-snapshot.timer
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
