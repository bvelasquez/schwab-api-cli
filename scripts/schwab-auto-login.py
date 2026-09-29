#!/usr/bin/env python3
"""schwab-auto-login.py — headless Schwab OAuth re-login for Jarvis.

Why this exists: Schwab refresh tokens die 7 days after interactive login and
there is no way to extend them (docs/JARVIS_PAPER_HOST.md). Today Barry runs
scripts/jarvis-auth-login.sh from his Mac by hand every ~7 days. This script
does the same authorize -> login -> 2FA -> consent -> code-exchange dance on
Jarvis with Playwright + Chromium, using the login id/password from a
dedicated, permission-checked credentials file and Barry's own phone for the
second factor (see "2FA" below) — no long-lived second-factor secret is ever
stored on Jarvis.

Chromium runs in normal (non-headless) mode under a virtual framebuffer
(Xvfb, via the `xvfb-run` wrapper in schwab-auto-login.sh) rather than
Playwright's `headless=True` mode: Schwab's Akamai WAF returns a flat 403
"Access Denied" to true headless Chromium's fingerprint before it ever
reaches the login page, but passes a normal Chrome window running on a
virtual display. Jarvis has no physical display or desktop session either
way — Xvfb only provides the framebuffer Chrome insists on having.

Credentials (2026-09-28 design, Barry):
  - SCHWAB_LOGIN_ID / SCHWAB_PASSWORD / optional SCHWAB_2FA_METHOD live in
    ~/.config/schwabinvestbot/autologin.env (override: SCHWAB_AUTOLOGIN_ENV),
    mode 600 or 400, owned by the running user. This script refuses to run
    if the file is missing, wrongly permissioned, or not owned by it. The
    file is parsed directly, in-process — it is NEVER merged into
    os.environ, and NEVER passed via env= to any subprocess. It must never
    appear in a systemd EnvironmentFile= (schwab-paper.conf, which IS an
    EnvironmentFile for the paper agents, only ever holds the Schwab app
    key and the Telegram bot token — secrets readable via
    /proc/<pid>/environ for anything in that file).
  - 2FA is NOT automated with a stored secret. At Schwab's 2FA step this
    script sends Barry a Telegram message with a short nonce and long-polls
    Telegram's getUpdates for his reply containing the 6-digit code Schwab
    shows/texts him. The second factor lives on Barry's phone, not on disk
    here — a stolen SCHWAB_PASSWORD alone is not enough to complete a login.

Exit codes:
  0  success, or a clean no-op (nothing needed doing)
  1  login flow failed at some step (see Telegram alert / artifacts)
  2  blocked by the lockout/backoff guard
  3  schwab auth login "succeeded" but auth status still isn't authenticated
  4  missing/misconfigured credentials (never attempts a login)

Run via scripts/schwab-auto-login.sh, which sets up/activates the venv at
~/.local/share/schwab-autologin/venv, runs everything under `xvfb-run`, and
sources schwab-paper.conf (app key + Telegram creds only). Running this file
directly requires `playwright` to already be importable (skip for
--self-test, which needs nothing but the stdlib and does no network I/O).
"""
from __future__ import annotations

import argparse
import json
import os
import re
import secrets
import shutil
import stat
import string
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Callable, Optional
from zoneinfo import ZoneInfo

SCRIPT_DIR = Path(__file__).resolve().parent
DEFAULT_REPO = SCRIPT_DIR.parent

# schwab-paper.conf: app key + Telegram bot creds ONLY. Never login id/password.
DEFAULT_CREDS_FILE = Path(
    os.environ.get(
        "SCHWAB_CREDS_FILE", str(Path.home() / ".config/environment.d/schwab-paper.conf")
    )
)
# autologin.env: SCHWAB_LOGIN_ID / SCHWAB_PASSWORD / SCHWAB_2FA_METHOD. Read
# directly by this script, in-process, and never anywhere else.
DEFAULT_AUTOLOGIN_ENV_FILE = Path(
    os.environ.get(
        "SCHWAB_AUTOLOGIN_ENV", str(Path.home() / ".config/schwabinvestbot/autologin.env")
    )
)
DEFAULT_OWNER_DIR = Path(
    os.environ.get("SCHWAB_OWNER_TOKEN_DIR", str(Path.home() / ".config/schwabinvestbot"))
)
DEFAULT_STATE_DIR = Path(
    os.environ.get("SCHWAB_STATE_DIR", str(Path.home() / ".local/state/schwab-paper"))
)
DEFAULT_REPO_DIR = Path(os.environ.get("SCHWAB_REPO", str(DEFAULT_REPO)))

# Decision logic thresholds (see decide_should_run).
REMAINING_THRESHOLD_HOURS = float(os.environ.get("SCHWAB_AUTOLOGIN_REMAINING_HOURS", "36"))
UNKNOWN_RETRY_DAYS = float(os.environ.get("SCHWAB_AUTOLOGIN_UNKNOWN_RETRY_DAYS", "6"))
# A 2FA prompt needs Barry awake; keeper-triggered runs outside this local-time
# window defer silently (the keeper re-triggers every 5 min, so the first check
# after the window opens picks it up). --force ignores the window.
AWAKE_TZ = ZoneInfo(os.environ.get("SCHWAB_AUTOLOGIN_TZ", "America/Los_Angeles"))
AWAKE_START_HOUR = int(os.environ.get("SCHWAB_AUTOLOGIN_AWAKE_START", "7"))
AWAKE_END_HOUR = int(os.environ.get("SCHWAB_AUTOLOGIN_AWAKE_END", "22"))

MAX_ATTEMPTS_PER_DAY = int(os.environ.get("SCHWAB_AUTOLOGIN_MAX_PER_DAY", "2"))
BACKOFF_HOURS = float(os.environ.get("SCHWAB_AUTOLOGIN_BACKOFF_HOURS", "6"))
CONSENT_TIMEOUT_SECS = float(os.environ.get("SCHWAB_AUTOLOGIN_TIMEOUT_SECS", "120"))
FIELD_WAIT_MS = int(os.environ.get("SCHWAB_AUTOLOGIN_FIELD_WAIT_MS", "8000"))
TWOFA_WAIT_SECS = int(os.environ.get("SCHWAB_2FA_WAIT_SECS", "600"))
TELEGRAM_CONFLICT_GRACE_SECS = float(os.environ.get("SCHWAB_TELEGRAM_CONFLICT_GRACE_SECS", "90"))
TELEGRAM_CONFLICT_RETRY_SECS = 3.0
ARTIFACT_RETENTION_DAYS = 14

REQUIRED_CRED_VARS = ("SCHWAB_LOGIN_ID", "SCHWAB_PASSWORD")
APP_KEY_VARS = ("SCHWAB_APP_KEY", "SCHWAB_CLIENT_ID")
VALID_CREDS_FILE_MODES = (0o600, 0o400)

AUTHORIZE_BASE = "https://api.schwabapi.com/v1/oauth/authorize"
TELEGRAM_API_BASE = "https://api.telegram.org"

# --- Selectors -------------------------------------------------------------
# Login-page selectors below were confirmed against the real, live Schwab
# login page during development (headed Chrome under xvfb-run, --dry-run,
# no credentials submitted): #loginIdInput / #passwordInput matched exactly.
#
# The 2FA method-choice, code-entry, and consent-page selectors below are
# NOT verified against a live page — reaching them requires submitting a
# real password, which this project must not do outside of Barry's own runs.
# They are written defensively (multiple independent fallbacks; text-based
# button matching in addition to CSS ids) and any miss saves a redacted
# screenshot + HTML dump for Barry to inspect and feed back into this file.
LOGIN_ID_SELECTORS = [
    "#loginIdInput",
    "input[name='loginIdInput']",
    "input[name='loginId']",
    "input#loginId",
    "input[id*='loginId' i]",
    "input[autocomplete='username']",
    "input[placeholder*='Login ID' i]",
    "input[aria-label*='Login ID' i]",
]
PASSWORD_SELECTORS = [
    "#passwordInput",
    "input[name='passwordInput']",
    "input[name='password']",
    "input#password",
    "input[id*='password' i]",
    "input[autocomplete='current-password']",
    "input[type='password']",
]
SUBMIT_SELECTORS = [
    "#btnLogin",
    "button#loginSubmit",
    "button[type='submit']",
    "input[type='submit']",
    "button:has-text('Log In')",
    "button:has-text('Log in')",
    "button:has-text('Sign In')",
]
# Method picker, verified live 2026-09-29: sws-gateway.schwab.com/ui/host/#/authenticators,
# "Confirm Your Identity" with cards "Schwab App", "Text me at xxx-xxx-NNNN",
# "Call me at ...", "Call Schwab". No authenticator/security-token card unless
# one is registered in Schwab Security Center.
TWOFA_PICKER_URL_FRAGMENT = "#/authenticators"
TWOFA_METHOD_CARD_TEXT = {"sms": "Text me at", "push": "Schwab App"}
TWOFA_METHODS = tuple(TWOFA_METHOD_CARD_TEXT)
# --- UNVERIFIED: code-entry page after "Text me at" ---
CODE_2FA_SELECTORS = [
    "#otpInput",
    "#securityCode",
    "#smsCode",
    "input[autocomplete='one-time-code']",
    "input[name*='otp' i]",
    "input[name*='securityCode' i]",
    "input[name*='code' i]",
    "input[placeholder*='code' i]",
    "input[inputmode='numeric']",
    "input[type='tel']",
    "input[maxlength='6']",
]
TWOFA_PROMPT_TEXT = [
    "confirm your identity",
    "select which method",
    "verify your identity",
    "choose a method",
    "select a method",
    "enter the code",
    "enter code",
    "security code",
    "verification code",
]
# --- end unverified block ---
CONSENT_TERMS_CHECKBOX = "#acceptTerms"
CONSENT_MODAL_ACCEPT = "#agree-modal-btn-"
CONSENT_SUBMIT = "#submit-btn"
CONSENT_AGREE_TEXT = ["i agree", "i accept", "accept terms", "agree to terms"]
CONSENT_SELECT_ALL_TEXT = ["select all", "all accounts"]
CONSENT_BUTTON_TEXT = ["continue", "allow", "accept", "done", "submit", "next", "authorize"]
CODE_SUBMIT_BUTTON_TEXT = ["verify", "continue", "submit", "log in", "next"]
METHOD_CONFIRM_BUTTON_TEXT = CONSENT_BUTTON_TEXT + ["send code", "text me the code"]

# Schwab's Akamai WAF flatly 403s Playwright's true `headless=True` mode
# (chrome-headless-shell fingerprint) before the login page ever loads.
# Launching in normal mode under Xvfb (see schwab-auto-login.sh) with a
# plain desktop UA reaches the real login page instead. This was confirmed
# against the live gateway during development — see docs/JARVIS_PAPER_HOST.md.
LAUNCH_ARGS = ["--disable-blink-features=AutomationControlled"]
DESKTOP_USER_AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36"
)
VIEWPORT = {"width": 1280, "height": 900}

LOGIN_ERROR_TEXT = [
    "didn't recognize",
    "did not recognize",
    "incorrect login",
    "incorrect password",
    "unable to verify",
    "we can't verify",
    "invalid login",
]
CODE_ERROR_TEXT = ["incorrect code", "invalid code", "code you entered", "try again", "didn't match"]

# Steps that happen strictly BEFORE step_submit_login — a failure here never
# touched Schwab's login-attempt counter, so it must not burn lockout budget.
PRE_SUBMIT_STEPS = frozenset({"goto_authorize", "find_login_fields"})

PLAIN_CODE_RE = re.compile(r"^\s*(\d{6})\s*$")


def nonce_code_re(nonce: str) -> re.Pattern:
    return re.compile(rf"^\s*{re.escape(nonce)}\s+(\d{{6}})\s*$", re.IGNORECASE)


class StepError(Exception):
    """Raised by a flow step; carries the step name for logs/artifacts/Telegram."""

    def __init__(self, step: str, message: str):
        super().__init__(message)
        self.step = step
        self.message = message


# --------------------------------------------------------------------------
# Small helpers
# --------------------------------------------------------------------------


def now_utc() -> datetime:
    return datetime.now(timezone.utc)


def iso(dt: datetime) -> str:
    return dt.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def parse_iso(s: str) -> datetime:
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


def read_json(path: Path) -> Optional[dict]:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return None


def load_env_file(path: Path) -> dict:
    """Tiny KEY=VALUE parser for environment.d-style files (no export, no eval)."""
    out: dict[str, str] = {}
    if not path.is_file():
        return out
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, _, v = line.partition("=")
        k = k.strip()
        v = v.strip().strip("'\"")
        if k:
            out[k] = v
    return out


class Logger:
    """Logs step names/short messages only — callers are responsible for never
    passing secrets in."""

    def __init__(self, log_path: Optional[Path]):
        self.log_path = log_path
        if log_path is not None:
            log_path.parent.mkdir(parents=True, exist_ok=True)

    def line(self, msg: str) -> None:
        stamped = f"{iso(now_utc())} {msg}"
        print(stamped, file=sys.stderr)
        if self.log_path is not None:
            try:
                with self.log_path.open("a", encoding="utf-8") as fh:
                    fh.write(stamped + "\n")
            except Exception:
                pass


# --------------------------------------------------------------------------
# autologin.env: permission-checked, in-process-only credentials
# --------------------------------------------------------------------------


def validate_creds_file_perms(path: Path) -> tuple[bool, str]:
    """Refuse anything but mode 600/400, owned by the current user.

    Pure and side-effect-free (aside from the stat() call) so it is directly
    unit-testable in --self-test with real temp files.
    """
    try:
        st = path.stat()
    except FileNotFoundError:
        return False, f"{path} does not exist"
    except OSError as exc:
        return False, f"{path} could not be stat()'d: {exc}"
    if not stat.S_ISREG(st.st_mode):
        return False, f"{path} is not a regular file"
    mode = stat.S_IMODE(st.st_mode)
    if mode not in VALID_CREDS_FILE_MODES:
        allowed = "/".join(oct(m) for m in VALID_CREDS_FILE_MODES)
        return False, f"{path} must be mode {allowed} (found {oct(mode)}); run: chmod 600 {path}"
    if st.st_uid != os.getuid():
        return False, f"{path} must be owned by the current user (uid {os.getuid()}, found {st.st_uid})"
    return True, "ok"


def load_autologin_creds(path: Path) -> tuple[Optional[dict], str]:
    """Loads SCHWAB_LOGIN_ID / SCHWAB_PASSWORD / SCHWAB_2FA_METHOD directly
    from autologin.env, entirely in-process. The returned dict must NEVER be
    merged into os.environ or passed as a subprocess env= — see module
    docstring. Returns (None, reason) if the file is missing/misconfigured.
    """
    ok, reason = validate_creds_file_perms(path)
    if not ok:
        return None, reason
    return load_env_file(path), "ok"


# --------------------------------------------------------------------------
# Attempt ledger / lockout protection
# --------------------------------------------------------------------------


class AttemptLedger:
    def __init__(self, path: Path, max_per_day: int = 2, backoff_hours: float = 6.0):
        self.path = path
        self.max_per_day = max_per_day
        self.backoff_hours = backoff_hours
        self._data = self._load()

    def _load(self) -> dict:
        data = read_json(self.path)
        if not isinstance(data, dict) or not isinstance(data.get("attempts"), list):
            return {"attempts": []}
        return data

    def _save(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(self.path.suffix + f".tmp.{os.getpid()}")
        tmp.write_text(json.dumps(self._data, indent=2, sort_keys=True), encoding="utf-8")
        os.chmod(tmp, 0o600)
        tmp.replace(self.path)
        os.chmod(self.path, 0o600)

    def attempts(self) -> list[dict]:
        return self._data.setdefault("attempts", [])

    def check_lockout(self, now: Optional[datetime] = None, bypass: bool = False) -> tuple[bool, str]:
        if bypass:
            return True, "bypassed (--force --i-know)"
        now = now or now_utc()
        recent = []
        for a in self.attempts():
            try:
                at = parse_iso(a["at"])
            except Exception:
                continue
            if now - at < timedelta(hours=24):
                recent.append((at, a))
        if len(recent) >= self.max_per_day:
            return False, f"daily cap reached ({len(recent)}/{self.max_per_day} attempts in the last 24h)"
        failures = [at for at, a in recent if not a.get("ok", False)]
        # Backoff also applies to failures older than 24h but within the window.
        for a in self.attempts():
            if a.get("ok", False):
                continue
            try:
                at = parse_iso(a["at"])
            except Exception:
                continue
            if now - at < timedelta(hours=self.backoff_hours):
                failures.append(at)
        if failures:
            last_fail = max(failures)
            remaining = timedelta(hours=self.backoff_hours) - (now - last_fail)
            if remaining > timedelta(0):
                return False, f"backoff after a recent failure active for another {remaining}"
        return True, "ok"

    def record(self, ok: bool, reason: str, now: Optional[datetime] = None) -> None:
        now = now or now_utc()
        self.attempts().append({"at": iso(now), "ok": bool(ok), "reason": reason[:200]})
        cutoff = now - timedelta(days=30)
        self._data["attempts"] = [a for a in self.attempts() if _safe_after(a.get("at"), cutoff)]
        self._save()


def _safe_after(at: Any, cutoff: datetime) -> bool:
    if not isinstance(at, str):
        return False
    try:
        return parse_iso(at) > cutoff
    except Exception:
        return False


# --------------------------------------------------------------------------
# Decision logic (should we even try?)
# --------------------------------------------------------------------------


@dataclass
class Paths:
    repo: Path
    owner_dir: Path
    state_dir: Path
    creds_file: Path
    autologin_env_file: Path

    @property
    def keeper_status_file(self) -> Path:
        return self.state_dir / "token-keeper.json"

    @property
    def ledger_file(self) -> Path:
        return self.state_dir / "auto-login.json"

    @property
    def last_success_file(self) -> Path:
        return self.state_dir / "auto-login-last-success.json"

    @property
    def log_file(self) -> Path:
        return self.state_dir / "auto-login.log"

    @property
    def artifacts_dir(self) -> Path:
        return self.state_dir / "auto-login"


def parse_json_blob(out: str) -> Optional[dict]:
    """schwab --json pretty-prints (multi-line), so this parses the whole
    blob rather than assuming a single line. Falls back to the substring
    starting at the first '{' in case a banner/warning line ever precedes
    the JSON on stdout."""
    out = (out or "").strip()
    if not out:
        return None
    try:
        return json.loads(out)
    except Exception:
        pass
    brace = out.find("{")
    if brace == -1:
        return None
    try:
        return json.loads(out[brace:])
    except Exception:
        return None


def run_schwab_json(args: list[str], env: dict, timeout: float = 30.0) -> Optional[dict]:
    try:
        proc = subprocess.run(
            ["schwab", *args, "--json"], env=env, capture_output=True, text=True, timeout=timeout
        )
    except Exception:
        return None
    return parse_json_blob(proc.stdout)


def read_last_success(paths: Paths) -> Optional[datetime]:
    data = read_json(paths.last_success_file)
    at = (data or {}).get("at")
    if not at:
        return None
    try:
        return parse_iso(at)
    except Exception:
        return None


def write_last_success(paths: Paths, now: Optional[datetime] = None) -> None:
    now = now or now_utc()
    paths.state_dir.mkdir(parents=True, exist_ok=True)
    tmp = paths.last_success_file.with_suffix(".tmp")
    tmp.write_text(json.dumps({"at": iso(now)}), encoding="utf-8")
    tmp.replace(paths.last_success_file)


def in_awake_window(now: Optional[datetime] = None) -> bool:
    local = (now or now_utc()).astimezone(AWAKE_TZ)
    return AWAKE_START_HOUR <= local.hour < AWAKE_END_HOUR


def decide_should_run(paths: Paths, env: dict, log: Logger) -> tuple[bool, str]:
    """No-op unless the refresh token has < REMAINING_THRESHOLD_HOURS left, or
    its remaining life is unknown and it has been >= UNKNOWN_RETRY_DAYS since
    the last successful auto-login (or there has never been one).

    `login_at` / `refresh_expiry_known` come straight from
    `schwab auth status --json` — the real source of truth now that the
    parallel token-lifetime change has landed in crates/. This deliberately
    no longer reads tokens.json directly or consults the keeper's status
    file: those were reasons-to-run in the old design, but the new schedule
    is intentionally just "is the clock actually close to zero".
    """
    status_envelope = run_schwab_json(["auth", "status"], env)
    data = (status_envelope or {}).get("data") or {}
    refresh_expiry_known = bool(data.get("refresh_expiry_known"))
    remaining = data.get("refresh_expires_in_seconds")
    unknown_reason_prefix = (
        "schwab auth status --json produced no output"
        if status_envelope is None
        else "refresh_expiry_known is false/missing"
    )

    if refresh_expiry_known and isinstance(remaining, (int, float)):
        remaining_h = remaining / 3600.0
        if remaining_h < REMAINING_THRESHOLD_HOURS:
            return True, f"refresh token remaining {remaining_h:.1f}h (< {REMAINING_THRESHOLD_HOURS}h)"
        return False, f"refresh token remaining {remaining_h:.1f}h (>= {REMAINING_THRESHOLD_HOURS}h)"

    last_success = read_last_success(paths)
    if last_success is None:
        return True, f"{unknown_reason_prefix} and no prior auto-login success on record"
    age_days = (now_utc() - last_success).total_seconds() / 86400.0
    if age_days >= UNKNOWN_RETRY_DAYS:
        return True, f"{unknown_reason_prefix} (unknown) and {age_days:.1f}d since last success (>= {UNKNOWN_RETRY_DAYS}d cap)"
    return False, f"{unknown_reason_prefix} (unknown) but only {age_days:.1f}d since last success (< {UNKNOWN_RETRY_DAYS}d cap)"


# --------------------------------------------------------------------------
# Credentials / env
# --------------------------------------------------------------------------


def build_run_env(paths: Paths) -> dict:
    """os.environ, topped up from schwab-paper.conf for anything missing:
    the Schwab app key and Telegram bot creds ONLY. SCHWAB_LOGIN_ID /
    SCHWAB_PASSWORD are never read from here — see load_autologin_creds.
    """
    env = dict(os.environ)
    file_vars = load_env_file(paths.creds_file)
    for k, v in file_vars.items():
        env.setdefault(k, v)
    cargo_bin = str(Path.home() / ".cargo/bin")
    path = env.get("PATH", "")
    if cargo_bin not in path.split(":"):
        env["PATH"] = f"{cargo_bin}:{path}" if path else cargo_bin
    return env


def missing_app_key(env: dict) -> Optional[str]:
    if not any(env.get(v) for v in APP_KEY_VARS):
        return "/".join(APP_KEY_VARS)
    return None


def missing_login_creds(creds: dict) -> list[str]:
    return [v for v in REQUIRED_CRED_VARS if not creds.get(v)]


def authorize_url(env: dict) -> str:
    key = env.get("SCHWAB_APP_KEY") or env.get("SCHWAB_CLIENT_ID") or ""
    redirect = env.get("SCHWAB_REDIRECT_URI", "https://127.0.0.1:8182")
    qs = urllib.parse.urlencode(
        {"client_id": key, "redirect_uri": redirect, "response_type": "code"}
    )
    return f"{AUTHORIZE_BASE}?{qs}"


def redirect_host_port(env: dict) -> tuple[str, int]:
    redirect = env.get("SCHWAB_REDIRECT_URI", "https://127.0.0.1:8182")
    parts = urllib.parse.urlsplit(redirect)
    host = parts.hostname or "127.0.0.1"
    port = parts.port or (443 if parts.scheme == "https" else 80)
    return host, port


def redact_url_for_log(url: str) -> str:
    """Only ever used for *diagnostic* logging of the authorize URL, which
    carries no secret beyond the app's public client_id — still redacted."""
    parts = urllib.parse.urlsplit(url)
    qs = dict(urllib.parse.parse_qsl(parts.query))
    if "client_id" in qs:
        qs["client_id"] = "REDACTED"
    if "code" in qs:
        qs["code"] = "REDACTED"
    new_qs = urllib.parse.urlencode(qs)
    return urllib.parse.urlunsplit((parts.scheme, parts.netloc, parts.path, new_qs, ""))


# --------------------------------------------------------------------------
# Telegram: alerts, and the long-poll 2FA reply wait
# --------------------------------------------------------------------------


def notify_telegram(env: dict, text: str, log: Logger) -> None:
    token = env.get("TELEGRAM_BOT_TOKEN")
    chat = env.get("TELEGRAM_CHAT_ID")
    if not token or not chat:
        log.line("telegram: skipped (TELEGRAM_BOT_TOKEN / TELEGRAM_CHAT_ID unset)")
        return
    data = urllib.parse.urlencode({"chat_id": chat, "text": text}).encode()
    req = urllib.request.Request(
        f"{TELEGRAM_API_BASE}/bot{token}/sendMessage", data=data, method="POST"
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            resp.read()
    except Exception as exc:
        log.line(f"telegram: send failed: {exc}")


def telegram_get_updates(env: dict, offset: Optional[int], timeout_secs: int) -> dict:
    """One getUpdates call. Raises StepError('telegram_conflict', ...) on HTTP
    409 (another poller/webhook holds this bot token) per the design doc —
    verified nothing else on jarvis polls this token, but fail loudly if
    that ever changes rather than silently missing Barry's reply."""
    token = env.get("TELEGRAM_BOT_TOKEN")
    if not token:
        raise StepError("telegram_config", "TELEGRAM_BOT_TOKEN not set")
    params: dict[str, Any] = {"timeout": timeout_secs, "allowed_updates": json.dumps(["message"])}
    if offset is not None:
        params["offset"] = offset
    url = f"{TELEGRAM_API_BASE}/bot{token}/getUpdates?{urllib.parse.urlencode(params)}"
    req = urllib.request.Request(url, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=timeout_secs + 15) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        if exc.code == 409:
            raise StepError(
                "telegram_conflict",
                "Telegram getUpdates returned HTTP 409 — another poller or webhook is "
                "active on this bot token",
            ) from exc
        raise StepError("telegram_error", f"Telegram getUpdates HTTP {exc.code}") from exc
    except StepError:
        raise
    except Exception as exc:
        raise StepError("telegram_error", f"Telegram getUpdates failed: {type(exc).__name__}") from exc


def extract_code_from_updates(
    updates: list[dict], chat_id: int, nonce: str, min_update_id: int
) -> tuple[Optional[str], int]:
    """Pure filter over a Telegram getUpdates 'result' list.

    Accepts ONLY a message where chat.id == chat_id AND from.id == chat_id
    (i.e. Barry himself, in his private chat with the bot — not a group, not
    an impersonator), whose text is exactly 6 digits or "<nonce> 123456".
    Every update's id — matching or not — advances the returned offset, so
    stale/irrelevant messages are never re-delivered on the next poll. Any
    update whose id is already below min_update_id is ignored outright
    (defensive: the server should already exclude these via the offset
    param, but this must not regress if that ever changes).

    Pure and side-effect-free — this is what --self-test exercises directly.
    """
    new_offset = min_update_id
    code: Optional[str] = None
    n_re = nonce_code_re(nonce)
    for upd in updates:
        uid = upd.get("update_id")
        if isinstance(uid, int):
            if uid < min_update_id:
                continue
            new_offset = max(new_offset, uid + 1)
        if code is not None:
            continue
        msg = upd.get("message") or {}
        chat = msg.get("chat") or {}
        frm = msg.get("from") or {}
        if chat.get("id") != chat_id or frm.get("id") != chat_id:
            continue
        text = msg.get("text") or ""
        m = PLAIN_CODE_RE.match(text) or n_re.match(text)
        if m:
            code = m.group(1)
    return code, new_offset


def wait_for_telegram_code(
    env: dict,
    log: Logger,
    chat_id: int,
    nonce: str,
    prompt: str,
    wait_secs: int,
    get_updates: Callable[[Optional[int], int], dict] = None,
) -> Optional[str]:
    """Sends `prompt`, then long-polls for up to wait_secs for a matching
    reply. `get_updates` is injectable for tests; defaults to the real
    Telegram call."""
    if get_updates is None:
        get_updates = lambda offset, timeout_secs: telegram_get_updates(env, offset, timeout_secs)
    raw_get_updates = get_updates
    conflict_started: list[float] = []

    def get_updates(offset: Optional[int], timeout_secs: int) -> dict:
        # A transient 409 (some other client briefly polled this token) should
        # not burn a login attempt; only a sustained conflict is fatal.
        while True:
            try:
                resp = raw_get_updates(offset, timeout_secs)
                conflict_started.clear()
                return resp
            except StepError as e:
                if e.step != "telegram_conflict":
                    raise
                if not conflict_started:
                    conflict_started.append(time.time())
                    log.line("step=telegram_conflict_retry (HTTP 409; retrying)")
                if time.time() - conflict_started[0] > TELEGRAM_CONFLICT_GRACE_SECS:
                    raise
                time.sleep(TELEGRAM_CONFLICT_RETRY_SECS)

    # Baseline: ignore anything already sitting in the update queue before we
    # even asked (e.g. an old code from a previous run).
    baseline = get_updates(None, 0)
    max_id = 0
    for upd in baseline.get("result") or []:
        uid = upd.get("update_id")
        if isinstance(uid, int):
            max_id = max(max_id, uid)
    offset = max_id + 1

    notify_telegram(env, prompt, log)
    log.line(f"step=telegram_prompt_sent nonce={nonce} wait_secs={wait_secs}")

    deadline = time.time() + wait_secs
    while time.time() < deadline:
        remaining = max(1, int(deadline - time.time()))
        poll_timeout = min(30, remaining)
        resp = get_updates(offset, poll_timeout)
        code, offset = extract_code_from_updates(resp.get("result") or [], chat_id, nonce, offset)
        if code:
            log.line("step=telegram_code_received")
            return code
    return None


def make_nonce(n: int = 4) -> str:
    alphabet = string.ascii_uppercase + string.digits
    return "".join(secrets.choice(alphabet) for _ in range(n))


# --------------------------------------------------------------------------
# Failure artifacts
# --------------------------------------------------------------------------


def strip_input_values(html: str) -> str:
    return re.sub(r'(value\s*=\s*")[^"]*(")', r"\1[stripped]\2", html, flags=re.IGNORECASE)


def save_failure_artifacts(page, paths: Paths, step: str, log: Logger) -> Optional[Path]:
    ts = now_utc().strftime("%Y%m%dT%H%M%SZ")
    out_dir = paths.artifacts_dir / ts
    try:
        out_dir.mkdir(parents=True, exist_ok=True)
        os.chmod(out_dir, 0o700)
    except Exception as exc:
        log.line(f"artifact dir creation failed: {exc}")
        return None

    try:
        shot = out_dir / "screenshot.png"
        page.screenshot(path=str(shot), full_page=True)
        os.chmod(shot, 0o600)
    except Exception as exc:
        log.line(f"screenshot capture failed: {exc}")

    try:
        html = strip_input_values(page.content())
        html_path = out_dir / "page.html"
        html_path.write_text(html, encoding="utf-8")
        os.chmod(html_path, 0o600)
    except Exception as exc:
        log.line(f"page HTML capture failed: {exc}")

    try:
        step_path = out_dir / "step.txt"
        step_path.write_text(step + "\n", encoding="utf-8")
        os.chmod(step_path, 0o600)
    except Exception:
        pass

    return out_dir


def prune_old_artifacts(paths: Paths, days: int = ARTIFACT_RETENTION_DAYS) -> None:
    root = paths.artifacts_dir
    if not root.is_dir():
        return
    cutoff = time.time() - days * 86400
    for child in root.iterdir():
        try:
            if child.is_dir() and child.stat().st_mtime < cutoff:
                shutil.rmtree(child, ignore_errors=True)
        except Exception:
            continue


# --------------------------------------------------------------------------
# Playwright flow steps
# --------------------------------------------------------------------------


def find_first_visible(page, selectors: list[str], timeout_ms: int = FIELD_WAIT_MS):
    for sel in selectors:
        try:
            loc = page.locator(sel).first
            loc.wait_for(state="visible", timeout=timeout_ms)
            return sel, loc
        except Exception:
            continue
    return None, None


TEXT_BUTTON_TAGS = ("button", "a", "input[type='submit']", "input[type='button']")


def find_text_button(page, phrases: list[str], timeout_ms: int = 1500):
    """One combined wait for any phrase/tag, so a miss costs timeout_ms total."""
    candidates = [(p, f"{tag}:has-text('{p}')") for p in phrases for tag in TEXT_BUTTON_TAGS]
    try:
        page.locator(", ".join(sel for _p, sel in candidates)).first.wait_for(
            state="visible", timeout=timeout_ms
        )
    except Exception:
        return None, None
    for phrase, sel in candidates:
        loc = page.locator(sel).first
        try:
            if loc.is_visible() and loc.is_enabled():
                return phrase, loc
        except Exception:
            continue
    return None, None


def page_has_text(page, phrases: list[str]) -> Optional[str]:
    try:
        body = page.inner_text("body").lower()
    except Exception:
        return None
    for phrase in phrases:
        if phrase.lower() in body:
            return phrase
    return None


def step_goto_authorize(page, url: str, log: Logger) -> None:
    log.line("step=goto_authorize")
    page.goto(url, wait_until="load", timeout=30000)


def step_find_login_fields(page, log: Logger) -> tuple[str, Any, str, Any]:
    log.line("step=find_login_fields")
    login_sel, login_loc = find_first_visible(page, LOGIN_ID_SELECTORS)
    if login_loc is None:
        raise StepError("find_login_fields", "no login id field matched any known selector")
    pw_sel, pw_loc = find_first_visible(page, PASSWORD_SELECTORS)
    if pw_loc is None:
        raise StepError("find_login_fields", "no password field matched any known selector")
    return login_sel, login_loc, pw_sel, pw_loc


def step_fill_credentials(login_loc, pw_loc, login_id: str, password: str, log: Logger) -> None:
    log.line("step=fill_credentials")
    login_loc.fill(login_id)
    pw_loc.fill(password)


def step_submit_login(page, log: Logger) -> None:
    log.line("step=submit_login")
    sel, loc = find_first_visible(page, SUBMIT_SELECTORS, timeout_ms=5000)
    if loc is None:
        raise StepError("submit_login", "no submit button matched any known selector")
    loc.click()


def try_detect_login_error(page) -> Optional[str]:
    return page_has_text(page, LOGIN_ERROR_TEXT)


def try_detect_code_error(page) -> Optional[str]:
    return page_has_text(page, CODE_ERROR_TEXT)


def on_2fa_picker(page) -> bool:
    return TWOFA_PICKER_URL_FRAGMENT in page.url or page_has_text(page, TWOFA_PROMPT_TEXT[:2]) is not None


def wait_for_2fa_picker(page, timeout_s: float, captured: dict) -> bool:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        if captured.get("url"):
            return False
        if on_2fa_picker(page):
            return True
        time.sleep(0.5)
    return False


def choose_2fa_method(page, method: str, log: Logger) -> None:
    card_text = TWOFA_METHOD_CARD_TEXT[method]
    try:
        card = page.get_by_text(card_text, exact=False).first
        card.wait_for(state="visible", timeout=5000)
        card.click()
    except Exception as exc:
        raise StepError("2fa_method", f"could not click the {card_text!r} card: {type(exc).__name__}")
    log.line(f"step=choose_2fa_method method={method} card={card_text!r}")
    _phrase, cont = find_text_button(page, METHOD_CONFIRM_BUTTON_TEXT, timeout_ms=1500)
    if cont is not None:
        cont.click()


def find_2fa_code_field(page, timeout_ms: int = FIELD_WAIT_MS):
    """One combined wait (not per-selector) so a miss costs timeout_ms, not N×timeout_ms."""
    combined = page.locator(", ".join(CODE_2FA_SELECTORS)).first
    try:
        combined.wait_for(state="visible", timeout=timeout_ms)
    except Exception:
        return None, None
    for sel in CODE_2FA_SELECTORS:
        loc = page.locator(sel).first
        try:
            if loc.is_visible():
                return sel, loc
        except Exception:
            continue
    return None, None


def submit_2fa_code(page, code: str, log: Logger) -> None:
    sel, loc = find_2fa_code_field(page, timeout_ms=2000)
    if loc is None:
        raise StepError(
            "2fa_field",
            "2FA code field not found when trying to submit (page structure may have "
            "changed since the field was first detected — selectors unverified)",
        )
    log.line(f"step=submit_2fa_code selector={sel}")
    loc.fill(code)
    _phrase, submit = find_text_button(page, CODE_SUBMIT_BUTTON_TEXT, timeout_ms=1500)
    if submit is not None:
        submit.click()
    else:
        loc.press("Enter")


def twofa_method(creds: dict) -> Optional[str]:
    method = (creds.get("SCHWAB_2FA_METHOD") or "sms").strip().lower()
    return method if method in TWOFA_METHODS else None


def wait_for_push_approval(page, captured: dict, timeout_s: float) -> bool:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        if captured.get("url"):
            return True
        if TWOFA_PICKER_URL_FRAGMENT not in page.url and not page_has_text(page, TWOFA_PROMPT_TEXT):
            return True
        time.sleep(2.0)
    return False


def handle_2fa_if_present(
    page, browser_env: dict, creds: dict, captured: dict, log: Logger, chat_id: Optional[int]
) -> None:
    """If Schwab shows the "Confirm Your Identity" picker, walk it with Barry
    over Telegram (SMS code relay or Schwab App push approval); otherwise
    no-op (e.g. a trusted-device cookie skipped 2FA). Raises StepError on any
    failure — all after step_submit_login, so they count toward the lockout
    budget (see PRE_SUBMIT_STEPS / main()).
    """
    if not wait_for_2fa_picker(page, 20.0, captured):
        log.line("step=2fa not_present (no method picker within 20s)")
        return
    log.line("step=2fa_picker_found")

    if chat_id is None:
        raise StepError("telegram_config", "TELEGRAM_CHAT_ID not set; cannot run 2FA")

    method = twofa_method(creds) or "sms"
    choose_2fa_method(page, method, log)
    wait_min = max(1, TWOFA_WAIT_SECS // 60)

    if method == "push":
        notify_telegram(
            browser_env,
            f"📱 Schwab re-login on jarvis: approve the login in the Schwab app within {wait_min} min.",
            log,
        )
        if not wait_for_push_approval(page, captured, TWOFA_WAIT_SECS):
            notify_telegram(browser_env, "⏰ Schwab app approval not received; will retry at the next schedule.", log)
            raise StepError("2fa_timeout", "Schwab app push not approved before timeout")
        notify_telegram(browser_env, "✅ approved, finishing login", log)
        return

    sel, loc = find_2fa_code_field(page, timeout_ms=20000)
    if loc is None:
        raise StepError(
            "2fa_field",
            "picked 'Text me' but no known code-field selector matched (see saved artifacts)",
        )
    log.line(f"step=2fa_field_found selector={sel}")

    for attempt in (1, 2):
        nonce = make_nonce()
        prompt = (
            f"🔐 Schwab re-login on jarvis: Schwab just texted you a code. "
            f"Reply here with the 6-digit code within {wait_min} min. (nonce {nonce})"
        )
        if attempt > 1:
            prompt = "❗ Retry — the first code was rejected. " + prompt
        code = wait_for_telegram_code(browser_env, log, chat_id, nonce, prompt, TWOFA_WAIT_SECS)
        if code is None:
            deadline_str = iso(now_utc() + timedelta(days=7))
            notify_telegram(
                browser_env,
                f"⏰ No 2FA code received; tokens expire around {deadline_str}; "
                "will retry at the next schedule.",
                log,
            )
            raise StepError("2fa_timeout", "no 2FA code received via Telegram before timeout")

        submit_2fa_code(page, code, log)
        time.sleep(1.5)
        err = try_detect_code_error(page)
        if not err:
            notify_telegram(browser_env, "✅ code accepted, finishing login", log)
            return
        log.line(f"step=2fa_rejected attempt={attempt} reason={err}")
        if attempt == 1:
            notify_telegram(browser_env, "❌ code rejected — one more try", log)
            continue
        raise StepError("2fa", f"2FA code rejected twice: {err}")


def click_if_visible(page, selector: str, log: Logger, label: str) -> bool:
    loc = page.locator(selector).first
    try:
        if not (loc.is_visible() and loc.is_enabled()):
            return False
        loc.click(timeout=3000)
    except Exception:
        return False
    log.line(f"step=consent_click {label}")
    return True


def try_handle_consent(page, log: Logger) -> bool:
    # Trader API terms page, verified live 2026-09-29: checkbox #acceptTerms
    # (may raise a confirm modal with #agree-modal-btn- "Accept"), then
    # #submit-btn "Continue". Later pages (account pick, done) reuse #submit-btn
    # or fall through to the generic text-button matching below.
    terms = page.locator(CONSENT_TERMS_CHECKBOX).first
    try:
        if terms.is_visible() and not terms.is_checked():
            terms.check(timeout=3000)
            log.line("step=consent_click terms_checkbox")
            return True
    except Exception:
        pass
    if click_if_visible(page, CONSENT_MODAL_ACCEPT, log, "modal_accept"):
        return True
    if click_if_visible(page, CONSENT_SUBMIT, log, "submit_btn"):
        return True

    acted = False
    phrase, agree = find_text_button(page, CONSENT_AGREE_TEXT, timeout_ms=1200)
    if agree is not None:
        try:
            agree.check()
        except Exception:
            agree.click()
        acted = True
    phrase, select_all = find_text_button(page, CONSENT_SELECT_ALL_TEXT, timeout_ms=1200)
    if select_all is not None:
        select_all.click()
        acted = True
    phrase, cont = find_text_button(page, CONSENT_BUTTON_TEXT, timeout_ms=1200)
    if cont is not None:
        log.line(f"step=consent_click phrase={phrase}")
        cont.click()
        acted = True
    return acted


def run_login_flow(
    page, browser_env: dict, creds: dict, captured: dict, log: Logger, chat_id: Optional[int]
) -> str:
    url = authorize_url(browser_env)
    step_goto_authorize(page, url, log)

    login_sel, login_loc, pw_sel, pw_loc = step_find_login_fields(page, log)
    log.line(f"step=selectors_found login={login_sel} password={pw_sel}")

    step_fill_credentials(login_loc, pw_loc, creds["SCHWAB_LOGIN_ID"], creds["SCHWAB_PASSWORD"], log)
    step_submit_login(page, log)
    # Everything from here on happened after Schwab saw the credentials.

    time.sleep(1.5)
    err = try_detect_login_error(page)
    if err:
        raise StepError("credentials", f"login rejected: {err}")

    if captured.get("url"):
        return captured["url"]

    handle_2fa_if_present(page, browser_env, creds, captured, log, chat_id)

    if captured.get("url"):
        return captured["url"]

    deadline = time.time() + CONSENT_TIMEOUT_SECS
    while time.time() < deadline:
        if captured.get("url"):
            return captured["url"]
        if try_handle_consent(page, log):
            time.sleep(1.0)
            continue
        time.sleep(1.0)

    raise StepError("redirect_wait", f"timed out after {CONSENT_TIMEOUT_SECS}s waiting for OAuth redirect")


def dry_run_flow(page, env: dict, log: Logger) -> int:
    url = authorize_url(env)
    log.line(f"step=goto_authorize url={redact_url_for_log(url)}")
    step_goto_authorize(page, url, log)
    try:
        login_sel, login_loc, pw_sel, pw_loc = step_find_login_fields(page, log)
    except StepError as e:
        print(f"dry-run FAILED at step={e.step}: {e.message}", file=sys.stderr)
        print(f"page title: {page.title()!r}", file=sys.stderr)
        print(f"page url: {redact_url_for_log(page.url)}", file=sys.stderr)
        return 1
    print("dry-run OK — login form detected, stopping before submit.")
    print(f"page title: {page.title()!r}")
    print(f"page url: {redact_url_for_log(page.url)}")
    print(f"login id selector: {login_sel}")
    print(f"password selector: {pw_sel}")
    return 0


# --------------------------------------------------------------------------
# Locking + schwab CLI calls
# --------------------------------------------------------------------------


def acquire_lock(lock_path: Path, log: Logger, timeout_s: float = 30.0) -> int:
    import fcntl

    lock_path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(str(lock_path), os.O_CREAT | os.O_RDWR, 0o600)
    deadline = time.time() + timeout_s
    while True:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return fd
        except BlockingIOError:
            if time.time() >= deadline:
                os.close(fd)
                raise StepError("lock", "token-keeper lock busy; another refresh in progress")
            time.sleep(0.5)


def release_lock(fd: int) -> None:
    import fcntl

    try:
        fcntl.flock(fd, fcntl.LOCK_UN)
    finally:
        os.close(fd)


def run_schwab_auth_login(redirect_url: str, env: dict, log: Logger) -> dict:
    log.line("step=schwab_auth_login")
    try:
        proc = subprocess.run(
            ["schwab", "auth", "login", "--code", redirect_url, "--json"],
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
        )
    except Exception as exc:
        raise StepError("exchange", f"schwab auth login could not be run: {exc}") from exc
    if proc.returncode != 0:
        raise StepError("exchange", f"schwab auth login exited {proc.returncode}")
    data = parse_json_blob(proc.stdout) or {}
    if not data.get("success", False):
        raise StepError("exchange", "schwab auth login reported success=false")
    return data


def run_token_keeper(paths: Paths, env: dict, log: Logger) -> None:
    script = paths.repo / "scripts" / "jarvis-token-keeper.sh"
    if not script.is_file():
        log.line(f"step=token_keeper skipped (not found: {script})")
        return
    log.line("step=token_keeper")
    try:
        proc = subprocess.run([str(script)], env=env, capture_output=True, text=True, timeout=60)
        tail = (proc.stdout or proc.stderr or "").strip().splitlines()[-1:] or [""]
        log.line(f"step=token_keeper rc={proc.returncode} last_line={tail[0][:160]}")
    except Exception as exc:
        log.line(f"step=token_keeper failed to run: {exc}")


def verify_authenticated(env: dict) -> dict:
    envelope = run_schwab_json(["auth", "status"], env)
    data = (envelope or {}).get("data") or {}
    if not data.get("authenticated", False):
        raise StepError("verify", "auth status not authenticated after login")
    return data


def compute_refresh_valid_until(data: dict) -> str:
    # Deliberately does NOT fall back to obtained_at: refreshing an access token does not
    # reset the refresh token's 7-day clock, so obtained_at would understate how stale the
    # login really is and silently hide the need to re-auth (same reasoning as the
    # `refresh_expiry_known` flag in `schwab auth status --json`).
    login_at = data.get("login_at")
    if not login_at:
        return "unknown (login_at not reported by this schwab build yet)"
    try:
        return iso(parse_iso(login_at) + timedelta(days=7))
    except Exception:
        return "unknown"


# --------------------------------------------------------------------------
# Self-test (no network, no browser)
# --------------------------------------------------------------------------


def self_test() -> int:
    failures = 0

    def check(name: str, cond: bool) -> None:
        nonlocal failures
        print(f"[{'PASS' if cond else 'FAIL'}] {name}")
        if not cond:
            failures += 1

    import tempfile

    CHAT = 555111222

    def upd(uid, chat_id=CHAT, from_id=CHAT, text="123456"):
        return {"update_id": uid, "message": {"chat": {"id": chat_id}, "from": {"id": from_id}, "text": text}}

    # --- Telegram reply filter (pure function, no network) ---
    code, off = extract_code_from_updates([upd(1)], CHAT, "ABCD", 0)
    check("plain 6-digit code from the right chat/from id matches", code == "123456" and off == 2)

    code, off = extract_code_from_updates([upd(1, text="ABCD 654321")], CHAT, "ABCD", 0)
    check("nonce-prefixed form '<nonce> 123456' matches", code == "654321")

    code, off = extract_code_from_updates([upd(1, text="abcd 654321")], CHAT, "ABCD", 0)
    check("nonce match is case-insensitive", code == "654321")

    code, off = extract_code_from_updates([upd(1, chat_id=999)], CHAT, "ABCD", 0)
    check("wrong chat id is ignored", code is None)

    code, off = extract_code_from_updates([upd(1, from_id=999)], CHAT, "ABCD", 0)
    check("right chat but wrong from id (e.g. group impersonation) is ignored", code is None)

    code, off = extract_code_from_updates([upd(1, text="12345")], CHAT, "ABCD", 0)
    check("5-digit code (wrong length) is ignored", code is None)

    code, off = extract_code_from_updates([upd(1, text="not a code")], CHAT, "ABCD", 0)
    check("non-matching text is ignored", code is None)

    code, off = extract_code_from_updates([upd(5)], CHAT, "ABCD", min_update_id=10)
    check("stale update (id below current offset) is ignored even if it would match", code is None and off == 10)

    updates = [upd(1, chat_id=999), upd(2, text="not it"), upd(3)]
    code, off = extract_code_from_updates(updates, CHAT, "ABCD", 0)
    check(
        "first matching update wins; offset advances past ALL updates in the batch",
        code == "123456" and off == 4,
    )

    code, off = extract_code_from_updates([{"update_id": 1, "message": {}}], CHAT, "ABCD", 0)
    check("malformed/empty message does not crash and is ignored", code is None)

    code, off = extract_code_from_updates([upd(1, text="  123456  ")], CHAT, "ABCD", 0)
    check("surrounding whitespace is tolerated", code == "123456")

    # --- Telegram getUpdates: stubbed HTTP (no real network) ---
    import urllib.request as _ur

    original_urlopen = _ur.urlopen

    class _FakeResp:
        def __init__(self, payload: bytes):
            self._payload = payload

        def read(self):
            return self._payload

        def __enter__(self):
            return self

        def __exit__(self, *a):
            return False

    def fake_urlopen(req, timeout=None):
        if "conflict-token" in req.full_url:
            raise urllib.error.HTTPError(req.full_url, 409, "Conflict", hdrs=None, fp=None)
        body = {"ok": True, "result": [upd(42)]}
        return _FakeResp(json.dumps(body).encode())

    _ur.urlopen = fake_urlopen
    try:
        resp = telegram_get_updates({"TELEGRAM_BOT_TOKEN": "normal-token"}, offset=None, timeout_secs=0)
        check(
            "telegram_get_updates parses a stubbed 200 response",
            (resp.get("result") or [{}])[0].get("update_id") == 42,
        )
        conflict_step = None
        try:
            telegram_get_updates({"TELEGRAM_BOT_TOKEN": "conflict-token"}, offset=None, timeout_secs=0)
        except StepError as e:
            conflict_step = e.step
        check("telegram_get_updates raises telegram_conflict on stubbed HTTP 409", conflict_step == "telegram_conflict")
    finally:
        _ur.urlopen = original_urlopen

    # --- wait_for_telegram_code loop with an injected get_updates stub ---
    batches = [
        {"result": [upd(1, text="nope")]},
        {"result": [upd(2)]},
    ]
    calls = {"n": 0}

    def stub_get_updates(offset, timeout_secs):
        i = min(calls["n"], len(batches) - 1)
        calls["n"] += 1
        return batches[i]

    quiet_log = Logger(None)
    env_no_telegram: dict[str, str] = {}
    code = wait_for_telegram_code(
        env_no_telegram, quiet_log, CHAT, "ABCD", "prompt", wait_secs=5, get_updates=stub_get_updates
    )
    check("wait_for_telegram_code returns the code once the stub yields a match", code == "123456")

    def stub_get_updates_never(offset, timeout_secs):
        return {"result": []}

    code = wait_for_telegram_code(
        env_no_telegram, quiet_log, CHAT, "ABCD", "prompt", wait_secs=1, get_updates=stub_get_updates_never
    )
    check("wait_for_telegram_code times out (returns None) when nothing matches", code is None)

    global TELEGRAM_CONFLICT_RETRY_SECS
    saved_retry = TELEGRAM_CONFLICT_RETRY_SECS
    TELEGRAM_CONFLICT_RETRY_SECS = 0.0
    flaky = {"n": 0}

    def stub_get_updates_flaky(offset, timeout_secs):
        flaky["n"] += 1
        if flaky["n"] in (1, 2):
            raise StepError("telegram_conflict", "409")
        if flaky["n"] == 3:
            return {"result": []}
        return {"result": [upd(9)]}

    code = wait_for_telegram_code(
        env_no_telegram, quiet_log, CHAT, "ABCD", "prompt", wait_secs=5, get_updates=stub_get_updates_flaky
    )
    check("transient 409s are retried instead of aborting", code == "123456")
    TELEGRAM_CONFLICT_RETRY_SECS = saved_retry

    # --- autologin.env permission check ---
    with tempfile.TemporaryDirectory() as td:
        good = Path(td) / "good.env"
        good.write_text("SCHWAB_LOGIN_ID=x\nSCHWAB_PASSWORD=y\n")
        os.chmod(good, 0o600)
        ok, reason = validate_creds_file_perms(good)
        check("mode 600, owned by us -> accepted", ok)

        good400 = Path(td) / "good400.env"
        good400.write_text("SCHWAB_LOGIN_ID=x\n")
        os.chmod(good400, 0o400)
        ok, reason = validate_creds_file_perms(good400)
        check("mode 400, owned by us -> accepted", ok)

        loose = Path(td) / "loose.env"
        loose.write_text("SCHWAB_LOGIN_ID=x\n")
        os.chmod(loose, 0o644)
        ok, reason = validate_creds_file_perms(loose)
        check("mode 644 (group/other readable) -> rejected", not ok and "mode" in reason)

        missing = Path(td) / "missing.env"
        ok, reason = validate_creds_file_perms(missing)
        check("missing file -> rejected", not ok and "does not exist" in reason)

        creds, reason = load_autologin_creds(good)
        check(
            "load_autologin_creds parses key=value once perms pass",
            creds is not None and creds.get("SCHWAB_LOGIN_ID") == "x" and creds.get("SCHWAB_PASSWORD") == "y",
        )

        creds, reason = load_autologin_creds(loose)
        check("load_autologin_creds refuses a loosely-permissioned file", creds is None)

    # --- Attempt ledger / lockout ---
    with tempfile.TemporaryDirectory() as td:
        ledger_path = Path(td) / "auto-login.json"
        ledger = AttemptLedger(ledger_path, max_per_day=2, backoff_hours=6)
        t0 = now_utc()

        allowed, reason = ledger.check_lockout(now=t0)
        check("fresh ledger allows a first attempt", allowed)

        ledger.record(True, "ok", now=t0)
        allowed, reason = ledger.check_lockout(now=t0 + timedelta(minutes=1))
        check("one success does not block a second attempt", allowed)

        ledger.record(True, "ok", now=t0 + timedelta(minutes=2))
        allowed, reason = ledger.check_lockout(now=t0 + timedelta(minutes=3))
        check("daily cap (2/24h) blocks a third attempt", not allowed and "daily cap" in reason)

        allowed, reason = ledger.check_lockout(now=t0 + timedelta(hours=25))
        check("cap resets after 24h", allowed)

        ledger2_path = Path(td) / "auto-login-2.json"
        ledger2 = AttemptLedger(ledger2_path, max_per_day=2, backoff_hours=6)
        ledger2.record(False, "credentials failed", now=t0)
        allowed, reason = ledger2.check_lockout(now=t0 + timedelta(hours=1))
        check("backoff after a failure blocks the next attempt", not allowed and "backoff" in reason)
        allowed, reason = ledger2.check_lockout(now=t0 + timedelta(hours=7))
        check("backoff clears after 6h", allowed)

        allowed, reason = ledger2.check_lockout(now=t0 + timedelta(hours=1), bypass=True)
        check("--force --i-know bypasses backoff", allowed)

    # --- Decision logic: remaining-hours / unknown-and-N-days ---
    with tempfile.TemporaryDirectory() as td:
        paths = Paths(
            repo=Path(td),
            owner_dir=Path(td) / "owner",
            state_dir=Path(td) / "state",
            creds_file=Path(td) / "creds.conf",
            autologin_env_file=Path(td) / "autologin.env",
        )
        paths.owner_dir.mkdir(parents=True)
        paths.state_dir.mkdir(parents=True)

        # `schwab` not on PATH -> status unreadable -> treated as unknown -> run.
        should_run, reason = decide_should_run(paths, {"PATH": "/nonexistent"}, Logger(None))
        check("decision logic runs when auth status is unreadable (unknown)", should_run)

        write_last_success(paths, now=now_utc() - timedelta(days=1))
        should_run, reason = decide_should_run(paths, {"PATH": "/nonexistent"}, Logger(None))
        check(
            "unknown + recent success (<6d) -> no-op",
            not should_run and "unknown" in reason,
        )

        write_last_success(paths, now=now_utc() - timedelta(days=7))
        should_run, reason = decide_should_run(paths, {"PATH": "/nonexistent"}, Logger(None))
        check("unknown + stale success (>=6d) -> run", should_run)

    # --- Quiet hours ---
    def at_local(hour: int) -> datetime:
        return datetime(2026, 9, 28, hour, 30, tzinfo=AWAKE_TZ).astimezone(timezone.utc)

    check("awake window: 03:30 local deferred", not in_awake_window(at_local(3)))
    check("awake window: 09:30 local allowed", in_awake_window(at_local(9)))
    check("awake window: 22:30 local deferred", not in_awake_window(at_local(22)))

    if failures:
        print(f"\n{failures} self-test check(s) FAILED", file=sys.stderr)
        return 1
    print("\nall self-test checks passed")
    return 0


# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------


def parse_args(argv: list[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--dry-run", action="store_true", help="load the login page, report selectors, stop")
    p.add_argument("--force", action="store_true", help="skip the should-run decision (lockout still applies)")
    p.add_argument("--i-know", action="store_true", help="with --force, also bypass the daily cap/backoff")
    p.add_argument(
        "--self-test",
        action="store_true",
        help="run offline unit checks (Telegram filter, perms, lockout, decision logic) and exit",
    )
    p.add_argument("--repo", default=str(DEFAULT_REPO_DIR), help="repo root (for jarvis-token-keeper.sh)")
    return p.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)

    if args.self_test:
        return self_test()

    paths = Paths(
        repo=Path(args.repo).expanduser(),
        owner_dir=DEFAULT_OWNER_DIR,
        state_dir=DEFAULT_STATE_DIR,
        creds_file=DEFAULT_CREDS_FILE,
        autologin_env_file=DEFAULT_AUTOLOGIN_ENV_FILE,
    )
    log = Logger(paths.log_file)
    browser_env = build_run_env(paths)  # app key + Telegram creds; NEVER login id/password

    missing_key = missing_app_key(browser_env)
    if missing_key:
        print(
            f"schwab-auto-login: missing required env var: {missing_key} "
            f"(checked env and {paths.creds_file}); refusing to run.",
            file=sys.stderr,
        )
        return 4

    creds: dict = {}
    if not args.dry_run:
        loaded, reason = load_autologin_creds(paths.autologin_env_file)
        if loaded is None:
            print(f"schwab-auto-login: {reason}; refusing to run.", file=sys.stderr)
            return 4
        creds = loaded
        missing_creds = missing_login_creds(creds)
        if missing_creds:
            print(
                "schwab-auto-login: missing required var(s) in "
                f"{paths.autologin_env_file}: {', '.join(missing_creds)}; refusing to run.",
                file=sys.stderr,
            )
            return 4
        if twofa_method(creds) is None:
            print(
                f"schwab-auto-login: SCHWAB_2FA_METHOD must be one of {', '.join(TWOFA_METHODS)}; "
                "refusing to run.",
                file=sys.stderr,
            )
            return 4

    if not args.dry_run and not args.force:
        should_run, reason = decide_should_run(paths, browser_env, log)
        log.line(f"decision should_run={should_run} reason={reason}")
        if not should_run:
            print(f"schwab-auto-login: no-op ({reason})")
            return 0
        if not in_awake_window():
            log.line(f"deferred: outside awake window {AWAKE_START_HOUR}:00-{AWAKE_END_HOUR}:00 {AWAKE_TZ.key}")
            print("schwab-auto-login: deferred (quiet hours)")
            return 0
    elif args.force:
        log.line("decision skipped (--force)")

    prune_old_artifacts(paths)

    ledger = AttemptLedger(paths.ledger_file, max_per_day=MAX_ATTEMPTS_PER_DAY, backoff_hours=BACKOFF_HOURS)
    if not args.dry_run:
        bypass = args.force and args.i_know
        allowed, why = ledger.check_lockout(bypass=bypass)
        if not allowed:
            log.line(f"blocked: {why}")
            print(f"schwab-auto-login: blocked by lockout guard: {why}", file=sys.stderr)
            return 2

    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        print(
            "schwab-auto-login: playwright is not importable; run scripts/schwab-auto-login.sh --setup",
            file=sys.stderr,
        )
        return 4

    host, port = redirect_host_port(browser_env)
    redirect_pattern = re.compile(rf"^https?://{re.escape(host)}:{port}(/.*)?$")
    captured: dict[str, str] = {}

    def route_handler(route):
        req_url = route.request.url
        if "code=" in req_url and "url" not in captured:
            captured["url"] = req_url
        try:
            route.fulfill(
                status=200,
                content_type="text/plain",
                body="Schwab OAuth captured by schwab-auto-login.",
            )
        except Exception:
            try:
                route.abort()
            except Exception:
                pass

    chat_id_str = browser_env.get("TELEGRAM_CHAT_ID")
    chat_id = int(chat_id_str) if chat_id_str and chat_id_str.lstrip("-").isdigit() else None

    exit_code = 0
    page = None
    with sync_playwright() as pw:
        browser = pw.chromium.launch(headless=False, args=LAUNCH_ARGS)
        context = browser.new_context(
            ignore_https_errors=True, user_agent=DESKTOP_USER_AGENT, viewport=VIEWPORT
        )
        context.route(redirect_pattern, route_handler)
        page = context.new_page()

        try:
            if args.dry_run:
                exit_code = dry_run_flow(page, browser_env, log)
            else:
                redirect_url = run_login_flow(page, browser_env, creds, captured, log, chat_id)

                lock_fd = acquire_lock(paths.owner_dir / ".token-keeper.lock", log)
                try:
                    run_schwab_auth_login(redirect_url, browser_env, log)
                finally:
                    release_lock(lock_fd)

                run_token_keeper(paths, browser_env, log)
                data = verify_authenticated(browser_env)

                ledger.record(True, "login ok")
                write_last_success(paths)
                valid_until = compute_refresh_valid_until(data)
                notify_telegram(
                    browser_env, f"Schwab auto-login OK — refresh valid until {valid_until}", log
                )
                log.line(f"success valid_until={valid_until}")
                exit_code = 0
        except StepError as e:
            if page is not None:
                out_dir = save_failure_artifacts(page, paths, e.step, log)
                log.line(f"failure artifacts: {out_dir}")
            submitted = e.step not in PRE_SUBMIT_STEPS
            if submitted:
                ledger.record(False, f"{e.step}: {e.message}"[:200])
            else:
                log.line(f"not counted toward lockout budget (pre-submit step {e.step})")
            notify_telegram(
                browser_env,
                f"Schwab auto-login FAILED at step {e.step}: {e.message[:180]}; "
                "run scripts/jarvis-auth-login.sh from the Mac",
                log,
            )
            log.line(f"failure step={e.step} message={e.message} submitted={submitted}")
            exit_code = 3 if e.step == "verify" else 1
        except Exception as e:  # unexpected — still fail closed
            if page is not None:
                out_dir = save_failure_artifacts(page, paths, "unexpected", log)
                log.line(f"failure artifacts: {out_dir}")
            ledger.record(False, f"unexpected: {e}"[:200])
            notify_telegram(
                browser_env,
                f"Schwab auto-login FAILED (unexpected error): {type(e).__name__}; "
                "run scripts/jarvis-auth-login.sh from the Mac",
                log,
            )
            log.line(f"unexpected failure: {type(e).__name__}: {e}")
            exit_code = 1
        finally:
            context.close()
            browser.close()

    return exit_code


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
