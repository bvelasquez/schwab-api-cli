#!/usr/bin/env bash
# Re-auth Schwab OAuth on Jarvis from an operator Mac.
#
# Captures the https://127.0.0.1:8182 callback locally, then runs
# `schwab auth login --code` on Jarvis so tokens are written to
# ~/.config/schwabinvestbot/tokens.json (the paper agents pick this up
# on the next tick — no service restart).
#
# Do not copy Mac tokens.json to Jarvis: a refresh on either host
# rotates the refresh token and invalidates the other copy.
#
# Run from a Mac with SSH to Jarvis. Do not run from CI.
# Never passes --trust or --yes.
#
# Usage:
#   ./scripts/jarvis-auth-login.sh          # auto-capture callback (default)
#   ./scripts/jarvis-auth-login.sh --paste   # paste redirect URL / code
#
# Env:
#   JARVIS_SSH            SSH target (default: jarvis)
#   JARVIS_PAPER_ENV      Remote EnvironmentFile
#                         (default: $HOME/.config/environment.d/schwab-paper.conf)
#   JARVIS_AUTH_TIMEOUT   Seconds to wait for the browser callback (default: 180)

set -euo pipefail

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  sed -n '2,23p' "$0"
  exit 0
fi

if [[ "$*" == *"--trust"* || "$*" == *"--yes"* ]]; then
  echo "refusing: this script must never pass --trust or --yes" >&2
  exit 1
fi

PASTE=0
for arg in "$@"; do
  case "$arg" in
    --paste) PASTE=1 ;;
    -h|--help) ;;
    *)
      echo "unknown argument: $arg (try --help)" >&2
      exit 1
      ;;
  esac
done

JARVIS_SSH="${JARVIS_SSH:-jarvis}"
JARVIS_PAPER_ENV="${JARVIS_PAPER_ENV:-}"
JARVIS_AUTH_TIMEOUT="${JARVIS_AUTH_TIMEOUT:-180}"
CALLBACK_PORT=8182

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

need_cmd ssh
need_cmd python3
need_cmd openssl
need_cmd open

TMPDIR_AUTH="$(mktemp -d "${TMPDIR:-/tmp}/jarvis-auth.XXXXXX")"
CTL="${TMPDIR_AUTH}/ssh.sock"
cleanup() {
  ssh -o ControlPath="${CTL}" -O exit "${JARVIS_SSH}" >/dev/null 2>&1 || true
  rm -rf "${TMPDIR_AUTH}"
}
trap cleanup EXIT

echo "==> ssh ${JARVIS_SSH} (ControlMaster)"
ssh -o BatchMode=yes -o ConnectTimeout=10 \
  -o ControlMaster=auto -o ControlPath="${CTL}" -o ControlPersist=60 \
  -fN "${JARVIS_SSH}"

jarvis() {
  ssh -o BatchMode=yes -o ControlPath="${CTL}" "${JARVIS_SSH}" "$@"
}

echo "==> authorize URL from Jarvis paper env"
AUTH_URL="$(
  jarvis env PAPER_ENV="${JARVIS_PAPER_ENV}" bash -s <<'REMOTE'
set -euo pipefail
conf="${PAPER_ENV:-$HOME/.config/environment.d/schwab-paper.conf}"
if [[ ! -f "$conf" ]]; then
  echo "missing paper env file: $conf" >&2
  exit 1
fi
set -a
# shellcheck disable=SC1090
source "$conf"
set +a
python3 - <<'PY'
import os
import urllib.parse

key = os.environ.get("SCHWAB_APP_KEY") or os.environ.get("SCHWAB_CLIENT_ID")
if not key:
    raise SystemExit("SCHWAB_APP_KEY missing in paper env")
redir = os.environ.get("SCHWAB_REDIRECT_URI", "https://127.0.0.1:8182")
qs = urllib.parse.urlencode(
    {
        "client_id": key,
        "redirect_uri": redir,
        "response_type": "code",
    }
)
print(f"https://api.schwabapi.com/v1/oauth/authorize?{qs}")
PY
REMOTE
)"

if [[ -z "${AUTH_URL}" || "${AUTH_URL}" != https://api.schwabapi.com/* ]]; then
  echo "failed to build authorize URL from Jarvis" >&2
  exit 1
fi

exchange_on_jarvis() {
  local raw="$1"
  echo "==> schwab auth login --code on Jarvis"
  jarvis env AUTH_CODE="${raw}" PAPER_ENV="${JARVIS_PAPER_ENV}" bash -s <<'REMOTE'
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
conf="${PAPER_ENV:-$HOME/.config/environment.d/schwab-paper.conf}"
if [[ ! -f "$conf" ]]; then
  echo "missing paper env file: $conf" >&2
  exit 1
fi
set -a
# shellcheck disable=SC1090
source "$conf"
set +a
if ! command -v schwab >/dev/null 2>&1; then
  echo "schwab binary not found on PATH" >&2
  exit 1
fi
schwab auth login --code "$AUTH_CODE" --json
echo
echo "==> schwab auth status"
schwab auth status --json
REMOTE
}

if [[ "${PASTE}" -eq 1 ]]; then
  echo
  echo "Opening Schwab login in your browser."
  echo "The https://127.0.0.1:${CALLBACK_PORT} page will fail to load — that is expected."
  echo "Copy the FULL URL from the address bar (it contains code=) and paste it here."
  echo "Authorization codes expire in ~30 seconds after redirect."
  echo
  open "${AUTH_URL}"
  echo -n "Paste redirect URL or code: "
  IFS= read -r PASTE_VALUE
  if [[ -z "${PASTE_VALUE}" ]]; then
    echo "empty paste; aborting" >&2
    exit 1
  fi
  exchange_on_jarvis "${PASTE_VALUE}"
else
  if lsof -nP -iTCP:"${CALLBACK_PORT}" -sTCP:LISTEN >/dev/null 2>&1; then
    echo "127.0.0.1:${CALLBACK_PORT} is already in use. Stop that listener or use --paste." >&2
    lsof -nP -iTCP:"${CALLBACK_PORT}" -sTCP:LISTEN >&2 || true
    exit 1
  fi

  cat >"${TMPDIR_AUTH}/capture.py" <<'PY'
"""HTTPS listener on 127.0.0.1:8182; print OAuth code and exit."""
from __future__ import annotations

import http.server
import os
import ssl
import subprocess
import sys
import tempfile
import time
from urllib.parse import parse_qs, urlparse

PORT = int(os.environ.get("CALLBACK_PORT", "8182"))
TIMEOUT = int(os.environ.get("JARVIS_AUTH_TIMEOUT", "180"))


class Handler(http.server.BaseHTTPRequestHandler):
    code: str | None = None

    def log_message(self, fmt: str, *args: object) -> None:
        return

    def do_GET(self) -> None:  # noqa: N802
        qs = parse_qs(urlparse(self.path).query)
        code = (qs.get("code") or [None])[0]
        if not code:
            self.send_error(404)
            return
        Handler.code = code
        body = b"Schwab OAuth complete. You can close this tab and return to the terminal."
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main() -> int:
    tmp = tempfile.mkdtemp()
    cert = os.path.join(tmp, "cert.pem")
    key = os.path.join(tmp, "key.pem")
    cfg = os.path.join(tmp, "openssl.cnf")
    with open(cfg, "w", encoding="utf-8") as fh:
        fh.write(
            "[req]\n"
            "distinguished_name = dn\n"
            "x509_extensions = v3_req\n"
            "prompt = no\n"
            "[dn]\n"
            "CN = 127.0.0.1\n"
            "[v3_req]\n"
            "subjectAltName = IP:127.0.0.1,DNS:localhost\n"
            "keyUsage = digitalSignature, keyEncipherment\n"
            "extendedKeyUsage = serverAuth\n"
        )
    subprocess.check_call(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-sha256",
            "-days",
            "1",
            "-nodes",
            "-keyout",
            key,
            "-out",
            cert,
            "-config",
            cfg,
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    httpd = http.server.HTTPServer(("127.0.0.1", PORT), Handler)
    httpd.allow_reuse_address = True
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(cert, key)
    httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
    httpd.timeout = 0.5

    deadline = time.time() + TIMEOUT
    while Handler.code is None and time.time() < deadline:
        try:
            httpd.handle_request()
        except (ssl.SSLError, ConnectionError, OSError):
            # Browser cert warning fails the first handshake; keep listening.
            continue
    if Handler.code:
        sys.stdout.write(Handler.code + "\n")
        return 0
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
PY

  echo
  echo "Opening Schwab login in your browser."
  echo "Accept the self-signed certificate for 127.0.0.1 (Advanced → Proceed)."
  echo "Waiting up to ${JARVIS_AUTH_TIMEOUT}s for https://127.0.0.1:${CALLBACK_PORT} …"
  echo

  CALLBACK_PORT="${CALLBACK_PORT}" JARVIS_AUTH_TIMEOUT="${JARVIS_AUTH_TIMEOUT}" \
    python3 "${TMPDIR_AUTH}/capture.py" >"${TMPDIR_AUTH}/code.txt" &
  CAPTURE_PID=$!

  bound=0
  for _ in $(seq 1 50); do
    if ! kill -0 "${CAPTURE_PID}" 2>/dev/null; then
      break
    fi
    if lsof -nP -iTCP:"${CALLBACK_PORT}" -sTCP:LISTEN >/dev/null 2>&1; then
      bound=1
      break
    fi
    sleep 0.1
  done
  if [[ "${bound}" -ne 1 ]]; then
    wait "${CAPTURE_PID}" || true
    echo "callback listener failed to start on 127.0.0.1:${CALLBACK_PORT}" >&2
    exit 1
  fi
  open "${AUTH_URL}"

  if ! wait "${CAPTURE_PID}"; then
    echo "callback timed out. Re-run, or use --paste and copy the URL containing code=." >&2
    exit 1
  fi
  CODE="$(tr -d '[:space:]' <"${TMPDIR_AUTH}/code.txt")"
  if [[ -z "${CODE}" ]]; then
    echo "callback produced an empty code" >&2
    exit 1
  fi
  exchange_on_jarvis "${CODE}"
fi

echo
echo "Jarvis tokens updated. Paper agents reload tokens.json on the next tick; no restart needed."
echo "If an agent was in auth_fatal backoff, it should recover automatically."
