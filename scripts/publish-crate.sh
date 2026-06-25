#!/usr/bin/env bash
# Publish a workspace crate to crates.io if this version is not already published.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=crates-io-common.sh
source "${SCRIPT_DIR}/crates-io-common.sh"

CRATE="${1:?usage: publish-crate.sh <crate-name>}"
VERSION="$(crate_version "$CRATE")"

if crate_version_on_registry "$CRATE" "$VERSION"; then
  echo "SKIP: ${CRATE} ${VERSION} is already on crates.io"
  exit 0
fi

echo "Publishing ${CRATE} ${VERSION} to crates.io..."
cargo publish -p "$CRATE" --locked --verbose
