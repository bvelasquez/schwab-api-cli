#!/usr/bin/env bash
# Publish a workspace crate to crates.io if this version is not already published.
set -euo pipefail

CRATE="${1:?usage: publish-crate.sh <crate-name>}"

VERSION="$(cargo metadata --format-version=1 --no-deps \
  | python3 -c "import json,sys; pkgs=json.load(sys.stdin)['packages']; print(next(p['version'] for p in pkgs if p['name']=='${CRATE}'))")"

STATUS="$(curl -s -o /dev/null -w "%{http_code}" "https://crates.io/api/v1/crates/${CRATE}/${VERSION}")"

if [ "$STATUS" = "200" ]; then
  echo "SKIP: ${CRATE} ${VERSION} is already on crates.io"
  exit 0
fi

echo "Publishing ${CRATE} ${VERSION} to crates.io..."
cargo publish -p "$CRATE" --locked --verbose
