#!/usr/bin/env python3
"""Tier C capital-free signal study.

For every scanned symbol-day in the swing journal (admitted or rejected),
record forward 1/5/10/20-day returns versus SPY and a hypothetical trade
that uses the strategy's stop, target, and time stop on daily bars.

No sleeve and no slot limit. `data_unavailable` rows are not decisions and
are skipped. Output is rules/analysis/candidate-outcomes.jsonl plus a
stdout summary. Read-only with respect to trading state.
"""
from __future__ import annotations

import argparse
import json
import math
import statistics
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

STOP_PCT = 5.5
TARGET_PCT = 7.5
TIME_STOP_DAYS = 10
HORIZONS = (1, 5, 10, 20)


def load_cache(path: Path) -> dict[str, list[dict]]:
    doc = json.loads(path.read_text())
    return doc.get("symbols") or {}


def bar_dates(bars: list[dict]) -> list[str]:
    out = []
    for b in bars:
        ms = b.get("datetime_ms")
        if ms is None:
            continue
        out.append(datetime.fromtimestamp(ms / 1000, tz=timezone.utc).date().isoformat())
    return out


def index_bars(bars: list[dict]) -> dict[str, int]:
    return {d: i for i, d in enumerate(bar_dates(bars))}


def fwd(bars: list[dict], i: int, n: int) -> float | None:
    j = i + n
    if j >= len(bars):
        return None
    a = bars[i].get("close")
    b = bars[j].get("close")
    if not a or not b:
        return None
    return (b / a - 1.0) * 100.0


def hypothetical(bars: list[dict], i: int) -> dict:
    entry = bars[i].get("close")
    if not entry:
        return {}
    stop = entry * (1.0 - STOP_PCT / 100.0)
    target = entry * (1.0 + TARGET_PCT / 100.0)
    for k in range(1, TIME_STOP_DAYS + 1):
        j = i + k
        if j >= len(bars):
            break
        bar = bars[j]
        op = bar.get("open") or bar.get("close")
        lo = bar.get("low")
        hi = bar.get("high")
        if op is not None and op <= stop:
            fill = op
            reason = "gap_stop"
        elif lo is not None and lo <= stop:
            fill = stop
            reason = "stop_loss"
        elif hi is not None and hi >= target:
            fill = target
            reason = "profit_target"
        else:
            continue
        return {
            "hyp_exit_reason": reason,
            "hyp_hold_days": k,
            "hyp_pnl_pct": (fill / entry - 1.0) * 100.0,
            "hyp_gap": reason == "gap_stop",
        }
    j = min(i + TIME_STOP_DAYS, len(bars) - 1)
    close = bars[j].get("close") or entry
    return {
        "hyp_exit_reason": "time_stop",
        "hyp_hold_days": j - i,
        "hyp_pnl_pct": (close / entry - 1.0) * 100.0,
        "hyp_gap": False,
    }


def iter_decisions(journal: Path):
    """One row per (symbol, day). Admitted wins over a rejection the same day."""
    seen: dict[tuple[str, str], dict] = {}
    with journal.open() as f:
        for line in f:
            if "sim_tick_summary" not in line:
                continue
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            ts = ev.get("ts") or ""
            day = ts[:10]
            scan = (ev.get("payload") or {}).get("scan") or {}
            for row, admitted in (
                *[(c, True) for c in scan.get("candidates") or []],
                *[(r, False) for r in scan.get("rejected") or []],
            ):
                code = row.get("reason_code")
                if code == "data_unavailable":
                    continue
                sym = (row.get("symbol") or "").upper()
                if not sym or not day:
                    continue
                key = (sym, day)
                prev = seen.get(key)
                if prev and prev["admitted"] and not admitted:
                    continue
                tech = row.get("technical_context") or {}
                seen[key] = {
                    "symbol": sym,
                    "date": day,
                    "admitted": admitted,
                    "reason_code": None if admitted else code,
                    "rsi_14": tech.get("rsi_14"),
                    "above_sma_20": (tech.get("above_sma_20")),
                }
    return seen.values()


def mean(xs: list[float]) -> float | None:
    return statistics.fmean(xs) if xs else None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rules-dir", type=Path, default=Path("rules"))
    ap.add_argument("--journal", type=Path, default=None)
    ap.add_argument("--cache", type=Path, default=None)
    args = ap.parse_args()
    rules = args.rules_dir
    journal = args.journal or rules / "trader-journal-trader-swing-9947.jsonl"
    cache_path = args.cache or rules / ".backtest-cache-trader-swing-9947.json"
    if not journal.is_file() or not cache_path.is_file():
        print(json.dumps({"status": "missing_inputs", "journal": str(journal), "cache": str(cache_path)}))
        return 0
    symbols = load_cache(cache_path)
    spy = symbols.get("SPY") or []
    spy_idx = index_bars(spy)
    out_dir = rules / "analysis"
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / "candidate-outcomes.jsonl"
    by_reason: dict[str, list[float]] = defaultdict(list)
    n = 0
    with out_path.open("w") as out:
        for dec in iter_decisions(journal):
            bars = symbols.get(dec["symbol"])
            if not isinstance(bars, list):
                continue
            idx = index_bars(bars)
            i = idx.get(dec["date"])
            if i is None:
                # decision day may be a session the cache has not closed yet
                continue
            row = dict(dec)
            for h in HORIZONS:
                r = fwd(bars, i, h)
                row[f"fwd_{h}d"] = r
                sr = None
                si = spy_idx.get(dec["date"])
                if r is not None and si is not None:
                    sr = fwd(spy, si, h)
                row[f"spy_{h}d"] = sr
                row[f"fwd_{h}d_excess"] = None if r is None or sr is None else r - sr
            row.update(hypothetical(bars, i))
            out.write(json.dumps(row) + "\n")
            n += 1
            key = "ADMITTED" if dec["admitted"] else (dec["reason_code"] or "other")
            if row.get("hyp_pnl_pct") is not None:
                by_reason[key].append(row["hyp_pnl_pct"])
    summary = {
        "rows": n,
        "by_reason": {
            k: {"n": len(v), "mean_hyp_pnl_pct": mean(v)}
            for k, v in sorted(by_reason.items(), key=lambda kv: -len(kv[1]))
        },
    }
    (out_dir / "candidate-outcomes-summary.json").write_text(json.dumps(summary, indent=2))
    print(json.dumps({"status": "ok", "rows": n, "path": str(out_path)}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
