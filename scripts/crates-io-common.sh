#!/usr/bin/env bash
# Shared helpers for crates.io publish scripts.
set -euo pipefail

CRATES_IO_USER_AGENT="${CRATES_IO_USER_AGENT:-schwab-api-cli-publish/0.1.0 (https://github.com/bvelasquez/schwab-api-cli)}"

crate_version() {
  local crate="$1"
  cargo metadata --format-version=1 --no-deps \
    | python3 -c "import json,sys; pkgs=json.load(sys.stdin)['packages']; print(next(p['version'] for p in pkgs if p['name']=='${crate}'))"
}

# Prefer cargo index (what `cargo publish` uses). Must run outside the workspace —
# otherwise `cargo info` resolves local path packages and looks "published".
crate_version_on_registry() {
  local crate="$1"
  local version="$2"
  local tmp
  tmp="$(mktemp -d)"
  if (cd "$tmp" && cargo info "${crate}@${version}" >/dev/null 2>&1); then
    rm -rf "$tmp"
    return 0
  fi
  rm -rf "$tmp"
  local status
  status="$(curl -fsS -o /dev/null -w "%{http_code}" \
    -A "${CRATES_IO_USER_AGENT}" \
    "https://crates.io/api/v1/crates/${crate}/${version}" 2>/dev/null || echo "000")"
  [ "$status" = "200" ]
}
