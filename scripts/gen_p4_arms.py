#!/usr/bin/env python3
"""Generate risk-budget arms (p4) on the best (control) exit geometry.

Finding that motivates this: one entry carried risk 0.75% of the $4,000 sleeve, and the
implied stop distance (~7% on high-ATR names) made the notional ~$426 = 10.6% of the sleeve.
With max_positions=4 that is <=3% portfolio heat against a DECLARED 8% ceiling -- the bot
declares a risk budget it never spends, so absolute P&L is limited by deployment, not signals.

Each arm scales the risk budget linearly and lifts the caps that would otherwise saturate
(max_position_pct caps a single name; max_portfolio_heat_pct caps total concurrent heat).
"""
import re
import shutil

SRC = "rules/trader-swing-p3-ctl-9947.yaml"
CACHE = "rules/.backtest-cache-trader-swing-p3-ctl-9947.json"

# arm -> (risk_per_trade_pct, max_positions, max_position_pct, max_portfolio_heat_pct)
ARMS = {
    "p4-h8-r2":  (2.0, 4, 40.0, 8.0),    # spend the FULL declared 8% heat (2% x 4 slots)
    "p4-h16-r2": (2.0, 8, 40.0, 16.0),   # 2x the declared budget, double the slots
    "p4-h16-r4": (4.0, 4, 60.0, 16.0),   # 4x per-trade size, same concurrency
    "p4-h32-r4": (4.0, 8, 60.0, 32.0),   # 4x size AND 8 slots (aggressive upper bound)
    "p4-h24-r6": (6.0, 4, 100.0, 24.0),  # push past the max_position_pct cap
    "p4-h32-r8": (8.0, 4, 100.0, 32.0),  # upper end of the frontier
}

base = open(SRC).read()
for name, (risk, maxpos, maxpos_pct, heat) in ARMS.items():
    s = base
    reps = [
        (r"(?m)^      risk_per_trade_pct: 0\.75$", f"      risk_per_trade_pct: {risk}"),
        (r"(?m)^    max_positions: 4$", f"    max_positions: {maxpos}"),
        (r"(?m)^      max_position_pct: 18\.0$", f"      max_position_pct: {maxpos_pct}"),
        (r"(?m)^  max_portfolio_heat_pct: 8\.0$", f"  max_portfolio_heat_pct: {heat}"),
    ]
    for pat, new in reps:
        s, n = re.subn(pat, new, s)
        assert n == 1, f"{name}: pattern {pat!r} matched {n} times"
    out = f"rules/trader-swing-{name}-9947.yaml"
    open(out, "w").write(s)
    shutil.copy(CACHE, f"rules/.backtest-cache-trader-swing-{name}-9947.json")

    # verify via the parser: what the engine will actually read
    import yaml
    d = yaml.safe_load(s)
    ps = d["playbook"]["entry"]["position_size"]
    print(f"{name:12} risk/trade={ps['risk_per_trade_pct']}% max_pos={d['playbook']['entry']['max_positions']} "
          f"max_pos_pct={ps['max_position_pct']} heat_ceiling={d['risk']['max_portfolio_heat_pct']}% "
          f"-> implied max heat={ps['risk_per_trade_pct']*d['playbook']['entry']['max_positions']:.1f}% of sleeve")
print("generated", len(ARMS), "arms")
