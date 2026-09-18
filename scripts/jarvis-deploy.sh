#!/usr/bin/env bash
# Deploy schwab + schwab-trader to Jarvis and restart paper systemd user units.
#
# Run from an operator Mac (or any host with SSH to Jarvis). Do not run from CI.
# Never passes --trust or --yes.
#
# Env:
#   JARVIS_SSH   SSH target (default: jarvis — User jarvis via ~/.ssh/config)
#   JARVIS_REPO  Clone path on Jarvis (default: $HOME/projects/schwabinvestbot)

set -euo pipefail

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  sed -n '2,14p' "$0"
  exit 0
fi

if [[ "$*" == *"--trust"* || "$*" == *"--yes"* ]]; then
  echo "refusing: this script must never pass --trust or --yes" >&2
  exit 1
fi

JARVIS_SSH="${JARVIS_SSH:-jarvis}"
# Empty REPO on the remote means "use $HOME/projects/schwabinvestbot".
JARVIS_REPO="${JARVIS_REPO:-}"

ssh -o BatchMode=yes "${JARVIS_SSH}" env REPO="${JARVIS_REPO}" bash -s <<'REMOTE'
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
cp -f deploy/systemd/user/schwab-options-8709.service "$unit_dir/"
cp -f deploy/systemd/user/schwab-swing-9947.service "$unit_dir/"
cp -f deploy/systemd/user/schwab-token-keeper.service "$unit_dir/"
cp -f deploy/systemd/user/schwab-token-keeper.timer "$unit_dir/"
cp -f deploy/systemd/user/schwab-bot-watchdog.service "$unit_dir/"
cp -f deploy/systemd/user/schwab-bot-watchdog.timer "$unit_dir/"

# Token-ownership drop-ins: the agents read the keeper's access-token-only
# mirror so they can never start an OAuth refresh of their own (two refreshers
# on one Schwab account invalidate each other — see docs/JARVIS_PAPER_HOST.md).
for u in schwab-options-8709 schwab-swing-9947; do
  mkdir -p "$unit_dir/$u.service.d"
  cp -f deploy/systemd/user/schwab-agents-token-mirror.conf "$unit_dir/$u.service.d/token-mirror.conf"
done

chmod +x scripts/jarvis-token-keeper.sh scripts/jarvis-bot-watchdog.sh

systemctl --user daemon-reload
systemctl --user enable schwab-options-8709.service schwab-swing-9947.service
systemctl --user enable --now schwab-token-keeper.timer schwab-bot-watchdog.timer
# Seed the mirror before the agents come up: they cannot refresh it themselves.
scripts/jarvis-token-keeper.sh || true
systemctl --user restart schwab-options-8709.service schwab-swing-9947.service

echo "==> versions"
command -v schwab
schwab --version
command -v schwab-trader
schwab-trader --version

echo "==> systemctl --user status"
systemctl --user --no-pager --full status schwab-options-8709.service schwab-swing-9947.service || true

echo "==> pgrep smoke"
pgrep -af 'schwab agent run' || echo "(no schwab agent run process)"
pgrep -af 'schwab-trader agent run' || echo "(no schwab-trader agent run process)"
REMOTE
