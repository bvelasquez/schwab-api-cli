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
import yaml

SRC = "/tmp/jarvis-swing-9947.yaml"          # pulled from jarvis
CACHE_SRC = "rules/.backtest-cache-trader-swing-p3-ctl-9947.json"

# the deployed jarvis file is the control (verbatim)
shutil.copy(SRC, "rules/trader-swing-j-ctl-9947.yaml")

base = open(SRC).read()
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

for arm in ("j-ctl", "j-r3"):
    shutil.copy(CACHE_SRC, f"rules/.backtest-cache-trader-swing-{arm}-9947.json")
    yaml.safe_load(open(f"rules/trader-swing-{arm}-9947.yaml"))
print("arms ready: j-ctl (control), j-r3 (sizing scaled)")
