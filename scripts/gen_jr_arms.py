#!/usr/bin/env python3
"""Generate arms on the ACTUAL jarvis paper config (trader-swing-9947.yaml as deployed).

Why: the p4 risk-budget arms were built on the LOCAL rules file, which is stale -- jarvis
runs later Barry-approved exit tuning (profit_target_pct 7.5, trail activate 3.0). The
deployed change must be measured on the deployed geometry, not the local one.

  j-ctl : jarvis config, unchanged (control)
  j-r3  : jarvis config + risk budget scaled (base 3.0%, caps lifted, regime profiles
          scaled x4, LLM clamp raised so the adaptive layer cannot undo it)
"""
import re
import shutil
import sys
from pathlib import Path

import yaml

# The config to measure. Pass the authoritative file explicitly (on jarvis:
#   gen_jr_arms.py rules/trader-swing-9947.yaml
# ). The default is only safe when run on the host that owns the file -- never point this
# at a hand-pulled /tmp copy, which is exactly how the earlier stale-geometry arms arose.
SRC = sys.argv[1] if len(sys.argv) > 1 else "rules/trader-swing-9947.yaml"
if not Path(SRC).is_file():
    sys.exit(f"rules file not found: {SRC}")

CACHE_SRC = sys.argv[2] if len(sys.argv) > 2 else "rules/.backtest-cache-trader-swing-9947.json"
if not Path(CACHE_SRC).is_file():
    cands = sorted(Path("rules").glob(".backtest-cache-trader-swing-*.json"))
    if not cands:
        sys.exit(f"no backtest cache: {CACHE_SRC} missing and no rules/.backtest-cache-trader-swing-*.json")
    CACHE_SRC = str(cands[0])

print(f"source rules: {SRC} | cache: {CACHE_SRC}")

base = open(SRC).read()
if "risk_per_trade_pct: 3.0" in base and "max_position_pct: 50.0" in base:
    sys.exit(
        f"{SRC} already carries the scaled risk budget (3.0 / 50.0).\n"
        "This script transforms the PRE-change file into the changed one -- point it at the\n"
        "pre-change copy (e.g. rules/trader-swing-9947.yaml.bak-<stamp>-riskbudget)."
    )
s = base
reps = [
    # 1. base risk budget (approved change)
    (r"(?m)^      risk_per_trade_pct: 0\.75$", "      risk_per_trade_pct: 3.0"),
    (r"(?m)^      max_position_pct: 18\.0$", "      max_position_pct: 50.0"),
    (r"(?m)^  max_portfolio_heat_pct: 8\.0$", "  max_portfolio_heat_pct: 12.0"),
    # 2. regime profile overrides scaled x4, so no regime silently negates the change
    (r"(?m)^            risk_per_trade_pct: 0\.55$", "            risk_per_trade_pct: 2.2"),
    (r"(?m)^            risk_per_trade_pct: 0\.9$", "            risk_per_trade_pct: 3.6"),
    (r"(?m)^            risk_per_trade_pct: 0\.4$", "            risk_per_trade_pct: 1.6"),
    # 3. raise the LLM clamp ceiling (min left at 0.3 so de-risking stays possible)
    (r"(?m)^      max: 1\.2$", "      max: 4.8"),
    (r"(?m)^      max_delta_per_change: 0\.15$", "      max_delta_per_change: 0.6"),
]
for pat, new in reps:
    s, n = re.subn(pat, new, s)
    assert n == 1, f"pattern {pat!r} matched {n} times"
out = "rules/trader-swing-j-r3-9947.yaml"
open(out, "w").write(s)


def profile_risk(prof):
    """None-safe read: profiles may carry overrides=None (e.g. baseline)."""
    ov = prof.get("overrides") or {}
    entry = ov.get("entry") or {}
    ps = entry.get("position_size") or {}
    return ps.get("risk_per_trade_pct")


d = yaml.safe_load(s)
ps = d["playbook"]["entry"]["position_size"]
print("j-r3 -> base risk/trade =", ps["risk_per_trade_pct"], "| max_position_pct =", ps["max_position_pct"],
      "| heat ceiling =", d["risk"]["max_portfolio_heat_pct"])
print("      profiles:", {k: profile_risk(v) for k, v in d["adaptation"]["profiles"].items()})
print("      llm clamp:", d["llm"]["adaptation_bounds"]["risk_per_trade_pct"])

# Write the control verbatim only now, after the transforms above have validated: a failed
# transform must never leave a mislabelled control behind (it did once -- a grid run against it
# would have silently compared change vs change).
shutil.copy(SRC, "rules/trader-swing-j-ctl-9947.yaml")

for arm in ("j-ctl", "j-r3"):
    shutil.copy(CACHE_SRC, f"rules/.backtest-cache-trader-swing-{arm}-9947.json")
    yaml.safe_load(open(f"rules/trader-swing-{arm}-9947.yaml"))
print("arms ready: j-ctl (control), j-r3 (sizing scaled)")
