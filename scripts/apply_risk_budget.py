#!/usr/bin/env python3
"""Apply the validated risk-budget change to a live rules file.

Validated on jarvis (trader-swing-9947.yaml geometry, both windows):
  pre  2024-06-30..2026-06-28 : P&L 131 -> 417 USD (3.2x), DD 3.51 -> 9.82 %
  live 2026-06-29..2026-09-17 : P&L  72 -> 239 USD (3.3x), DD 0.70 -> 2.61 %
Win rate unchanged in both windows -> pure position-sizing effect, no change in
signal quality. Drawdown stays under the 10 % halt.

Why more than the base number is touched: the live file routes sizing through regime
profiles (adaptation.enabled + regime_auto_select), so changing only the base would let
0.4-0.9 % profile overrides keep winning. The profiles are scaled x4 to preserve the
risk-appetite RATIOS between regimes, and the LLM clamp ceiling is raised so the
adaptive layer cannot silently undo the change. The clamp minimum stays at 0.3 so the
LLM can still de-risk.

Usage:
  python3 scripts/apply_risk_budget.py                 # apply in place, with backup
  python3 scripts/apply_risk_budget.py --check         # dry run: report, change nothing
"""
import re
import shutil
import sys
import time
import yaml

TARGET = "rules/trader-swing-9947.yaml"
CHECK = "--check" in sys.argv

REPS = [
    # 1. base risk budget (the approved change)
    (r"(?m)^      risk_per_trade_pct: 0\.75$", "      risk_per_trade_pct: 3.0"),
    (r"(?m)^      max_position_pct: 18\.0$", "      max_position_pct: 50.0"),
    (r"(?m)^  max_portfolio_heat_pct: 8\.0$", "  max_portfolio_heat_pct: 12.0"),
    # 2. regime profile overrides scaled x4 (0.55/0.9/0.4 -> 2.2/3.6/1.6)
    (r"(?m)^            risk_per_trade_pct: 0\.55$", "            risk_per_trade_pct: 2.2"),
    (r"(?m)^            risk_per_trade_pct: 0\.9$", "            risk_per_trade_pct: 3.6"),
    (r"(?m)^            risk_per_trade_pct: 0\.4$", "            risk_per_trade_pct: 1.6"),
    # 3. raise the LLM clamp ceiling (min left at 0.3 so de-risking stays possible)
    (r"(?m)^      max: 1\.2$", "      max: 4.8"),
    (r"(?m)^      max_delta_per_change: 0\.15$", "      max_delta_per_change: 0.6"),
]


def profile_risk(prof):
    """None-safe: profiles may carry overrides=None (e.g. baseline)."""
    ov = prof.get("overrides") or {}
    return ((ov.get("entry") or {}).get("position_size") or {}).get("risk_per_trade_pct")


def report(label, text):
    d = yaml.safe_load(text)
    ps = d["playbook"]["entry"]["position_size"]
    print(f"  {label}: risk/trade={ps['risk_per_trade_pct']} max_position_pct={ps['max_position_pct']} "
          f"heat={d['risk']['max_portfolio_heat_pct']} halt={d['risk']['max_drawdown_halt_pct']}")
    print(f"      profiles={ {k: profile_risk(v) for k, v in d['adaptation']['profiles'].items()} }")
    print(f"      llm clamp={d['llm']['adaptation_bounds']['risk_per_trade_pct']}")


original = open(TARGET).read()
print(f"target: {TARGET}")
report("before", original)

s = original
for pat, new in REPS:
    s, n = re.subn(pat, new, s)
    if n != 1:
        sys.exit(f"ABORT: pattern {pat!r} matched {n} times (expected 1) - file not modified")

report("after ", s)

if CHECK:
    print("--check: nothing written")
    sys.exit(0)

bak = f"{TARGET}.bak-{time.strftime('%Y%m%d-%H%M%S')}-riskbudget"
shutil.copy(TARGET, bak)
open(TARGET, "w").write(s)
yaml.safe_load(open(TARGET))          # must still parse
print(f"applied. backup: {bak}")
