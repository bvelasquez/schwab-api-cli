#!/usr/bin/env python3
"""Gate probe matrix on the ALIGNED cache (58 syms, 520 bars each, 2024-07-01..2026-07-28).

Caches are already byte-identical for every arm (sha 5b722a08c9ae8049) - this driver only runs
and harvests, it never touches a cache. Each run's journal is copied aside immediately, because
`-j` rewrites the journal and `--fresh` resets it.
"""
import json, subprocess, pathlib, shutil, statistics as st, math

REPO = pathlib.Path.home() / "projects/schwabinvestbot"
AGD = REPO / "rules/analysis-20260918"
JOURNAL = AGD / "trader-backtest-journal.json"
BIN = "schwab-trader"
ARMS = ["C", "P", "P2", "P3", "Y", "Z"]
WINDOWS = {"W1": ("2024-07-01", "2025-06-30"), "W2": ("2025-07-01", "2026-06-26")}


def run(arm, tag):
    frm, to = WINDOWS[tag]
    out = AGD / f"arm-{arm}-{tag}.aligned.json"
    cmd = [BIN, "backtest", "run", "--rules-file", str(AGD / f"arm-{arm}.yaml"),
           "--from", frm, "--to", to, "--fresh", "--no-learn", "-j"]
    p = subprocess.run(cmd, capture_output=True, text=True, cwd=REPO, timeout=600)
    if p.returncode != 0:
        return {"arm": arm, "tag": tag, "error": (p.stderr or p.stdout)[-400:]}
    out.write_text(p.stdout)
    cands = sorted(AGD.glob(f"trader-backtest-journal-arm-{arm}.jsonl")) or \
            sorted(AGD.glob(f"*backtest-journal*{arm}*.jsonl"))
    if not cands:
        return {"arm": arm, "tag": tag, "error": "no journal found",
                "journals": [x.name for x in AGD.glob("*journal*.jsonl")]}
    shutil.copy(cands[0], AGD / f"trades-aligned-{arm}-{tag}.jsonl")
    d = json.loads(p.stdout)
    dd = d.get("data", d)
    fs = dd.get("final_stats", {})
    return {"arm": arm, "tag": tag, "fills": None, "closed": fs.get("closed_trades"),
            "roi": fs.get("roi_pct"), "exp": fs.get("expectancy_usd"),
            "pnl": fs.get("total_pnl_usd"),
            "exits": fs.get("exit_reason_counts"), "dd": fs.get("max_drawdown_pct"),
            "win": fs.get("win_rate_pct")}


res = []
for tag in ("W1", "W2"):
    for arm in ARMS:
        r = run(arm, tag)
        res.append(r)
        print(" ran", arm, tag, r.get("closed"), r.get("pnl"), r.get("error", "")[:120], flush=True)

json.dump(res, open(AGD / "gate-probes-aligned.json", "w"), indent=1)

def fmt(v, w, d=2):
    return f"{v:{w}.{d}f}" if v is not None else "n/a".rjust(w)

print(f"\n{'arm':4} {'win':3} {'closed':>6} {'net$':>9} {'exp$':>8} {'roi%':>7} {'win%':>6} {'maxDD%':>7}")
for r in res:
    if r.get("error"):
        print(f"{r['arm']:4} {r['tag']:3}  ERROR {r['error'][:80]}")
        continue
    print(f"{r['arm']:4} {r['tag']:3} {str(r['closed']):>6} {fmt(r['pnl'], 9)} {fmt(r['exp'], 8)} "
          f"{fmt(r['roi'], 7)} {fmt(r['win'], 6, 1)} {fmt(r['dd'], 7)}")

# per-trade CI from the journals
def pnls(arm, tag):
    p = AGD / f"trades-aligned-{arm}-{tag}.jsonl"
    v = []
    for line in open(p, encoding="utf-8", errors="replace"):
        try:
            e = json.loads(line)
        except Exception:
            continue
        if "exit_filled" in e.get("type", ""):
            v.append(float((e.get("payload") or {}).get("pnl_usd") or 0.0))
    return v

T95 = {1:12.71,2:4.303,3:3.182,4:2.776,5:2.571,6:2.447,7:2.365,8:2.306,9:2.262,10:2.228,11:2.201,
       12:2.179,13:2.160,14:2.145,15:2.131,16:2.120,17:2.110,18:2.101,19:2.093,20:2.086,21:2.080,
       22:2.074,23:2.069,24:2.064,25:2.060,26:2.056,27:2.052,28:2.048,29:2.045,30:2.042,40:2.021,
       50:2.009,60:2.000,80:1.990,100:1.984,200:1.972,500:1.965}

def tc(df):
    ks = sorted(T95)
    if df in T95: return T95[df]
    lo = max([k for k in ks if k <= df] or [ks[0]]); hi = min([k for k in ks if k >= df] or [ks[-1]])
    return T95[lo]

print(f"\n{'arm':4} {'win':3} {'n':>3} {'exp$/trade':>11} {'95% CI':>21}")
for tag in ("W1", "W2"):
    for arm in ARMS:
        v = pnls(arm, tag)
        if len(v) < 2:
            print(f"{arm:4} {tag:3} {len(v):3} {'(empty)':>11} {'-':>21}")
            continue
        m, sd = st.mean(v), st.stdev(v)
        h = tc(len(v) - 1) * sd / math.sqrt(len(v))
        span = "SPANS 0 (noise)" if m - h <= 0 <= m + h else "excludes 0"
        print(f"{arm:4} {tag:3} {len(v):3} {m:11.2f} {f'[{m-h:7.2f},{m+h:7.2f}]':>21}  {span}")
