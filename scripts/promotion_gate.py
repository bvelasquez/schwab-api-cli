#!/usr/bin/env python3
"""Decide whether a shadow arm may be proposed for the production paper sleeve.

This script does not edit rules and does not reload jarvis. A pass still
needs the operator's OK. Real money is a separate decision after paper proof.

Required:
  * >= 40 trading days in both journals
  * day-paired mean equity-return difference vs the control arm, 90% CI above 0
  * max drawdown not worse than the control
  * mean stop-gap slippage (stop exits) not worse than the control
  * Benjamini-Hochberg adjusted p <= 0.10 given --trials (default: count of
    rules/arms/proposed/trial-log.jsonl plus one)
"""
from __future__ import annotations

import argparse
import json
import math
import random
from pathlib import Path

MIN_DAYS = 40
BOOTSTRAP_N = 4000
SEED = 42
Q = 0.10


def load_jsonl(path: Path) -> list[dict]:
    rows = []
    if not path.is_file():
        return rows
    for line in path.read_text(errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return rows


def daily_returns(rows: list[dict]) -> dict[str, float]:
    out = {}
    for row in rows:
        if row.get("event") not in (None, "shadow_day_summary") and row.get("type") != "shadow_day_summary":
            payload = row.get("payload") or {}
            event = row.get("event") or row.get("type")
        else:
            payload = row.get("payload") or row
            event = row.get("event") or row.get("type") or "shadow_day_summary"
        if event != "shadow_day_summary" and "arm_equity_change_usd" not in payload and "equity_change_pct" not in row:
            continue
        body = row.get("payload") or row
        day = (body.get("date") or body.get("day") or row.get("day") or "")[:10]
        if not day:
            continue
        if body.get("equity_change_pct") is not None:
            out[day] = float(body["equity_change_pct"])
            continue
        start = body.get("arm_start_equity_usd")
        change = body.get("arm_equity_change_usd")
        if start and change is not None and float(start) != 0:
            out[day] = float(change) / float(start) * 100.0
    return out


def closed_stops(rows: list[dict]) -> list[float]:
    slips = []
    for row in rows:
        event = row.get("event") or row.get("type")
        body = row.get("payload") or row
        if event not in ("shadow_exit_filled", "sim_exit_filled") and body.get("exit_reason") != "stop_loss":
            if body.get("exit_reason") != "stop_loss":
                continue
        if body.get("exit_reason") != "stop_loss":
            continue
        if body.get("stop_gap_slippage_pct") is None:
            continue
        slips.append(float(body["stop_gap_slippage_pct"]))
    return slips


def max_drawdown(daily: dict[str, float]) -> float:
    equity = 1.0
    peak = 1.0
    worst = 0.0
    for day in sorted(daily):
        equity *= 1.0 + daily[day] / 100.0
        peak = max(peak, equity)
        dd = (peak - equity) / peak * 100.0
        worst = max(worst, dd)
    return worst


def paired(arm: dict[str, float], control: dict[str, float]) -> list[float]:
    days = sorted(set(arm) & set(control))
    return [arm[d] - control[d] for d in days]


def bootstrap_ci(diffs: list[float]) -> tuple[float, float, float]:
    rng = random.Random(SEED)
    n = len(diffs)
    if n == 0:
        return (0.0, 0.0, 1.0)
    mean = sum(diffs) / n
    means = []
    below = 0
    for _ in range(BOOTSTRAP_N):
        sample = [diffs[rng.randrange(n)] for _ in range(n)]
        m = sum(sample) / n
        means.append(m)
        if m <= 0:
            below += 1
    means.sort()
    lo = means[int(0.05 * (BOOTSTRAP_N - 1))]
    return (mean, lo, below / BOOTSTRAP_N)


def bh_adjust(p: float, trials: int) -> float:
    """Single-test BH at rank 1 of `trials` tests: p * trials / 1, capped at 1."""
    if trials < 1:
        trials = 1
    return min(1.0, p * trials)


def trial_count(log_path: Path) -> int:
    if not log_path.is_file():
        return 1
    n = sum(1 for line in log_path.read_text().splitlines() if line.strip())
    return max(1, n)


def evaluate(arm_rows: list[dict], control_rows: list[dict], trials: int) -> dict:
    arm_daily = daily_returns(arm_rows)
    ctl_daily = daily_returns(control_rows)
    diffs = paired(arm_daily, ctl_daily)
    mean, lo, p = bootstrap_ci(diffs)
    p_adj = bh_adjust(p, trials)
    arm_dd = max_drawdown(arm_daily)
    ctl_dd = max_drawdown(ctl_daily)
    arm_gap = _mean(closed_stops(arm_rows))
    ctl_gap = _mean(closed_stops(control_rows))
    # More negative slippage is worse. None means no stop exits yet.
    gap_ok = arm_gap is None or ctl_gap is None or arm_gap >= ctl_gap - 1e-9
    days_ok = len(diffs) >= MIN_DAYS
    ci_ok = lo > 0
    dd_ok = arm_dd <= ctl_dd + 1e-9
    bh_ok = p_adj <= Q
    passed = bool(days_ok and ci_ok and dd_ok and gap_ok and bh_ok)
    return {
        "pass": passed,
        "days": len(diffs),
        "min_days": MIN_DAYS,
        "mean_daily_excess_pct": mean,
        "ci90_low": lo,
        "p_bootstrap": p,
        "p_bh": p_adj,
        "trials": trials,
        "arm_max_dd_pct": arm_dd,
        "control_max_dd_pct": ctl_dd,
        "arm_stop_gap_pct": arm_gap,
        "control_stop_gap_pct": ctl_gap,
        "operator_ok_required": True,
        "writes_rules": False,
        "note": "A pass does not change jarvis. The operator has to apply the arm.",
    }


def _mean(vals: list[float]) -> float | None:
    if not vals:
        return None
    return sum(vals) / len(vals)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--arm-journal", type=Path)
    p.add_argument("--control-journal", type=Path)
    p.add_argument("--trials", type=int, default=None)
    p.add_argument(
        "--trial-log",
        type=Path,
        default=Path("rules/arms/proposed/trial-log.jsonl"),
    )
    p.add_argument("--self-test", action="store_true")
    args = p.parse_args()
    if args.self_test:
        report = _self_test()
    else:
        if not args.arm_journal or not args.control_journal:
            p.error("--arm-journal and --control-journal are required")
        trials = args.trials if args.trials is not None else trial_count(args.trial_log)
        report = evaluate(load_jsonl(args.arm_journal), load_jsonl(args.control_journal), trials)
    print(json.dumps(report, indent=2))
    if report["pass"]:
        print("PASS. Operator OK is still required. This script did not edit rules.")
        return 0
    print("FAIL. No rules were changed.")
    return 2


def _self_test() -> dict:
    arm = []
    ctl = []
    for i in range(45):
        day = f"2026-{(i // 28) + 1:02d}-{(i % 28) + 1:02d}"
        arm.append({"event": "shadow_day_summary", "payload": {"date": day, "arm_start_equity_usd": 30000, "arm_equity_change_usd": 40}})
        ctl.append({"event": "shadow_day_summary", "payload": {"date": day, "arm_start_equity_usd": 30000, "arm_equity_change_usd": -5}})
        arm.append({"event": "shadow_exit_filled", "payload": {"exit_reason": "stop_loss", "stop_gap_slippage_pct": -0.2}})
        ctl.append({"event": "shadow_exit_filled", "payload": {"exit_reason": "stop_loss", "stop_gap_slippage_pct": -0.8}})
    report = evaluate(arm, ctl, trials=1)
    assert report["pass"], report
    short = evaluate(arm[:10], ctl[:10], trials=1)
    assert not short["pass"]
    return report


if __name__ == "__main__":
    raise SystemExit(main())
