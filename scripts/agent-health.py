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
    "options-8709": ["open_positions", "llm_review_count", "last_regime"],
    "swing-9947": ["closed_trades_since_learn", "open_positions", "active_profile",
                   "active_profile_source", "last_regime"],
}

SWING_STARTING_CASH_USD = 4000.0


def enrich_metrics(name, state, extra):
    """Add PnL / deployment fields; agents do not share a schema."""
    if name == "options-8709":
        sim = state.get("sim") or {}
        if isinstance(sim, dict) and "realized_pnl_usd" in sim:
            extra["realized_pnl_usd"] = sim["realized_pnl_usd"]
        unreal = 0.0
        for pos in (state.get("open_positions") or {}).values():
            credit = pos.get("entry_credit")
            debit = pos.get("last_good_debit_to_close")
            contracts = pos.get("contracts") or 0
            if credit is not None and debit is not None:
                unreal += (credit - debit) * 100.0 * contracts
        extra["unrealized_est_usd"] = round(unreal, 2)
        return
    if name == "swing-9947":
        sim = state.get("sim")
        if isinstance(sim, dict) and "realized_pnl_usd" in sim:
            extra["realized_pnl_usd"] = sim["realized_pnl_usd"]
        deployed = 0.0
        for pos in (state.get("open_positions") or {}).values():
            deployed += float(pos.get("market_value_usd") or 0.0)
        extra["deployed_usd"] = round(deployed, 2)
        extra["deployed_pct"] = round(100.0 * deployed / SWING_STARTING_CASH_USD, 1)


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
        enrich_metrics(name, d, extra)
        print(f"{name:14} unit={active:8} last_tick={lt} "
              f"age={age:.1f}m{'  <-- STALE' if flagged else ''} {extra}")
        if flagged:
            stale.append(name)
    print("\nSTALE: " + (", ".join(stale) if stale else "none"))
    return 1 if stale else 0


if __name__ == "__main__":
    sys.exit(main())
