#!/usr/bin/env python3
"""Promotion gate for shadow arms versus research-base (and big-base).

An arm is eligible only when all of these hold:
  - at least 40 trading days of shadow_day_summary
  - day-paired expectancy difference versus the control has a CI above 0
  - max drawdown is no worse than the control
  - stop-gap slippage is no worse than the control

This script prints the verdict and stops. It does not edit production rules.
Applying a pass still requires the operator's OK on jarvis. Real money is a
separate decision after paper proof.
"""
from __future__ import annotations

import argparse
import json
import math
import statistics
from pathlib import Path

MIN_DAYS = 40


def load_days(path: Path) -> dict[str, dict]:
    days = {}
    if not path.is_file():
        return days
    for line in path.read_text().splitlines():
        if "shadow_day_summary" not in line:
            continue
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        p = ev.get("payload") or {}
        day = p.get("date")
        if day:
            days[day] = p
    return days


def gap_slippage(path: Path) -> float | None:
    vals = []
    if not path.is_file():
        return None
    for line in path.read_text().splitlines():
        if "shadow_exit_filled" not in line:
            continue
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        v = (ev.get("payload") or {}).get("stop_gap_slippage_pct")
        if isinstance(v, (int, float)):
            vals.append(float(v))
    if not vals:
        return None
    return statistics.fmean(vals)


def max_dd(equity: list[float]) -> float:
    peak = equity[0] if equity else 0.0
    worst = 0.0
    for e in equity:
        peak = max(peak, e)
        if peak > 0:
            worst = min(worst, (e - peak) / peak * 100.0)
    return worst


def paired(arm: dict[str, dict], ctrl: dict[str, dict]) -> dict:
    diffs = []
    for day, row in sorted(arm.items()):
        if day not in ctrl:
            continue
        diffs.append(row.get("arm_equity_change_usd", 0.0) - ctrl[day].get("arm_equity_change_usd", 0.0))
    n = len(diffs)
    if n < 2:
        return {"n": n, "mean": None, "ci_low": None, "ci_high": None}
    mu = statistics.fmean(diffs)
    se = statistics.stdev(diffs) / math.sqrt(n)
    # normal approx; 40 days is the gate, not a t table
    return {"n": n, "mean": mu, "ci_low": mu - 1.96 * se, "ci_high": mu + 1.96 * se}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rules-dir", type=Path, default=Path("rules"))
    ap.add_argument("--trader-id", default=None)
    ap.add_argument("--control", default="research-base")
    args = ap.parse_args()
    stem = args.trader_id or "swing-beneficiary-9947"
    rules = args.rules_dir / "trader-swing-9947.yaml"
    if args.trader_id is None and rules.is_file():
        for line in rules.read_text().splitlines():
            if line.startswith("trader_id:"):
                stem = line.split(":", 1)[1].strip().strip('"')
                break
    ctrl_path = args.rules_dir / f"trader-shadow-journal-{stem}-{args.control}.jsonl"
    ctrl = load_days(ctrl_path)
    verdicts = []
    for path in sorted(args.rules_dir.glob(f"trader-shadow-journal-{stem}-research-*.jsonl")):
        arm_id = path.name.removeprefix(f"trader-shadow-journal-{stem}-").removesuffix(".jsonl")
        if arm_id == args.control:
            continue
        arm = load_days(path)
        stats = paired(arm, ctrl)
        eq = [arm[d].get("equity_usd", 0.0) for d in sorted(arm)]
        ceq = [ctrl[d].get("equity_usd", 0.0) for d in sorted(ctrl)]
        dd = max_dd(eq)
        cdd = max_dd(ceq)
        gap = gap_slippage(path)
        cgap = gap_slippage(ctrl_path)
        reasons = []
        if stats["n"] < MIN_DAYS:
            reasons.append(f"days {stats['n']} < {MIN_DAYS}")
        if stats["ci_low"] is None or stats["ci_low"] <= 0:
            reasons.append("day-paired CI does not sit above 0")
        if ceq and eq and dd < cdd:
            reasons.append(f"max drawdown {dd:.2f}% worse than control {cdd:.2f}%")
        if gap is not None and cgap is not None and gap < cgap:
            reasons.append(f"stop-gap slippage {gap:.3f} worse than control {cgap:.3f}")
        verdicts.append({
            "arm": arm_id,
            "eligible": not reasons,
            "reasons": reasons,
            "paired": stats,
            "operator_ok_required": True,
            "applied": False,
        })
    report = {
        "control": args.control,
        "min_days": MIN_DAYS,
        "arms": verdicts,
        "note": "A pass is not a production change. Operator OK is required. Real money is a separate decision.",
    }
    out = args.rules_dir / "analysis" / "promotion-gate.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2))
    print(json.dumps(report))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
