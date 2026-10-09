#!/usr/bin/env bash
# Install the five $30k options paper arms on this machine.
#
# Reads the account hash from rules/options-pilot-8709.yaml, writes gitignored
# rules/options-arm-oN.yaml, and starts schwab-options-arm-oN.service.
# Paper only. Never passes --trust or --yes. Does not restart the 8709 agent.
#
# Run on the paper host after a fast-forward pull:
#   ./scripts/install-options-arms.sh

set -euo pipefail

if [[ "$*" == *"--trust"* || "$*" == *"--yes"* ]]; then
  echo "refusing: this script must never pass --trust or --yes" >&2
  exit 1
fi

repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"

src="rules/options-pilot-8709.yaml"
if [[ ! -f "$src" ]]; then
  echo "missing $src" >&2
  exit 1
fi

hash="$(python3 - "$src" <<'PY'
import sys
from pathlib import Path
for line in Path(sys.argv[1]).read_text().splitlines():
    if line.startswith("  - hash: "):
        value = line.split(": ", 1)[1].strip()
        if len(value) < 32 or value == "__ACCOUNT_HASH__":
            raise SystemExit("hash line is not a real account hash")
        print(value)
        break
else:
    raise SystemExit("no hash line in source rules")
PY
)"

unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
drop_src="$unit_dir/schwab-options-8709.service.d"
mkdir -p "$unit_dir"

for arm in o0 o1 o2 o3 o4; do
  template="rules/arms/options/${arm}.yaml"
  dest="rules/options-arm-${arm}.yaml"
  if [[ ! -f "$template" ]]; then
    echo "missing $template" >&2
    exit 1
  fi
  python3 - "$template" "$dest" "$hash" <<'PY'
import sys
from pathlib import Path
src, dest, account = sys.argv[1:]
text = Path(src).read_text()
if text.count("__ACCOUNT_HASH__") != 1:
    raise SystemExit(f"{src} should contain the hash placeholder once")
Path(dest).write_text(text.replace("__ACCOUNT_HASH__", account, 1))
PY
  chmod 600 "$dest"

  unit="schwab-options-arm-${arm}.service"
  cp -f "deploy/systemd/user/${unit}" "$unit_dir/${unit}"
  mkdir -p "$unit_dir/${unit}.d"
  if [[ -f "$drop_src/env.conf" ]]; then
    cp -f "$drop_src/env.conf" "$unit_dir/${unit}.d/env.conf"
  fi
  if [[ -f "$drop_src/token-mirror.conf" ]]; then
    cp -f "$drop_src/token-mirror.conf" "$unit_dir/${unit}.d/token-mirror.conf"
  else
    echo "missing token-mirror drop-in; arms must not refresh OAuth on their own" >&2
    exit 1
  fi
done

systemctl --user daemon-reload
for arm in o0 o1 o2 o3 o4; do
  systemctl --user enable "schwab-options-arm-${arm}.service"
  systemctl --user restart "schwab-options-arm-${arm}.service"
  sleep 15
done

echo "==> arm units"
systemctl --user is-active \
  schwab-options-arm-o0.service \
  schwab-options-arm-o1.service \
  schwab-options-arm-o2.service \
  schwab-options-arm-o3.service \
  schwab-options-arm-o4.service
echo "==> 8709 left as-is: $(systemctl --user is-active schwab-options-8709.service)"
