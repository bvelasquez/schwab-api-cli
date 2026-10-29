#!/usr/bin/env bash
# Deploy this working tree to the Jarvis paper-trading host.
#
#   scripts/deploy-jarvis.sh --dry-run          # show what would change
#   scripts/deploy-jarvis.sh                    # copy code + rules, no restart
#   scripts/deploy-jarvis.sh --restart          # copy, then restart the swing agent
#   scripts/deploy-jarvis.sh --bootstrap-epoch  # first deploy: record the legacy
#                                               # epoch, open the adaptive one
#   scripts/deploy-jarvis.sh --rollback         # restore the newest backup
#   scripts/deploy-jarvis.sh --rollback --ts 20261028T153000
#
# Why rsync instead of git: the live host carries runtime state (positions,
# journals, shadow arms, token mirrors) that is never in git and must never be
# overwritten by a deploy. Every deploy state file is excluded below and the
# live tree is tarred to ~/backups/schwabinvestbot/ first.
#
# What is NOT excluded: scripts/, crates/, rules/*.yaml, deploy/, docs/, tests/.
# Rust is only rebuilt when --build is passed (a rules/script deploy needs no
# binary), and a restart is opt-in so a bad deploy cannot silently re-arm the
# agent.
set -euo pipefail

HOST="${JARVIS_SSH:-jarvis@100.90.93.111}"
REPO="${JARVIS_REPO:-/home/jarvis/projects/schwabinvestbot}"
BACKUP_DIR="${JARVIS_BACKUP_DIR:-/home/jarvis/backups/schwabinvestbot}"
UNIT="${SWING_UNIT:-agent-trader-swing-9947.service}"
LOCAL="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

DRY=0 RESTART=0 BUILD=0 BOOTSTRAP=0 ROLLBACK=0
TS=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY=1 ;;
    --restart) RESTART=1 ;;
    --build) BUILD=1 ;;
    --bootstrap-epoch) BOOTSTRAP=1 ;;
    --rollback) ROLLBACK=1 ;;
    --ts) TS="${2:?--ts needs a value}"; shift ;;
    -h|--help) sed -n '2,26p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
  shift
done

# Runtime state. Never copied, never deleted.
EXCLUDES=(
  --exclude='.git/'
  --exclude='target/'
  --exclude='__pycache__/'
  --exclude='.venv/'
  --exclude='.env'
  --exclude='.env.*'
  --exclude='tokens/'
  --exclude='*.log'
  --exclude='*.pid'
  --exclude='rules/trader-state-*.json'
  --exclude='rules/trader-journal-*.jsonl'
  --exclude='rules/trader-shadow-*.json'
  --exclude='rules/trader-shadow-*.jsonl'
  --exclude='rules/agent-trader-*.json'
  --exclude='rules/agent-trader-*.jsonl'
  --exclude='rules/epochs.json'
  --exclude='rules/lifetime/'
  --exclude='rules/deploy-manifest.json'
  --exclude='rules/lab/'
  --exclude='rules/lab-data/'
  --exclude='rules/archive/'
  --exclude='rules/analysis/'
  --exclude='rules/analysis-*/'
  --exclude='scripts/__pycache__/'
  --exclude='.pytest_cache/'
  --exclude='*.pyc'
  --exclude='rules/scorecards/'
)

ssh_do() { ssh -o BatchMode=yes -o ConnectTimeout=10 "$HOST" "$@"; }

if [[ $ROLLBACK -eq 1 ]]; then
  echo "==> rollback on $HOST"
  ssh_do bash -s <<REMOTE
set -euo pipefail
repo="$REPO"; backup_dir="$BACKUP_DIR"; unit="$UNIT"
target="\${1:-}"
if [[ -n "\$target" ]]; then
  archive="\$backup_dir/\$target.tar.gz"
else
  archive="\$(ls -1t "\$backup_dir"/*.tar.gz 2>/dev/null | head -1)"
fi
[[ -f "\$archive" ]] || { echo "no backup found in \$backup_dir" >&2; exit 1; }
echo "restoring \$archive"
tar xzf "\$archive" -C "\$repo"
systemctl --user restart "\$unit"
sleep 2
systemctl --user is-active "\$unit" || true
REMOTE
  exit 0
fi

echo "==> preflight (local)"
cd "$LOCAL"
git rev-parse --short HEAD >/dev/null 2>&1 || { echo "not a git repo" >&2; exit 1; }
SHA="$(git rev-parse --short HEAD)"
DIRTY="$(git status --porcelain | wc -l | tr -d ' ')"
python3 -m py_compile scripts/epochs.py scripts/epoch.py scripts/scorecard.py \
  scripts/promotion_gate.py scripts/morning_review_agent.py
python3 - <<'PY'
import yaml, pathlib, sys
path = pathlib.Path("rules/trader-swing-9947.yaml")
data = yaml.safe_load(path.read_text())
profiles = (data.get("adaptation") or {}).get("profiles")
assert isinstance(profiles, dict), "adaptation.profiles block missing"
for name, prof in profiles.items():
    entry = (prof.get("overrides") or {}).get("entry") or {}
    if "max_new_entries_per_day" in entry:
        print(f"  {name}: max_new_entries_per_day={entry['max_new_entries_per_day']}")
filters = (data.get("playbook") or {}).get("filters") or {}
print("  min_distance_from_52w_high_pct=", filters.get("min_distance_from_52w_high_pct"))
print("  near_52w_high_soft_zone_pct=", filters.get("near_52w_high_soft_zone_pct"))
PY
python3 -m pytest tests/test_epochs.py -q

echo "==> target checks"
ssh_do "test -d '$REPO' && command -v rsync >/dev/null && echo target-ok"

if [[ $DRY -eq 1 ]]; then
  echo "==> dry run (no changes written)"
  rsync -azn --itemize-changes "${EXCLUDES[@]}" -e "ssh -o BatchMode=yes" \
    "$LOCAL/" "$HOST:$REPO/" | sed -n '1,200p'
  exit 0
fi

STAMP="$(date -u +%Y%m%dT%H%M%S)"
echo "==> backup live tree on $HOST -> $BACKUP_DIR/$STAMP.tar.gz"
ssh_do bash -s <<REMOTE
set -euo pipefail
repo="$REPO"; backup_dir="$BACKUP_DIR"
mkdir -p "\$backup_dir"
tar czf "\$backup_dir/$STAMP.tar.gz" -C "\$repo" \
  --exclude=./target --exclude=./.git .
ls -1t "\$backup_dir"/*.tar.gz | tail -n +11 | xargs -r rm -f
du -sh "\$backup_dir/$STAMP.tar.gz"
REMOTE

echo "==> rsync $LOCAL -> $HOST:$REPO"
rsync -az --delete --itemize-changes "${EXCLUDES[@]}" -e "ssh -o BatchMode=yes" \
  "$LOCAL/" "$HOST:$REPO/"

if [[ $BUILD -eq 1 ]]; then
  echo "==> cargo install schwab-trader (release)"
  ssh_do "export PATH=\$HOME/.cargo/bin:\$PATH; cd '$REPO' && cargo install --path crates/schwab-trader --force 2>&1 | tail -5"
fi

echo "==> remote verification"
ssh_do bash -s <<REMOTE
set -euo pipefail
cd "$REPO"
python3 -m py_compile scripts/epochs.py scripts/epoch.py scripts/scorecard.py \
  scripts/promotion_gate.py scripts/morning_review_agent.py
python3 - <<'PY'
import yaml
d = yaml.safe_load(open("rules/trader-swing-9947.yaml"))
print("rules ok: profiles =", sorted(((d.get("adaptation") or {}).get("profiles") or {}).keys()))
PY
cat > rules/deploy-manifest.json <<JSON
{"sha": "$SHA", "deployed_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)", "by": "hermes@beast",
 "local_dirty_files": $DIRTY, "backup": "$BACKUP_DIR/$STAMP.tar.gz",
 "build": $([[ $BUILD -eq 1 ]] && echo true || echo false)}
JSON
cat rules/deploy-manifest.json
REMOTE

if [[ $BOOTSTRAP -eq 1 ]]; then
  echo "==> bootstrap epoch ledger (legacy -> closed, adaptive -> active)"
  ssh_do bash -s <<REMOTE
set -euo pipefail
cd "$REPO"
if [[ -f rules/epochs.json ]]; then
  echo "ledger already exists; leaving it alone"
  python3 scripts/epoch.py status --book swing-9947 || true
else
  # Pre-reset history, recorded as retired.
  python3 scripts/epoch.py import-archive --book swing-9947 \
    --archive rules/archive/swing-reset-2026-10-23 \
    --id swing-9947-legacy --reason "reset 2026-10-23; retired from current metrics" \
    --rules-file trader-swing-9947.yaml
  # The adaptive strategy starts now: its numbers share nothing with legacy.
  python3 scripts/epoch.py open --book swing-9947 \
    --rules-file trader-swing-9947.yaml \
    --strategy-version adaptive-v1 \
    --start-equity "\$(python3 - <<'PY'
import json
try:
    s = json.load(open("rules/trader-state-trader-swing-9947.json"))
    print(s.get("sim", {}).get("cash") or 4000.0)
except Exception:
    print(4000.0)
PY
)" --note "adaptive strategy: regime-sized entries, 5.5/7.5 geometry, 52w soft zone"
  python3 scripts/epoch.py status --book swing-9947
fi
REMOTE
fi

if [[ $RESTART -eq 1 ]]; then
  echo "==> restart $UNIT"
  ssh_do "systemctl --user restart '$UNIT'; sleep 3; systemctl --user is-active '$UNIT'; journalctl --user -u '$UNIT' -n 12 --no-pager"
fi

echo "==> done ($SHA, backup $STAMP)"
[[ $RESTART -eq 0 ]] && echo "note: agent NOT restarted (pass --restart to apply rules to the running process)"
exit 0
