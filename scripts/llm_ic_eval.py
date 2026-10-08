#!/usr/bin/env python3
"""Rank IC of the premarket LLM signal against Tier C outcomes.

Pre-registered pass (all required, post knowledge-cutoff only):
  - mean daily rank IC >= 0.03
  - day-clustered t >= 2  (mean of daily ICs / se)
  - incremental IC over RSI > 0 (same sign)
  - same sign in every regime split that has >= 10 days
  - blinded IC reported beside named IC (not required to be lower to pass,
    but a blinded IC within 0.01 of the named IC is a memorization warning)

Insufficient data prints FAIL with reason "insufficient" and exits 0.
This script never changes rules or orders.
"""
from __future__ import annotations

import argparse
import json
import math
import statistics
from collections import defaultdict
from pathlib import Path


def spearman(xs: list[float], ys: list[float]) -> float | None:
    n = len(xs)
    if n < 5:
        return None
    def ranks(vs: list[float]) -> list[float]:
        order = sorted(range(n), key=lambda i: vs[i])
        r = [0.0] * n
        for rank, i in enumerate(order):
            r[i] = rank + 1
        return r
    rx, ry = ranks(xs), ranks(ys)
    mx, my = statistics.fmean(rx), statistics.fmean(ry)
    num = sum((a - mx) * (b - my) for a, b in zip(rx, ry))
    dx = math.sqrt(sum((a - mx) ** 2 for a in rx))
    dy = math.sqrt(sum((b - my) ** 2 for b in ry))
    if dx == 0 or dy == 0:
        return None
    return num / (dx * dy)


def ols_residual(y: list[float], x: list[float]) -> list[float]:
    n = len(y)
    mx, my = statistics.fmean(x), statistics.fmean(y)
    var = sum((a - mx) ** 2 for a in x)
    if var == 0:
        return [v - my for v in y]
    beta = sum((a - mx) * (b - my) for a, b in zip(x, y)) / var
    alpha = my - beta * mx
    return [b - (alpha + beta * a) for a, b in zip(x, y)]


def trader_id_from(path: Path) -> str:
    if path.is_file():
        for line in path.read_text().splitlines():
            if line.startswith("trader_id:"):
                return line.split(":", 1)[1].strip().strip('"')
    return "trader"


def load_outcomes(path: Path) -> dict[tuple[str, str], dict]:
    out = {}
    if not path.is_file():
        return out
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        out[(row["symbol"], row["date"])] = row
    return out


def load_signals(path: Path, cutoff: str) -> list[dict]:
    rows = []
    if not path.is_file():
        return rows
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        doc = json.loads(line)
        day = doc.get("date") or ""
        if cutoff and day < cutoff:
            continue
        model = doc.get("model")
        for sym, d in (doc.get("symbols") or {}).items():
            score = d.get("score")
            if score is None:
                continue
            rows.append({
                "date": day,
                "symbol": sym,
                "score": float(score),
                "blinded_score": d.get("blinded_score"),
                "compare_score": d.get("compare_score"),
                "model": model,
                "regime": d.get("regime"),
            })
    return rows


def ic_table(pairs: list[tuple[float, float, str]]) -> dict:
    by_day: dict[str, list[tuple[float, float]]] = defaultdict(list)
    for score, ret, day in pairs:
        by_day[day].append((score, ret))
    daily = []
    for day, rows in sorted(by_day.items()):
        ic = spearman([a for a, _ in rows], [b for _, b in rows])
        if ic is not None:
            daily.append(ic)
    if len(daily) < 2:
        return {"days": len(daily), "mean_ic": None, "t": None}
    mu = statistics.fmean(daily)
    se = statistics.pstdev(daily) / math.sqrt(len(daily)) if len(daily) else None
    t = None if not se else mu / se
    return {"days": len(daily), "mean_ic": mu, "t": t}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rules-dir", type=Path, default=Path("rules"))
    ap.add_argument("--trader-id", default=None)
    ap.add_argument("--cutoff", default="2025-01-01")
    args = ap.parse_args()
    trader_id = args.trader_id or trader_id_from(args.rules_dir / "trader-swing-9947.yaml")
    outcomes = load_outcomes(args.rules_dir / "analysis" / "candidate-outcomes.jsonl")
    signals = load_signals(
        args.rules_dir / f"llm-signal-journal-{trader_id}.jsonl",
        args.cutoff,
    )
    named, blinded, rsi_pairs = [], [], []
    by_regime: dict[str, list] = defaultdict(list)
    for s in signals:
        o = outcomes.get((s["symbol"], s["date"]))
        if not o or o.get("fwd_10d_excess") is None:
            continue
        ret = o["fwd_10d_excess"]
        named.append((s["score"], ret, s["date"]))
        if s["blinded_score"] is not None:
            blinded.append((float(s["blinded_score"]), ret, s["date"]))
        if o.get("rsi_14") is not None:
            rsi_pairs.append((s["score"], ret, o["rsi_14"], s["date"]))
        if s.get("regime"):
            by_regime[s["regime"]].append((s["score"], ret, s["date"]))
    named_ic = ic_table(named)
    blind_ic = ic_table(blinded)
    incr = None
    if len(rsi_pairs) >= 20:
        scores = [a for a, _, _, _ in rsi_pairs]
        rets = [b for _, b, _, _ in rsi_pairs]
        rsis = [c for _, _, c, _ in rsi_pairs]
        days = [d for _, _, _, d in rsi_pairs]
        sr = ols_residual(scores, rsis)
        rr = ols_residual(rets, rsis)
        incr = ic_table(list(zip(sr, rr, days)))
    regimes = {k: ic_table(v) for k, v in by_regime.items()}
    signs = [
        v["mean_ic"]
        for v in regimes.values()
        if v.get("days", 0) >= 10 and v.get("mean_ic") is not None
    ]
    same_sign = (not signs) or all(s > 0 for s in signs) or all(s < 0 for s in signs)
    mean_ic = named_ic.get("mean_ic")
    t = named_ic.get("t")
    incr_ic = None if not incr else incr.get("mean_ic")
    passed = (
        mean_ic is not None
        and mean_ic >= 0.03
        and t is not None
        and t >= 2
        and incr_ic is not None
        and incr_ic > 0
        and same_sign
        and named_ic.get("days", 0) >= 20
    )
    warn = None
    if (
        mean_ic is not None
        and blind_ic.get("mean_ic") is not None
        and abs(blind_ic["mean_ic"] - mean_ic) < 0.01
    ):
        warn = "blinded IC is within 0.01 of named IC; memorization not ruled out"
    report = {
        "status": "pass" if passed else "fail",
        "reason": None if passed else "criteria not met or insufficient post-cutoff days",
        "named": named_ic,
        "blinded": blind_ic,
        "incremental_vs_rsi": incr,
        "regimes": regimes,
        "memorization_warning": warn,
        "criteria": {
            "mean_daily_ic": 0.03,
            "day_clustered_t": 2,
            "incremental_ic_positive": True,
            "min_days": 20,
        },
    }
    out = args.rules_dir / "analysis" / "llm-ic-eval.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2))
    print(json.dumps(report))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
