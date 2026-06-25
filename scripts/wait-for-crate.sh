#!/usr/bin/env bash
# Wait until a crate version appears on the crates.io index (after publish).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=crates-io-common.sh
source "${SCRIPT_DIR}/crates-io-common.sh"

CRATE="${1:?usage: wait-for-crate.sh <crate-name>}"
VERSION="$(crate_version "$CRATE")"

for attempt in $(seq 1 36); do
  if crate_version_on_registry "$CRATE" "$VERSION"; then
    echo "Indexed: ${CRATE} ${VERSION} on crates.io"
    exit 0
  fi
  echo "Waiting for ${CRATE} ${VERSION} on crates.io (${attempt}/36)..."
  sleep 10
done

echo "Timeout: ${CRATE} ${VERSION} not visible to cargo yet (crate may still be indexing)"
exit 1
