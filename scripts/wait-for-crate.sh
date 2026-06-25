#!/usr/bin/env bash
# Wait until a crate version appears on the crates.io index (after publish).
set -euo pipefail

CRATE="${1:?usage: wait-for-crate.sh <crate-name>}"

VERSION="$(cargo metadata --format-version=1 --no-deps \
  | python3 -c "import json,sys; pkgs=json.load(sys.stdin)['packages']; print(next(p['version'] for p in pkgs if p['name']=='${CRATE}'))")"

for attempt in $(seq 1 36); do
  STATUS="$(curl -s -o /dev/null -w "%{http_code}" "https://crates.io/api/v1/crates/${CRATE}/${VERSION}")"
  if [ "$STATUS" = "200" ]; then
    echo "Indexed: ${CRATE} ${VERSION} on crates.io"
    exit 0
  fi
  echo "Waiting for ${CRATE} ${VERSION} on crates.io (${attempt}/36)..."
  sleep 10
done

echo "Timeout: ${CRATE} ${VERSION} not found on crates.io"
exit 1
