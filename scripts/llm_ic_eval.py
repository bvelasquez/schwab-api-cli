#!/usr/bin/env python3
"""Rank IC of journaled LLM scores against Tier C forward excess.

Pre-registered pass (all required, post knowledge-cutoff rows only):
  * mean daily Spearman rank IC >= 0.03
  * day-clustered t >= 2
  * incremental IC (score residualized on rsi_14 and above_sma_20) >= 0.02
  * same sign in both regime splits when each split has >= 10 days
  * named IC exceeds blinded IC by >= 0.01 when blinded rows exist

Dry-run rows (live=false) are ignored. A fail means stop spending on an
in-loop LLM and keep the model on research duty only.
"""
from __future__ import annotations

import argparse
import json
import math
import statistics
from collections import defaultdict
from pathlib import Path

RANK_IC_MIN = 0.03
T_MIN = 2.0
INCREMENTAL_IC_MIN = 0.02
BLIND_GAP_MIN = 0.01
MIN_REGIME_DAYS = 10


def load_jsonl(path: Path) -> list[dict]:
    if not path.is_file():
        return []
    rows = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return rows


def spearman(xs: list[float], ys: list[float]) -> float | None:
    if len(xs) < 5 or len(xs) != len(ys):
        return None
    rx = _ranks(xs)
    ry = _ranks(ys)
    return _pearson(rx, ry)


def _ranks(vals: list[float]) -> list[float]:
    order = sorted(range(len(vals)), key=lambda i: vals[i])
    ranks = [0.0] * len(vals)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and vals[order[j + 1]] == vals[order[i]]:
            j += 1
        avg = (i + j) / 2.0 + 1.0
        for k in range(i, j + 1):
            ranks[order[k]] = avg
        i = j + 1
    return ranks


def _pearson(xs: list[float], ys: list[float]) -> float | None:
    n = len(xs)
    if n < 3:
        return None
    mx = sum(xs) / n
    my = sum(ys) / n
    num = sum((a - mx) * (b - my) for a, b in zip(xs, ys))
    dx = math.sqrt(sum((a - mx) ** 2 for a in xs))
    dy = math.sqrt(sum((b - my) ** 2 for b in ys))
    if dx == 0 or dy == 0:
        return None
    return num / (dx * dy)


def _mean_t(daily: list[float]) -> tuple[float | None, float | None, int]:
    vals = [v for v in daily if v is not None and math.isfinite(v)]
    n = len(vals)
    if n < 2:
        return (None, None, n)
    mu = sum(vals) / n
    sd = statistics.pstdev(vals)
    if sd == 0:
        # Constant daily IC. The t-stat is undefined; report a large finite
        # value so a perfect series still clears the pre-registered bar.
        sign = 0.0 if mu == 0.0 else math.copysign(1e6, mu)
        return (mu, sign, n)
    # Day is the cluster. One IC per day, so the t-stat is over days.
    t = mu / (sd / math.sqrt(n))
    return (mu, t, n)


def residualize(y: list[float], cols: list[list[float]]) -> list[float]:
    """OLS residual of y on cols plus an intercept. Falls back to y if singular."""
    n = len(y)
    k = 1 + len(cols)
    xtx = [[0.0] * k for _ in range(k)]
    xty = [0.0] * k
    for i in range(n):
        row = [1.0] + [c[i] for c in cols]
        for a in range(k):
            xty[a] += row[a] * y[i]
            for b in range(k):
                xtx[a][b] += row[a] * row[b]
    beta = _solve(xtx, xty)
    if beta is None:
        return list(y)
    out = []
    for i in range(n):
        row = [1.0] + [c[i] for c in cols]
        fitted = sum(b * x for b, x in zip(beta, row))
        out.append(y[i] - fitted)
    return out


def _solve(a: list[list[float]], b: list[float]) -> list[float] | None:
    n = len(b)
    m = [row[:] + [b[i]] for i, row in enumerate(a)]
    for col in range(n):
        pivot = max(range(col, n), key=lambda r: abs(m[r][col]))
        if abs(m[pivot][col]) < 1e-12:
            return None
        m[col], m[pivot] = m[pivot], m[col]
        div = m[col][col]
        for j in range(col, n + 1):
            m[col][j] /= div
        for r in range(n):
            if r == col:
                continue
            factor = m[r][col]
            for j in range(col, n + 1):
                m[r][j] -= factor * m[col][j]
    return [m[i][n] for i in range(n)]


def after_cutoff(row: dict) -> bool:
    cutoff = (row.get("model_cutoff") or "")[:10]
    day = (row.get("day") or "")[:10]
    if not cutoff or not day:
        return False
    return day > cutoff


def evaluate(decisions: list[dict], outcomes: list[dict], horizon: str = "fwd_5d") -> dict:
    outcomes_by = {(o.get("symbol"), o.get("day")): o for o in outcomes}
    named = [
        d
        for d in decisions
        if d.get("live") and not d.get("blinded") and d.get("source", "scorer") == "scorer" and after_cutoff(d)
    ]
    by_model: dict[str, list[dict]] = defaultdict(list)
    for row in named:
        by_model[row.get("model") or ""].append(row)

    models = {}
    for model, rows in sorted(by_model.items()):
        models[model] = _eval_model(model, rows, decisions, outcomes_by, horizon)

    passed = bool(models) and all(m["pass"] for m in models.values())
    return {
        "horizon": horizon,
        "criteria": {
            "rank_ic_min": RANK_IC_MIN,
            "t_min": T_MIN,
            "incremental_ic_min": INCREMENTAL_IC_MIN,
            "blind_gap_min": BLIND_GAP_MIN,
        },
        "models": models,
        "pass": passed,
        "note": "Fail means keep the LLM off the order path.",
    }


def _eval_model(model: str, rows: list[dict], decisions: list[dict], outcomes_by: dict, horizon: str) -> dict:
    per_day: dict[str, list[tuple[float, float, float, float, str]]] = defaultdict(list)
    for row in rows:
        outcome = outcomes_by.get((row.get("symbol"), row.get("day")))
        if not outcome:
            continue
        fwd = outcome.get(horizon)
        if fwd is None:
            continue
        rsi = outcome.get("rsi_14")
        above = outcome.get("above_sma_20")
        regime = (outcome.get("regime") or "unknown")
        per_day[row["day"]].append(
            (
                float(row.get("score") or 0),
                float(fwd),
                float(rsi) if isinstance(rsi, (int, float)) else 50.0,
                1.0 if above else 0.0,
                regime,
            )
        )
    daily_ic = []
    daily_inc = []
    regime_days: dict[str, list[float]] = defaultdict(list)
    for day, pts in sorted(per_day.items()):
        ic = spearman([p[0] for p in pts], [p[1] for p in pts])
        daily_ic.append(ic)
        resid = residualize([p[0] for p in pts], [[p[2] for p in pts], [p[3] for p in pts]])
        daily_inc.append(spearman(resid, [p[1] for p in pts]))
        by_reg: dict[str, list[tuple[float, float]]] = defaultdict(list)
        for p in pts:
            by_reg[p[4]].append((p[0], p[1]))
        for reg, pairs in by_reg.items():
            ric = spearman([a for a, _ in pairs], [b for _, b in pairs])
            if ric is not None:
                regime_days[reg].append(ric)

    mean_ic, t_ic, n_days = _mean_t(daily_ic)
    mean_inc, _, _ = _mean_t(daily_inc)
    blind_rows = [
        d
        for d in decisions
        if d.get("live") and d.get("blinded") and d.get("model") == model and d.get("source", "scorer") == "scorer" and after_cutoff(d)
    ]
    blind_ic = _blind_ic(blind_rows, outcomes_by, horizon)
    gap = None if mean_ic is None or blind_ic is None else mean_ic - blind_ic
    signs = []
    regime_report = {}
    for reg, ics in regime_days.items():
        mu, _, n = _mean_t(ics)
        regime_report[reg] = {"mean_ic": mu, "days": n}
        if n >= MIN_REGIME_DAYS and mu is not None:
            signs.append(mu)
    same_sign = len(signs) < 2 or all(s > 0 for s in signs) or all(s < 0 for s in signs)
    ok_ic = mean_ic is not None and mean_ic >= RANK_IC_MIN
    ok_t = t_ic is not None and t_ic >= T_MIN
    ok_inc = mean_inc is not None and mean_inc >= INCREMENTAL_IC_MIN
    ok_blind = gap is not None and gap >= BLIND_GAP_MIN
    return {
        "model": model,
        "days": n_days,
        "mean_rank_ic": mean_ic,
        "t": t_ic,
        "incremental_ic": mean_inc,
        "blinded_ic": blind_ic,
        "named_minus_blinded": gap,
        "regimes": regime_report,
        "same_sign": same_sign,
        "pass": bool(ok_ic and ok_t and ok_inc and ok_blind and same_sign),
    }


def _blind_ic(rows: list[dict], outcomes_by: dict, horizon: str) -> float | None:
    per_day: dict[str, list[tuple[float, float]]] = defaultdict(list)
    for row in rows:
        outcome = outcomes_by.get((row.get("symbol"), row.get("day")))
        if not outcome or outcome.get(horizon) is None:
            continue
        per_day[row["day"]].append((float(row.get("score") or 0), float(outcome[horizon])))
    ics = []
    for pts in per_day.values():
        ic = spearman([a for a, _ in pts], [b for _, b in pts])
        if ic is not None:
            ics.append(ic)
    mu, _, _ = _mean_t(ics)
    return mu


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--decisions", type=Path, default=Path("rules/llm-signal-journal.jsonl"))
    p.add_argument("--outcomes", type=Path, default=Path("rules/candidate-outcomes.jsonl"))
    p.add_argument("--horizon", default="fwd_5d")
    p.add_argument("--json", action="store_true")
    p.add_argument("--self-test", action="store_true")
    args = p.parse_args()
    if args.self_test:
        report = _self_test()
    else:
        report = evaluate(load_jsonl(args.decisions), load_jsonl(args.outcomes), args.horizon)
    text = json.dumps(report, indent=2)
    print(text)
    return 0 if report["pass"] else 2


def _self_test() -> dict:
    decisions = []
    outcomes = []
    # Five names, twenty days, score aligned with forward excess. Blinded scores are noise.
    for d in range(20):
        day = f"2026-06-{d+1:02d}"
        for i, sym in enumerate(["AAA", "BBB", "CCC", "DDD", "EEE"]):
            excess = (i - 2) * 0.4
            regime = "low_vol_trend" if d < 10 else "elevated_vol"
            decisions.append(
                {
                    "day": day,
                    "symbol": sym,
                    "model": "google/gemini-2.5-flash",
                    "model_cutoff": "2025-01-01",
                    "blinded": False,
                    "live": True,
                    "source": "scorer",
                    "score": i - 2,
                }
            )
            decisions.append(
                {
                    "day": day,
                    "symbol": sym,
                    "model": "google/gemini-2.5-flash",
                    "model_cutoff": "2025-01-01",
                "blinded": True,
                "live": True,
                "source": "scorer",
                "score": 2 - i,
                }
            )
            outcomes.append(
                {
                    "day": day,
                    "symbol": sym,
                    "fwd_5d": excess,
                    "rsi_14": 50,
                    "above_sma_20": True,
                    "regime": regime,
                }
            )
    report = evaluate(decisions, outcomes)
    assert report["pass"], report
    return report


if __name__ == "__main__":
    raise SystemExit(main())
