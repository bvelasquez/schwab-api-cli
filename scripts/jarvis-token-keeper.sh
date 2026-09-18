#!/usr/bin/env bash
# jarvis-token-keeper.sh — the ONE OAuth refresher on Jarvis.
#
# Why this exists (2026-09-18 post-mortem):
#   Schwab keeps a single active OAuth session per app+account. Two processes
#   that both refresh rotate each other's refresh token out from under them.
#   On 2026-09-18 both jarvis paper agents sat in `auth_fatal invalid_grant`
#   (swing 4h15m of the regular session, options ~20h) and a fresh token only
#   appeared when one of them happened to win a refresh race.
#
#   Fix: exactly one process refreshes — this keeper — and every agent reads an
#   access-token-only mirror (refresh_token emptied) so an agent can never start
#   a refresh of its own. Same doctrine the Mac already follows for Jarvis.
#
# Modes:
#   (default)   refresh only when the access token is nearly dead, then rewrite
#               the mirror and the status file.
#   --dry-run   print what it would do; touch nothing, refresh nothing.
#   --force     refresh even if the access token is still valid (testing).
#
# Exit codes: 0 ok/idle, 2 no usable refresh token (needs interactive login),
#             3 refresh failed, 4 creds/config missing.
#
# Run by systemd (schwab-token-keeper.timer) or by hand. Safe to run often:
# a flock keeps concurrent runs from racing.

set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

OWNER_DIR="${SCHWAB_OWNER_TOKEN_DIR:-$HOME/.config/schwabinvestbot}"
MIRROR_DIR="${SCHWAB_MIRROR_TOKEN_DIR:-$OWNER_DIR/agents}"
MIN_VALID_SECS="${MIN_VALID_SECS:-900}"
STATE_DIR="${SCHWAB_STATE_DIR:-$HOME/.local/state/schwab-paper}"
CREDS_FILE="${SCHWAB_CREDS_FILE:-$HOME/.config/environment.d/schwab-paper.conf}"
LOG="$STATE_DIR/token-keeper.log"
STATUS_JSON="$STATE_DIR/token-keeper.json"

DRY_RUN=0
FORCE=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --force) FORCE=1 ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "unknown arg: $arg" >&2; exit 64 ;;
  esac
done

mkdir -p "$STATE_DIR"

log() { printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >>"$LOG"; }

# Non-interactive shells (and systemd units started before environment.d) may
# not carry the credentials — source them if missing.
if [[ -z "${SCHWAB_APP_KEY:-}${SCHWAB_CLIENT_ID:-}" ]]; then
  # shellcheck disable=SC1090
  [[ -r "$CREDS_FILE" ]] && { set -a; . "$CREDS_FILE"; set +a; }
fi
if [[ -z "${SCHWAB_APP_KEY:-}${SCHWAB_CLIENT_ID:-}" ]]; then
  log "ERROR missing Schwab app credentials (SCHWAB_APP_KEY/SCHWAB_CLIENT_ID)"
  echo "missing Schwab app credentials" >&2
  exit 4
fi

if [[ ! -d "$OWNER_DIR" ]]; then
  log "ERROR owner token dir missing: $OWNER_DIR"
  exit 4
fi

# Serialize keeper runs (and any manual run) against each other.
exec 9>"$OWNER_DIR/.token-keeper.lock"
if ! flock -n 9; then
  echo "another keeper run holds the lock; exiting"
  exit 0
fi

status_json="$(schwab auth status --json 2>/dev/null || true)"
if [[ -z "$status_json" ]]; then
  log "ERROR auth status produced no output"
  exit 4
fi

authenticated="$(jq -r '.data.authenticated // false' <<<"$status_json")"
expires_in="$(jq -r '.data.expires_in_seconds // 0' <<<"$status_json")"
refresh_expires_in="$(jq -r '.data.refresh_expires_in_seconds // 0' <<<"$status_json")"
token_path="$(jq -r '.data.token_path // ""' <<<"$status_json")"

write_status() { # action, ok, note
  local action="$1" ok="$2" note="$3"
  if (( DRY_RUN )); then return 0; fi
  cat >"$STATUS_JSON.tmp" <<JSON
{"at":"$(date -u +%Y-%m-%dT%H:%M:%SZ)","action":"$action","ok":$ok,"note":"$note","expires_in_seconds":$expires_in,"refresh_expires_in_seconds":$refresh_expires_in,"token_path":"$token_path","mirror_dir":"$MIRROR_DIR"}
JSON
  mv "$STATUS_JSON.tmp" "$STATUS_JSON"
}

if [[ "$authenticated" != "true" ]]; then
  log "ERROR not authenticated; interactive login required (token=$token_path)"
  write_status "unauthenticated" false "interactive login required"
  echo "NOT AUTHENTICATED: run jarvis-auth-login.sh from the Mac" >&2
  exit 2
fi

action="idle"
if (( FORCE )) || (( expires_in < MIN_VALID_SECS )); then
  action="refresh"
  if (( DRY_RUN )); then
    echo "would refresh: expires_in=${expires_in}s (threshold ${MIN_VALID_SECS}s)"
  else
    if ! refresh_json="$(schwab auth refresh --yes --json 2>&1)"; then
      log "ERROR refresh failed: $(tr '\n' ' ' <<<"$refresh_json" | cut -c1-400)"
      write_status "refresh_failed" false "$(tr '\n' ' ' <<<"$refresh_json" | cut -c1-200)"
      echo "refresh failed" >&2
      exit 3
    fi
    if [[ "$(jq -r '.success // false' <<<"$refresh_json" 2>/dev/null)" != "true" ]]; then
      log "ERROR refresh not successful: $(tr '\n' ' ' <<<"$refresh_json" | cut -c1-400)"
      write_status "refresh_failed" false "$(tr '\n' ' ' <<<"$refresh_json" | cut -c1-200)"
      exit 3
    fi
    expires_in="$(jq -r '.data.expires_in_seconds // 0' <<<"$refresh_json" 2>/dev/null || echo 0)"
    log "refreshed ok; new expires_in=${expires_in}s"
  fi
fi

# Mirror: access token only — an empty refresh_token means a mirror reader can
# never start its own OAuth refresh.
owner_file="$OWNER_DIR/tokens.json"
if [[ ! -r "$owner_file" ]]; then
  log "ERROR owner token file unreadable: $owner_file"
  exit 4
fi

if (( DRY_RUN )); then
  echo "would write mirror $MIRROR_DIR/tokens.json from $owner_file"
  echo "owner expires_in=${expires_in}s refresh_expires_in=${refresh_expires_in}s action=$action"
  exit 0
fi

mkdir -p "$MIRROR_DIR"
tmp="$MIRROR_DIR/.tokens.json.tmp.$$"
jq '{access_token, refresh_token:"", token_type, expires_at, scope, obtained_at}' \
  "$owner_file" >"$tmp"
chmod 600 "$tmp"
mv -f "$tmp" "$MIRROR_DIR/tokens.json"

log "ok action=$action expires_in=${expires_in}s mirror=$MIRROR_DIR/tokens.json"
write_status "$action" true "ok"
echo "ok action=$action expires_in=${expires_in}s"
