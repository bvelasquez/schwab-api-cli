#!/usr/bin/env python3
"""Generate exit-geometry arms (p3) from the frozen deterministic baseline.

Hypothesis: the exit geometry sits exactly on the reward-risk gate floor
(target=min(8.5%, 2.5*ATR), stop=min(5.5%, 2.0*ATR) -> R:R 1.25 for a 2%-ATR name),
which caps BOTH payoff per trade and admission. Raise the target ATR multiple so
R:R rises, and separately widen the stop, keeping R:R >= 1.25 in every arm.
The pct/horizon caps are raised so they cannot pre-empt the ATR caps being tested.
"""
import json
import shutil
import os

SRC = "rules/trader-swing-p2-base-9947.yaml"
CACHE = "rules/.backtest-cache-trader-swing-9947.json"

# arm -> (profit_target_pct, pt_atr_multiple, horizon_sqrt_days, stop_loss_pct, stop_atr_multiple)
ARMS = {
    "p3-ctl":  (8.5, 2.5, 1.0, 5.5, 2.0),   # control = current live geometry (R:R 1.25)
    "p3-tp35": (14.0, 3.5, 1.5, 8.0, 2.0),  # raise target only            (R:R 1.75)
    "p3-tp45": (14.0, 4.5, 1.5, 8.0, 2.0),  # raise target harder          (R:R 2.25)
    "p3-tp35s25": (14.0, 3.5, 1.5, 8.0, 2.5),  # raise target + wider stop  (R:R 1.40)
}

# exact anchors in playbook.exit (4-space indent, unique because deeper profile blocks are 10-space)
ANCHORS = {
    "profit_target_pct": "\n    profit_target_pct: 8.5\n",
    "pt_atr": "\n    profit_target_atr_cap:\n      enabled: true\n      atr_multiple: 2.5\n",
    "horizon": "\n    profit_target_horizon_cap:\n      enabled: true\n      sqrt_days_multiple: 1.0\n",
    "stop_pct": "\n    stop_loss_pct: 5.5\n",
    "stop_atr": "\n    stop_loss_atr_cap:\n      enabled: true\n      atr_multiple: 2.0\n",
}

base = open(SRC).read()
for name, (pt, pt_atr, hor, sl, sl_atr) in ARMS.items():
    s = base
    reps = {
        "profit_target_pct": f"\n    profit_target_pct: {pt}\n",
        "pt_atr": f"\n    profit_target_atr_cap:\n      enabled: true\n      atr_multiple: {pt_atr}\n",
        "horizon": f"\n    profit_target_horizon_cap:\n      enabled: true\n      sqrt_days_multiple: {hor}\n",
        "stop_pct": f"\n    stop_loss_pct: {sl}\n",
        "stop_atr": f"\n    stop_loss_atr_cap:\n      enabled: true\n      atr_multiple: {sl_atr}\n",
    }
    for k, new in reps.items():
        old = ANCHORS[k]
        assert s.count(old) == 1, f"{name}: anchor {k} not unique ({s.count(old)})"
        s = s.replace(old, new)
    s = s.replace("    require_above_sma:\n    - 20\n    - 50\n",
                  "    require_above_sma:\n    - 20\n    - 50\n"
                  f"    # p3 ARM {name}: target={pt_atr}xATR stop={sl_atr}xATR pt_pct={pt} stop_pct={sl} horizon={hor}\n", 1)
    out_yaml = f"rules/trader-swing-{name}-9947.yaml"
    open(out_yaml, "w").write(s)
    shutil.copy(CACHE, f"rules/.backtest-cache-trader-swing-{name}-9947.json")
    # verify the arm's effective geometry is what we intended
    import yaml
    d = yaml.safe_load(s)["playbook"]["exit"]
    print(f"{name:12} pt={d['profit_target_pct']} pt_atr={d['profit_target_atr_cap']['atr_multiple']} "
          f"horizon={d['profit_target_horizon_cap']['sqrt_days_multiple']} "
          f"stop={d['stop_loss_pct']} stop_atr={d['stop_loss_atr_cap']['atr_multiple']}  -> {out_yaml}")
print("generated", len(ARMS), "arms")
