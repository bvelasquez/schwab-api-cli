#!/usr/bin/env python3
"""Summarize an arm/window grid from the backtest outputs in rules/analysis-20260917/bt/."""
import json
import glob
import os
import sys
import statistics as st

START = 4000.0
prefixes = sys.argv[1:] or ["p3-"]


def load(p):
    t = open(p).read()
    return json.loads(t[t.index("{"):])


rows = []
for p in sorted(glob.glob("rules/analysis-20260917/bt/*.txt")):
    base = os.path.basename(p)[:-4]
    if not any(base.startswith(x) for x in prefixes):
        continue
    try:
        j = load(p)
    except Exception as e:
        print(f"{base}: PARSE FAIL {e}")
        continue
    fs = j["final_stats"]
    b = j["benchmark_comparison"]
    dep = [d["sim_stats"]["current_equity_usd"] - d["sim_stats"]["cash_usd"] for d in j["day_summaries"]]
    md = st.mean(dep)
    rows.append(dict(
        run=base, days=j["trading_days"], trades=fs["closed_trades"], open=fs["open_positions"],
        roi=fs["roi_pct"], spy=b["buy_hold_roi_pct"], wr=fs["win_rate_pct"],
        exp=fs["expectancy_usd"], pnl=fs["total_pnl_usd"], dd=fs["max_drawdown_pct"],
        dep=md, dep_pct=md / START * 100, on_dep=fs["total_pnl_usd"] / md * 100 if md else 0,
        exits=fs["exit_reason_counts"],
    ))

hdr = (f"{'run':16}{'days':>5}{'trd':>5}{'ROI%':>8}{'SPY%':>7}{'excess':>8}{'WR%':>7}"
       f"{'exp/trd':>9}{'PnL$':>9}{'maxDD':>7}{'avgDep':>8}{'%cap':>6}{'ret/dep':>9}")
print(hdr)
print("-" * len(hdr))
for r in rows:
    print(f"{r['run']:16}{r['days']:>5}{r['trades']:>5}{r['roi']:>+8.2f}{r['spy']:>+7.2f}"
          f"{r['roi']-r['spy']:>+8.2f}{r['wr']:>7.1f}{r['exp']:>+9.2f}{r['pnl']:>+9.2f}"
          f"{r['dd']:>7.2f}{r['dep']:>8.0f}{r['dep_pct']:>5.1f}%{r['on_dep']:>+8.1f}%")
print()
for r in rows:
    print(f"{r['run']:16} exits: " + ", ".join(f"{k}={v}" for k, v in sorted(r["exits"].items())))
