#!/usr/bin/env bash
# Mirror Jarvis's Schwab *access* token onto the Mac (read-only consumer).
#
# Jarvis is the single OAuth owner for this Schwab app: only Jarvis logs in and
# only Jarvis refreshes. This Mac copies the current access token so local CLI
# calls still have live Schwab data, WITHOUT minting or rotating anything.
#
# Schwab keeps one active OAuth session per account, so a second refresher
# invalidates the other holder's refresh token. Two deliberate guards:
#   1. the mirrored file is written with an EMPTY refresh_token, so if a Mac-side
#      call ever needs a refresh it fails closed instead of killing Jarvis's auth;
#   2. the refresh token value never lands on Mac disk at all (this script
#      extracts only access_token/expiry metadata from Jarvis).
#
# Usage:
#   ./scripts/jarvis-token-sync.sh
#
# Env:
#   JARVIS_SSH             SSH target                       (default: jarvis)
#   JARVIS_PAPER_ENV       Remote EnvironmentFile
#                          (default: $HOME/.config/environment.d/schwab-paper.conf)
#   JARVIS_REPO            Remote repo path                 (default: $HOME/projects/schwabinvestbot)
#   SCHWAB_MAC_TOKEN_DIR   Mac token dir (tokens.json lands here)
#                          (default: ~/Library/Application Support/schwabinvestbot)
#   SYNC_MIN_VALID_SECS    Ask Jarvis to refresh when the mirrored access token
#                          has less than this left             (default: 300)
#
# Never passes --trust. --yes is used only for the Jarvis-side auth refresh
# (Jarvis is the owner, so refreshing there is the supported path).

set -euo pipefail

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  sed -n '2,32p' "$0"
  exit 0
fi

if [[ "$*" == *"--trust"* ]]; then
  echo "refusing: this script must never pass --trust" >&2
  exit 1
fi

JARVIS_SSH="${JARVIS_SSH:-jarvis}"
JARVIS_REPO="${JARVIS_REPO:-\$HOME/projects/schwabinvestbot}"
JARVIS_PAPER_ENV="${JARVIS_PAPER_ENV:-\$HOME/.config/environment.d/schwab-paper.conf}"
MAC_TOKEN_DIR="${SCHWAB_MAC_TOKEN_DIR:-$HOME/Library/Application Support/schwabinvestbot}"
MIN_VALID="${SYNC_MIN_VALID_SECS:-300}"

remote() {
  ssh "$JARVIS_SSH" "set -a; . $JARVIS_PAPER_ENV; set +a; export SCHWAB_REPO=$JARVIS_REPO; cd $JARVIS_REPO; $*"
}

command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }

# 1. Ask Jarvis for token status. Only Jarvis refreshes, and only when needed.
status_json="$(remote '~/.cargo/bin/schwab auth status --json')"
valid_left="$(printf '%s' "$status_json" | jq -r '.data.expires_in_seconds // 0')"

refreshed="no"
if (( valid_left < MIN_VALID )); then
  echo "Jarvis access token has ${valid_left}s left (< ${MIN_VALID}s) — asking Jarvis to refresh (owner side)..."
  if refresh_out="$(remote '~/.cargo/bin/schwab auth refresh --yes --json' 2>&1)"; then
    refreshed="yes"
  else
    echo "Jarvis cannot refresh its own token:" >&2
    printf '%s\n' "$refresh_out" >&2
    echo "" >&2
    echo "Jarvis needs a fresh login (browser step, run from this Mac):" >&2
    echo "  ./scripts/jarvis-auth-login.sh" >&2
    exit 1
  fi
fi

# 2. Pull Jarvis's bundle and keep only the access-token half.
raw="$(ssh "$JARVIS_SSH" 'cat "$HOME/.config/schwabinvestbot/tokens.json"')"
mkdir -p "$MAC_TOKEN_DIR"
chmod 700 "$MAC_TOKEN_DIR"
scratch="$(mktemp "${TMPDIR:-/tmp}/schwab-mirror.XXXXXX")"
trap 'rm -f "$scratch"' EXIT

printf '%s' "$raw" | jq -S '{
  access_token: .access_token,
  refresh_token: "",
  token_type: (.token_type // "Bearer"),
  expires_at: .expires_at,
  scope: (.scope // "api"),
  obtained_at: .obtained_at
}' > "$scratch"

# Guard: refuse to install a mirror that carries a usable refresh token.
if [[ "$(jq -r '.refresh_token' "$scratch")" != "" ]]; then
  echo "refusing: mirror carries a refresh token (would create a second refrester)" >&2
  exit 1
fi

install -m 600 "$scratch" "$MAC_TOKEN_DIR/tokens.json"

jq -r '"mirror: \(.obtained_at)  expires_at=\(.expires_at)  refresh_token=<empty by design>"' "$scratch"
echo "refreshed_on_jarvis=$refreshed   wrote=$MAC_TOKEN_DIR/tokens.json (mode 600)"
echo "note: as a read-only consumer the Mac cannot refresh; re-run this script when the token goes stale."
