#!/usr/bin/env python3
"""Gate probes: does `require_below_sma` bind in the equity backtest?

Runs a set of arms over one window each, on BYTE-IDENTICAL caches (the C cache is copied to
every other arm's cache name), same universe bytes, --fresh --no-learn, and reports
fills/exits/net PnL per arm. A one-variable change that alters nothing is a NULL result.

Arms: C (live), P (+below[9]), Y (above[200]), Z (above[20,50]+below[20] -> must be 0 if the
gate binds), P2 (above[50]+below[9]), P3 (below[9] only).
"""
import json, os, shutil, subprocess, sys, collections, pathlib

REPO = pathlib.Path.home() / "projects/schwabinvestbot"
AGD = REPO / "rules/analysis-20260918"
ARMS = ["C", "P", "Y", "Z", "P2", "P3"]
WINDOWS = {"W1": ("2024-07-01", "2025-06-30"), "W2": ("2025-07-01", "2026-06-26")}

os.chdir(REPO)
# byte-identical cache for every arm: copy the (only) existing cache
src = AGD / ".backtest-cache-arm-C.json"
print("control cache sha:", subprocess.run(["shasum","-a","256",str(src)],capture_output=True,text=True).stdout.split()[0][:20])
for a in ARMS:
    dst = AGD / f".backtest-cache-arm-{a}.json"
    if not dst.exists():
        shutil.copy2(src, dst)
        print("copied cache ->", dst.name)

def run(arm, tag):
    frm, to = WINDOWS[tag]
    jpath = AGD / f"arm-{arm}-{tag}.json"
    with open(jpath, "w") as fh:
        p = subprocess.run(["schwab-trader", "backtest", "run",
                            "--rules-file", f"rules/analysis-20260918/arm-{arm}.yaml",
                            "--from", frm, "--to", to, "--fresh", "--no-learn", "-j"],
                           stdout=fh, stderr=subprocess.PIPE, text=True)
    err = (p.stderr or "").strip()
    if err:
        print(f"   [{arm}-{tag}] stderr: {err[:200]}")
    trade_j = AGD / f"trades-arm{arm}-{tag}.jsonl"
    src_j = AGD / f"trader-backtest-journal-arm-{arm}.jsonl"
    if src_j.exists():
        shutil.copy2(src_j, trade_j)
    fills = exits = 0; net = 0.0; syms = []
    if trade_j.exists():
        for line in open(trade_j, encoding="utf-8", errors="replace"):
            try: e = json.loads(line)
            except: continue
            pl = e.get("payload") or {}
            if "entry_filled" in e.get("type",""): fills += 1; syms.append(pl.get("symbol"))
            if "exit_filled" in e.get("type",""): exits += 1; net += float(pl.get("pnl_usd") or 0)
    stats = {}
    try:
        stats = json.load(open(jpath))["data"].get("final_stats") or {}
    except Exception as ex:
        print(f"   [{arm}-{tag}] json unreadable: {ex}")
    return {"arm": arm, "tag": tag, "fills": fills, "exits": exits,
            "net_journal": round(net,2), "closed": stats.get("closed_trades"),
            "roi": stats.get("roi_pct"), "exp": stats.get("expectancy_usd"),
            "err_bytes": len(err), "syms": ",".join(str(s) for s in syms[:8])}

rows = []
for tag in ("W1", "W2"):
    for arm in ARMS:
        rows.append(run(arm, tag))

print(f"\n{'window':6} {'arm':4} {'fills':>5} {'exits':>5} {'net$_jr':>9} {'closed':>6} {'roi%':>7} {'exp$':>7}  first symbols")
for r in rows:
    print(f"{r['tag']:6} {r['arm']:4} {r['fills']:5} {r['exits']:5} {r['net_journal']:9} "
          f"{str(r['closed']):>6} {str(r['roi']):>7} {str(r['exp']):>7}  {r['syms']}")
json.dump(rows, open(AGD / "gate-probes.json", "w"), indent=1)
print("\nwrote", AGD / "gate-probes.json")
