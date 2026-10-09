#!/usr/bin/env bash
# Record option chain snapshots for backtest pricing (read-only; no OAuth refresh).
#
# Env:
#   OUT_DIR          Output root (default: rules/chains)
#   SCHWAB_TOKEN_DIR Token directory (default: ~/.config/schwabinvestbot)
#   SCHWAB_REPO      Repo path for schwab CLI (optional)

set -euo pipefail

log() { printf '%s\n' "$*" >&2; }

if [[ -f "${HOME}/.config/environment.d/schwab-paper.conf" ]]; then
  set -a
  # shellcheck source=/dev/null
  . "${HOME}/.config/environment.d/schwab-paper.conf"
  set +a
fi

export SCHWAB_TOKEN_DIR="${SCHWAB_TOKEN_DIR:-${HOME}/.config/schwabinvestbot}"
export SCHWAB_REPO="${SCHWAB_REPO:-${HOME}/projects/schwabinvestbot}"

SCHWAB="${SCHWAB:-${HOME}/.cargo/bin/schwab}"
OUT_DIR="${OUT_DIR:-rules/chains}"
STRIKE_COUNT="${STRIKE_COUNT:-150}"
DTE_FROM="${DTE_FROM:-14}"
DTE_TO="${DTE_TO:-60}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

if [[ -x "${SCHWAB}" ]]; then
  :
elif command -v schwab >/dev/null 2>&1; then
  SCHWAB="$(command -v schwab)"
else
  log "chain-snapshot: schwab CLI not found"
  exit 1
fi

market_open() {
  local hours_json
  if ! hours_json="$("${SCHWAB}" market hours --markets equity,option -j 2>/dev/null)"; then
    log "chain-snapshot: market hours unavailable; continuing"
    return 0
  fi
  python3 -c '
import json, sys
try:
    doc = json.loads(sys.stdin.read())
except json.JSONDecodeError:
    sys.exit(0)
data = doc.get("data") or {}
eq = (data.get("equity") or {}).get("EQ") or {}
opt = (data.get("option") or {}).get("EQO") or {}
if eq.get("isOpen") and opt.get("isOpen"):
    sys.exit(0)
sys.exit(2)
' <<<"${hours_json}"
}

if ! market_open; then
  rc=$?
  if [[ "${rc}" -eq 2 ]]; then
    log "chain-snapshot: market closed or not a trading day; skipping"
    exit 0
  fi
fi

TZ_ET="America/New_York"
DAY="$(TZ="${TZ_ET}" date +%Y-%m-%d)"
STAMP="$(TZ="${TZ_ET}" date +%H%M)"
FROM_DATE="$(date -d "+${DTE_FROM} days" +%Y-%m-%d)"
TO_DATE="$(date -d "+${DTE_TO} days" +%Y-%m-%d)"

DEST="${OUT_DIR}/${DAY}"
mkdir -p "${DEST}"

snapshot_symbol() {
  local sym="$1"
  local put_tmp call_tmp merged_tmp out_gz
  put_tmp="$(mktemp)"
  call_tmp="$(mktemp)"
  merged_tmp="$(mktemp)"
  # shellcheck disable=SC2064
  trap "rm -f '${put_tmp}' '${call_tmp}' '${merged_tmp}'" RETURN

  local put_ok=0 call_ok=0
  if "${SCHWAB}" options chain \
    --symbol "${sym}" \
    --contract-type PUT \
    --from-date "${FROM_DATE}" \
    --to-date "${TO_DATE}" \
    --strike-count "${STRIKE_COUNT}" \
    -j >"${put_tmp}"; then
    put_ok=1
  else
    log "chain-snapshot: ${sym} PUT chain failed"
  fi

  if "${SCHWAB}" options chain \
    --symbol "${sym}" \
    --contract-type CALL \
    --from-date "${FROM_DATE}" \
    --to-date "${TO_DATE}" \
    --strike-count "${STRIKE_COUNT}" \
    -j >"${call_tmp}"; then
    call_ok=1
  else
    log "chain-snapshot: ${sym} CALL chain failed"
  fi

  if [[ "${put_ok}" -eq 0 && "${call_ok}" -eq 0 ]]; then
    log "chain-snapshot: ${sym} skipped (PUT and CALL failed)"
    return 0
  fi

  export FROM_DATE TO_DATE
  if ! python3 - "${put_tmp}" "${call_tmp}" "${merged_tmp}" <<'PY'
import json
import os
import sys
from copy import deepcopy

def load(path: str):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, json.JSONDecodeError):
        return None

put_path, call_path, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
put = load(put_path)
call = load(call_path)

def ok(doc):
    return isinstance(doc, dict) and doc.get("success") and isinstance(doc.get("data"), dict)

if ok(put) and ok(call):
    merged = deepcopy(put)
    merged["data"]["callExpDateMap"] = call["data"].get("callExpDateMap") or {}
elif ok(put):
    merged = deepcopy(put)
    merged["data"].setdefault("callExpDateMap", {})
elif ok(call):
    merged = deepcopy(call)
    merged["data"].setdefault("putExpDateMap", {})
else:
    sys.exit(1)

merged["snapshot_meta"] = {
    "from_date": os.environ["FROM_DATE"],
    "to_date": os.environ["TO_DATE"],
    "strike_count": int(os.environ.get("STRIKE_COUNT", "150")),
    "dte_from": int(os.environ.get("DTE_FROM", "14")),
    "dte_to": int(os.environ.get("DTE_TO", "60")),
    "contract_types": ["PUT", "CALL"],
}

with open(out_path, "w", encoding="utf-8") as f:
    json.dump(merged, f, separators=(",", ":"))
PY
  then
    log "chain-snapshot: ${sym} merge failed"
    return 0
  fi

  out_gz="${DEST}/${sym}-${STAMP}ET.json.gz"
  gzip -c "${merged_tmp}" >"${out_gz}"
  local raw_bytes gz_bytes
  raw_bytes="$(wc -c <"${merged_tmp}" | tr -d ' ')"
  gz_bytes="$(wc -c <"${out_gz}" | tr -d ' ')"
  log "chain-snapshot: ${sym} -> ${out_gz} (${gz_bytes} bytes gzip, ${raw_bytes} bytes raw)"
}

for sym in QQQ SPY; do
  snapshot_symbol "${sym}" || log "chain-snapshot: ${sym} unexpected error"
done
