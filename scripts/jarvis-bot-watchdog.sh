#!/usr/bin/env bash
# jarvis-bot-watchdog.sh — restart a paper agent that has silently stopped ticking.
#
# Why this exists (2026-09-18 post-mortem):
#   Both jarvis agents can degrade *internally* (auth_fatal) without exiting, so
#   systemd's Restart=on-failure never fires and nothing alerts. Swing 9947 was
#   blind for 4h15m of the regular session with 4 open positions unmanaged;
#   options 8709 was blind ~20h. The agents log to files and keep a `last_tick`
#   in their state JSON — that is the freshness signal this watchdog reads.
#
# Behaviour per unit:
#   - recovered (last_tick fresh)          -> reset restart counter, say nothing
#   - stale for the CURRENT session        -> `systemctl --user restart <unit>`
#                                             + one Telegram alert. Thresholds are
#                                             session-aware (see UNITS below): the
#                                             swing agent's after-hours sleep is
#                                             1800s by design, so a fixed constant
#                                             restarted it while it was healthy.
#   - stale again after a restart          -> escalate (alert, no restart) so a
#                                             dead refresh token surfaces as
#                                             "needs interactive login" instead
#                                             of a restart loop.
#
#   --dry-run   print verdicts only; no restarts, no alerts, no state writes.
#
# Run by systemd (schwab-bot-watchdog.timer, every 5 min) or by hand.
# Env overrides: SCHWAB_STALE_SECS, SCHWAB_RESTART_GRACE_SECS, SCHWAB_REPO.

set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

REPO="${SCHWAB_REPO:-$HOME/projects/schwabinvestbot}"
STALE_SECS_FLOOR="${SCHWAB_STALE_SECS:-900}"   # regular-session threshold + floor
RESTART_GRACE_SECS="${SCHWAB_RESTART_GRACE_SECS:-600}"
ESCALATE_COOLDOWN_SECS="${SCHWAB_ESCALATE_COOLDOWN_SECS:-3600}"
STATE_DIR="${SCHWAB_STATE_DIR:-$HOME/.local/state/schwab-paper}"
CREDS_FILE="${SCHWAB_CREDS_FILE:-$HOME/.config/environment.d/schwab-paper.conf}"
STATE_FILE="$STATE_DIR/watchdog-state.json"
LOG="$STATE_DIR/watchdog.log"

# unit|state-json|log-file|stale-s-regular|stale-s-offhours
#
# Staleness is session-aware because the agents' sleep comes from their rules
# schedule (crates/{schwab-cli,schwab-trader}/src/agent/schedule.rs):
#   swing (schwab-trader): regular = tick_interval_seconds            = 90s
#                          premarket = premarket_tick_interval_seconds = 300s
#                          idle/closed = overnight.tick_interval_seconds.max(1800)
#                                      = 1800s -> 30 MINUTES of healthy silence
#   options (schwab-cli):  regular AND idle = tick_interval_seconds    = 120s
# A single constant is therefore wrong: it restarted the healthy swing agent
# three times after hours. Thresholds are ~4x the expected sleep.
# If the `schedule:` block of rules/*.yaml changes, change these numbers with it.
UNITS=(
  "schwab-options-8709.service|rules/agent-sim-state-options-pilot-8709.json|rules/agent-options-pilot-8709.log|900|900"
  "schwab-swing-9947.service|rules/trader-state-trader-swing-9947.json|rules/trader-trader-swing-9947.log|900|3600"
)

DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "unknown arg: $arg" >&2; exit 64 ;;
  esac
done

mkdir -p "$STATE_DIR"
log() { printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >>"$LOG"; }

if [[ -z "${TELEGRAM_BOT_TOKEN:-}" || -z "${TELEGRAM_CHAT_ID:-}" ]]; then
  # shellcheck disable=SC1090
  [[ -r "$CREDS_FILE" ]] && { set -a; . "$CREDS_FILE"; set +a; }
fi

alert() { # message
  local msg="$1"
  if (( DRY_RUN )); then
    echo "   [dry-run] would alert: $msg"
    return 0
  fi
  log "ALERT $msg"
  if [[ -n "${TELEGRAM_BOT_TOKEN:-}" && -n "${TELEGRAM_CHAT_ID:-}" ]]; then
    curl -sS -m 15 -X POST \
      "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/sendMessage" \
      -d "chat_id=${TELEGRAM_CHAT_ID}" \
      --data-urlencode "text=${msg}" >/dev/null || log "WARN telegram alert failed"
  fi
}

if [[ ! -r "$STATE_FILE" ]]; then echo '{}' >"$STATE_FILE"; fi
if (( DRY_RUN )); then
  prev_state="$(cat "$STATE_FILE")"
else
  prev_state="$(cat "$STATE_FILE")"
fi
new_state="$prev_state"
st="$STATE_DIR/nudge.$$"

for entry in "${UNITS[@]}"; do
  IFS='|' read -r unit state_rel log_rel stale_regular stale_offhours <<<"$entry"
  state_path="$REPO/$state_rel"
  log_path="$REPO/$log_rel"

  age=-1
  session=""
  if [[ -r "$state_path" ]]; then
    read -r age session <<<"$(python3 - "$state_path" <<'PY'
import json, sys, datetime
try:
    d = json.load(open(sys.argv[1]))
    lt = d.get("last_tick")
    if not lt:
        print("-1"); raise SystemExit
    t = datetime.datetime.fromisoformat(lt.replace("Z", "+00:00"))
    age = int((datetime.datetime.now(datetime.timezone.utc) - t).total_seconds())
    print(age, d.get("last_session") or "")
except Exception:
    print("-1 -")
PY
)"
  fi

  # Session-aware threshold: after hours the swing agent sleeps 30 min by design.
  case "$session" in
    regular|premarket) threshold="$stale_regular" ;;
    *)                 threshold="$stale_offhours" ;;
  esac
  if (( threshold < STALE_SECS_FLOOR )); then threshold="$STALE_SECS_FLOOR"; fi

  active="$(systemctl --user is-active "$unit" 2>/dev/null || true)"

  # Evidence from the agent log, for the alert body. Count failure markers only
  # AFTER the last successful tick line: the log keeps historical failures
  # forever, and counting those made the alert body lie about the current state.
  evidence="none"
  if [[ -r "$log_path" ]]; then
    since_tick="$(awk '/^tick=/{buf=""} {buf=buf $0 "\n"} END{printf "%s", buf}' "$log_path" 2>/dev/null | tail -n 60 || true)"
    tail_txt="$since_tick"
    n_auth="$(grep -ci 'auth_fatal\|invalid_grant\|login required\|degraded' <<<"$tail_txt" || true)"
    last_line="$(grep -v '^$' <<<"$tail_txt" | tail -n 1 | cut -c1-160)"
    evidence="fail_markers_since_last_tick=$n_auth last_log_line=$last_line"
  fi

  verdict="ok"
  if [[ "$active" != "active" ]]; then
    verdict="unit_inactive"
  elif (( age < 0 )); then
    verdict="no_last_tick"
  elif (( age > threshold )); then
    verdict="stale"
  fi

  # Per-unit bookkeeping.
  read -r last_restart_at restarts last_alert_at <<<"$(jq -r --arg u "$unit" \
    '.[$u] // {} | "\(.last_restart_at // 0) \(.restarts // 0) \(.last_alert_at // 0)"' \
    <<<"$prev_state")"
  now="$(date +%s)"
  prior_restarts="$restarts"

  action="none"
  case "$verdict" in
    ok)
      if (( prior_restarts > 0 )); then
        action="recovered"
        alert "✅ Schwab paper agent RECOVERED: $unit (tick fresh again after $prior_restarts restart(s); last_tick age ${age}s)"
      fi
      last_restart_at=0; restarts=0
      ;;
    stale|unit_inactive|no_last_tick)
      if (( restarts >= 2 )); then
        if (( now - last_alert_at > ESCALATE_COOLDOWN_SECS )); then
          action="escalate"
          last_alert_at=$now
          alert "🛑 Schwab paper agent NEEDS A HUMAN: $unit not ticking (age ${age}s, unit=$active) after $restarts restart(s). ${evidence}. Likely a dead refresh token — run scripts/jarvis-auth-login.sh from the Mac."
        else
          action="escalate_suppressed"
        fi
      elif (( now - last_restart_at < RESTART_GRACE_SECS )); then
        action="grace"
      else
        action="restart"
        restarts=$((restarts + 1))
        last_restart_at=$now
        alert "⚠️ Schwab paper agent NOT TICKING: $unit (last_tick age ${age}s, unit=$active, restart #${restarts}). ${evidence}. Refreshing the token mirror, then restarting the unit."
        if (( ! DRY_RUN )); then
          # A stale mirror is the usual reason an agent stops: top it up first,
          # because restarting against a stale mirror just repeats the failure.
          "$REPO/scripts/jarvis-token-keeper.sh" >/dev/null 2>&1 || log "WARN token keeper failed before restarting $unit"
          systemctl --user restart "$unit" || log "WARN restart failed for $unit"
        fi
      fi
      ;;
  esac

  # Reset the auth-failure escalation counter when ticks are fresh.
  if [[ "$verdict" == "ok" ]]; then
    restarts=0
  fi

  new_state="$(jq -c --arg u "$unit" \
    --argjson r "$restarts" --argjson lr "$last_restart_at" --argjson la "$last_alert_at" \
    '.[$u] = {restarts:$r, last_restart_at:$lr, last_alert_at:$la}' <<<"$new_state")"

  printf 'VERDICT unit=%s session=%s active=%s last_tick_age_s=%s threshold_s=%s verdict=%s action=%s restarts=%s\n' \
    "$unit" "${session:--}" "$active" "$age" "$threshold" "$verdict" "$action" "$prior_restarts"
done

if (( DRY_RUN )); then
  echo "[dry-run] state would be: $new_state"
else
  printf '%s\n' "$new_state" >"$st"
  mv -f "$st" "$STATE_FILE"
fi
