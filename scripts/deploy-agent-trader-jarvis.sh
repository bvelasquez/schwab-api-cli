#!/usr/bin/env bash
# Deploy the local repo to the Jarvis paper host.
#
# Jarvis is an *installation*, not a git clone. This script:
#   1. rsyncs only git-tracked files (so local-only rules and live state are never
#      deleted or overwritten), then
#   2. rebuilds and reinstalls the binaries on Jarvis (glibc there is older than
#      on a typical dev box, so we build on the host), then
#   3. reloads the systemd user units.
#
# Usage:
#   scripts/deploy-agent-trader-jarvis.sh            # full deploy
#   scripts/deploy-agent-trader-jarvis.sh --sync-only  # rsync only, no build/restart
#
# Environment overrides:
#   JARVIS_SSH   (default: jarvis)
#   JARVIS_USER  (default: jarvis)
#   JARVIS_DEST  (default: /home/$JARVIS_USER/projects/schwabinvestbot)
#   DEPLOY_CRATES (default: "crates/agent-trader-cli crates/agent-trader-swing")
#   DEPLOY_UNITS  (default: the running agent-trader units)

set -euo pipefail

HOST="${JARVIS_SSH:-jarvis}"
RUN_USER="${JARVIS_USER:-jarvis}"
DEST="${JARVIS_DEST:-/home/${RUN_USER}/projects/schwabinvestbot}"
CRATES="${DEPLOY_CRATES:-crates/agent-trader-cli crates/agent-trader-swing}"
UNITS="${DEPLOY_UNITS:-agent-trader-dashboard.service agent-trader-swing-9947.service agent-trader-swing-paper.service agent-trader-workflows.service agent-trader-research.service}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "error: $REPO_ROOT is not a git work tree" >&2
  exit 1
fi

BRANCH="$(git rev-parse --abbrev-ref HEAD)"
HEAD="$(git rev-parse --short HEAD)"
DIRTY="$(git status --porcelain | wc -l | tr -d ' ')"
echo "deploying $BRANCH@$HEAD ($DIRTY uncommitted change(s)) -> ${RUN_USER}@${HOST}:${DEST}"

# 1. Tracked files only. --files-from implies --relative, preserving paths.
#    rsync never deletes, so local-only rules (trader-swing-9947.yaml, ...) and
#    live state stay put.
LIST="$(mktemp)"
trap 'rm -f "$LIST"' EXIT
git ls-files -z > "$LIST"
RSYNC_ARGS=(-a --from0 --files-from="$LIST" --exclude=.git --exclude=target)
if [ "${1:-}" = "--dry-run" ]; then
  RSYNC_ARGS+=(--dry-run --itemize-changes)
fi
rsync "${RSYNC_ARGS[@]}" --rsync-path="sudo -u ${RUN_USER} rsync" ./ "${HOST}:${DEST}/"

if [ "${1:-}" = "--sync-only" ] || [ "${1:-}" = "--dry-run" ]; then
  echo "sync-only/dry-run: skipping build and unit reload"
  exit 0
fi

# 2. Rebuild + install on Jarvis as the run user.
echo "building on ${HOST} as ${RUN_USER}: ${CRATES}"
# shellcheck disable=SC2029
ssh "$HOST" "sudo -u ${RUN_USER} -H bash -lc '
  set -e
  export PATH=\"\$HOME/.cargo/bin:\$PATH\"
  cd ${DEST}
  for c in ${CRATES}; do
    echo \"  cargo install --path \$c --force\"
    cargo install --path \"\$c\" --force
  done
'"

# 3. Reload units.
echo "reloading systemd user units on ${HOST}"
# shellcheck disable=SC2029
ssh "$HOST" "sudo -u ${RUN_USER} -H bash -lc '
  export XDG_RUNTIME_DIR=/run/user/\$(id -u)
  systemctl --user daemon-reload
  systemctl --user restart ${UNITS}
  systemctl --user is-active ${UNITS} || true
'"

echo "deploy complete: ${BRANCH}@${HEAD} -> ${HOST}:${DEST}"
