#!/usr/bin/env python3
"""Health check for the jarvis paper agents.

`systemctl is-active` is NOT health: a unit can be "active" while its loop is wedged.
The real signal is `last_tick` inside the agent's state file — but the two agents use
DIFFERENT state-file names, which is easy to get wrong:

  options (schwab)         -> rules/agent-sim-state-options-pilot-8709.json
  swing   (schwab-trader)  -> rules/trader-state-trader-swing-9947.json   <- not agent-sim-*

Run on jarvis with the paper env loaded:
  set -a; . $HOME/.config/environment.d/schwab-paper.conf; set +a
"""
import json
import pathlib
import subprocess
import sys

BASE = pathlib.Path.home() / "projects/schwabinvestbot"
AGENTS = {
    "options-8709": (BASE / "rules/agent-sim-state-options-pilot-8709.json",
                     "schwab-options-8709.service"),
    "swing-9947": (BASE / "rules/trader-state-trader-swing-9947.json",
                   "schwab-swing-9947.service"),
}


# Per-agent fields worth printing: the agents do NOT share a schema, and there is no
# `closed_trades` in either live state file (that field only exists in backtest output).
EXTRAS = {
    "options-8709": ["cumulative_realized_pnl_usd", "open_positions",
                     "llm_review_count", "last_regime"],
    "swing-9947": ["closed_trades_since_learn", "open_positions", "active_profile",
                   "active_profile_source", "last_regime"],
}


def iso_age(ts):
    """Age in minutes of an RFC3339 timestamp, without importing dateutil."""
    import datetime as dt
    try:
        t = dt.datetime.fromisoformat(ts.replace("Z", "+00:00"))
        return (dt.datetime.now(dt.UTC) - t).total_seconds() / 60.0
    except Exception:
        return None


def main():
    stale = []
    for name, (path, unit) in AGENTS.items():
        active = subprocess.run(["systemctl", "--user", "is-active", unit],
                                capture_output=True, text=True).stdout.strip()
        if not path.exists():
            print(f"{name:14} unit={active:8} STATE FILE MISSING: {path.name}")
            stale.append(name)
            continue
        d = json.loads(path.read_text())
        lt = d.get("last_tick")
        age = iso_age(lt) if lt else None
        flagged = age is None or age > 15          # a market-hours tick is minutes apart
        extra = {k: d[k] for k in EXTRAS.get(name, []) if k in d}
        open_n = len(extra.get("open_positions") or []) if "open_positions" in extra else None
        if open_n is not None:
            extra["open_n"] = open_n
            extra.pop("open_positions", None)
        print(f"{name:14} unit={active:8} last_tick={lt} "
              f"age={age:.1f}m{'  <-- STALE' if flagged else ''} {extra}")
        if flagged:
            stale.append(name)
    print("\nSTALE: " + (", ".join(stale) if stale else "none"))
    return 1 if stale else 0


if __name__ == "__main__":
    sys.exit(main())
