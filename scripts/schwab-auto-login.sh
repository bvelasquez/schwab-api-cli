#!/usr/bin/env bash
# schwab-auto-login.sh — wrapper for schwab-auto-login.py.
#
# Runs ON Jarvis (via schwab-auto-login.timer, or by hand). Sources
# schwab-paper.conf (Schwab app key + Telegram bot creds ONLY — never the
# login id/password), runs the venv's python under xvfb-run, and passes all
# arguments through untouched.
#
# The login id/password/2FA-method live in a SEPARATE file,
# ~/.config/schwabinvestbot/autologin.env (mode 600/400), which this script
# never sources or exports — schwab-auto-login.py reads it directly,
# in-process, so the password never enters a process environment.
#
# 2FA is a Telegram round trip with Barry (no TOTP secret stored on jarvis):
# see the schwab-auto-login.py module docstring.
#
# Setup (one-time, or after a Chromium/Playwright upgrade):
#   ./scripts/schwab-auto-login.sh --setup
#
# Normal use:
#   ./scripts/schwab-auto-login.sh              # decides on its own whether to log in
#   ./scripts/schwab-auto-login.sh --dry-run     # load the login page, report selectors, stop
#   ./scripts/schwab-auto-login.sh --self-test   # offline checks (Telegram filter, perms, lockout), no network
#   ./scripts/schwab-auto-login.sh --force --i-know   # force a real login attempt (bypasses lockout too)
#
# Env:
#   SCHWAB_CREDS_FILE      schwab-paper.conf path (default: $HOME/.config/environment.d/schwab-paper.conf)
#   SCHWAB_AUTOLOGIN_ENV   Separate login-id/password file (default: $HOME/.config/schwabinvestbot/autologin.env)
#   SCHWAB_AUTOLOGIN_VENV  Venv path (default: $HOME/.local/share/schwab-autologin/venv)

set -euo pipefail

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  sed -n '2,29p' "$0"
  exit 0
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
CREDS_FILE="${SCHWAB_CREDS_FILE:-$HOME/.config/environment.d/schwab-paper.conf}"
VENV_DIR="${SCHWAB_AUTOLOGIN_VENV:-$HOME/.local/share/schwab-autologin/venv}"

export PATH="$HOME/.cargo/bin:$PATH"

install_chromium_directly() {
  # Fallback for hosts where Node's Happy-Eyeballs downloader hangs on a
  # broken/blackholed IPv6 route (seen on jarvis: `curl` over IPv4 fetches
  # the same 190MB build in ~2s, but Node's dual-stack lookup inside
  # playwright's own downloader times out after 30s on every attempt).
  # Reproduces `playwright install chromium` by hand: read the exact
  # install locations/URLs for this Playwright version from --dry-run,
  # then curl + unzip each one and drop the INSTALLATION_COMPLETE marker
  # playwright checks for.
  echo "==> playwright's own downloader failed; falling back to direct curl download" >&2
  local dryrun
  dryrun="$("${VENV_DIR}/bin/python" -m playwright install chromium --dry-run)"
  local pairs
  pairs="$(awk '
    /^(Chrome for Testing|Chrome Headless Shell)/ {
      getline; sub(/^  Install location: */,""); loc=$0;
      getline; sub(/^  Download url: */,""); url=$0;
      print loc "|" url
    }
  ' <<<"$dryrun")"
  if [[ -z "$pairs" ]]; then
    echo "could not parse 'playwright install chromium --dry-run' output; giving up" >&2
    return 1
  fi
  while IFS='|' read -r loc url; do
    [[ -z "$loc" || -z "$url" ]] && continue
    echo "   downloading $url"
    echo "        -> $loc"
    mkdir -p "$loc"
    local tmp_zip
    tmp_zip="$(mktemp)"
    if ! curl -fL --retry 3 --connect-timeout 15 -sS -o "$tmp_zip" "$url"; then
      echo "curl download failed for $url" >&2
      rm -f "$tmp_zip"
      return 1
    fi
    (cd "$loc" && unzip -q -o "$tmp_zip")
    rm -f "$tmp_zip"
    touch "$loc/INSTALLATION_COMPLETE"
  done <<<"$pairs"
}

setup() {
  echo "==> creating venv: ${VENV_DIR}"
  mkdir -p "$(dirname "${VENV_DIR}")"
  python3 -m venv "${VENV_DIR}"

  echo "==> pip install playwright"
  "${VENV_DIR}/bin/pip" install --upgrade pip >/dev/null
  "${VENV_DIR}/bin/pip" install --upgrade playwright

  echo "==> playwright install chromium"
  if ! "${VENV_DIR}/bin/python" -m playwright install chromium; then
    install_chromium_directly || {
      echo "chromium install failed (both the normal downloader and the direct-curl fallback)" >&2
      exit 1
    }
  fi
  echo "==> checking for xvfb-run"
  # Schwab's Akamai WAF 403s Playwright's true headless=True mode; the login
  # flow runs Chrome in normal mode under a virtual framebuffer instead (see
  # schwab-auto-login.py header comment). Jarvis still needs no real display.
  if ! command -v xvfb-run >/dev/null 2>&1; then
    if sudo -n true 2>/dev/null; then
      sudo apt-get update -qq && sudo apt-get install -y -qq xvfb
    else
      echo "xvfb-run not found and sudo needs a password — ask Barry to run: sudo apt-get install -y xvfb" >&2
      exit 1
    fi
  fi

  echo "==> verifying chromium launches (under xvfb-run, non-headless — see comment above)"
  verify_launch() {
    xvfb-run -a "${VENV_DIR}/bin/python" -c "
from playwright.sync_api import sync_playwright
with sync_playwright() as pw:
    b = pw.chromium.launch(headless=False, args=['--disable-blink-features=AutomationControlled'])
    b.new_page().goto('about:blank')
    b.close()
print('chromium launch OK')
"
  }
  if verify_launch; then
    echo "==> setup complete: ${VENV_DIR}"
    return 0
  fi

  echo "==> chromium failed to launch — likely missing system shared libraries" >&2
  echo "==> playwright install-deps (system libraries for Chromium)" >&2
  if sudo -n true 2>/dev/null; then
    "${VENV_DIR}/bin/python" -m playwright install-deps chromium
    echo "==> re-verifying chromium launches"
    verify_launch
    echo "==> setup complete: ${VENV_DIR}"
  else
    cat <<'EOF' >&2
sudo needs a password on this host — cannot run `playwright install-deps` automatically.
Ask Barry to run:

  sudo apt-get update && sudo apt-get install -y \
    libnss3 libnspr4 libatk1.0-0 libatk-bridge2.0-0 libcups2 libdrm2 \
    libxkbcommon0 libxcomposite1 libxdamage1 libxfixes3 libxrandr2 \
    libgbm1 libasound2t64 libpango-1.0-0 libcairo2 libatspi2.0-0

(Package names above match Ubuntu 24.04; run `python -m playwright install-deps --dry-run` in the
venv for the exact list on this host.)
EOF
    echo "==> setup finished with a manual step required (see apt command above)" >&2
    exit 1
  fi
}

if [[ "${1:-}" == "--setup" ]]; then
  setup
  exit 0
fi

if [[ ! -x "${VENV_DIR}/bin/python" ]]; then
  echo "schwab-auto-login venv not found at ${VENV_DIR} — run: $0 --setup" >&2
  exit 4
fi

if [[ -r "${CREDS_FILE}" ]]; then
  set -a
  # shellcheck disable=SC1090
  . "${CREDS_FILE}"
  set +a
fi

cd "${REPO_DIR}"

# --self-test is pure stdlib (Telegram filter, perms, lockout checks) and
# never touches a browser or the network — skip Xvfb for it. Everything else
# drives real Chromium, which
# needs a virtual display (see schwab-auto-login.py header comment on why
# it's non-headless).
for arg in "$@"; do
  if [[ "$arg" == "--self-test" ]]; then
    exec "${VENV_DIR}/bin/python" "${SCRIPT_DIR}/schwab-auto-login.py" "$@"
  fi
done

if ! command -v xvfb-run >/dev/null 2>&1; then
  echo "xvfb-run not found — run: $0 --setup" >&2
  exit 4
fi
exec xvfb-run -a "${VENV_DIR}/bin/python" "${SCRIPT_DIR}/schwab-auto-login.py" "$@"
