#!/usr/bin/env bash
# Regenerate embedded trade-event MP3s via elabs (ElevenLabs CLI).
set -euo pipefail

VOICE_ID="${TRADE_AUDIO_VOICE_ID:-nHpjrnfJ7ggpW7rVHqsY}"
OUT_DIR="$(cd "$(dirname "$0")/.." && pwd)/crates/schwab-cli/assets/trade-audio"

mkdir -p "$OUT_DIR"

pairs=(
  "entry_opened|New position opened."
  "entry_working|Limit order working."
  "entry_deferred|New position deferred."
  "entry_rejected|Order rejected."
  "entry_cancelled|Entry order cancelled."
  "exit_profit|Position closed. Profit taken."
  "exit_stop|Position closed. Stop loss hit."
  "exit_time|Position closed. Time stop."
  "exit_eod|End of day flatten."
  "exit_overnight|Overnight flatten."
  "exit_dte|Approaching expiration. Position closed."
  "exit_advisor|Advisor exit. Position closed."
  "exit_bracket|Position closed on bracket."
  "alert_halted|Trading halted."
  "alert_risk|Risk alert."
)

count=0
for pair in "${pairs[@]}"; do
  name="${pair%%|*}"
  text="${pair#*|}"
  dest="$OUT_DIR/${name}.mp3"
  echo "→ $name: $text"
  elabs tts speak --voice "$VOICE_ID" --text "$text" -o "$dest" --json >/dev/null
  count=$((count + 1))
done

echo "Wrote $count clips to $OUT_DIR (voice $VOICE_ID)"
