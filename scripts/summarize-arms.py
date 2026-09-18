#!/usr/bin/env python3
"""Walk-forward arm comparison: arm-C (live) vs arm-P (+require_below_sma[9]).
Reads /tmp/arm<ARM>-<TAG>.json (envelope: payload under 'data') and the per-run
trade journal copies /tmp/trades-arm<ARM>-<TAG>.jsonl for a bootstrap CI."""
import json, os, random, collections

random.seed(7)
TAGS = [("W1", "in-sample  2024-07-01..2025-06-30"),
        ("W2", "out-sample 2025-07-01..2026-06-26"),
        ("W3", "live win   2026-06-29..2026-07-28")]

def trades(path):
    pnls, exits, syms = [], collections.Counter(), set()
    if not os.path.exists(path):
        return None
    for line in open(path, encoding="utf-8", errors="replace"):
        try:
            d = json.loads(line)
        except Exception:
            continue
        if "exit_filled" in d.get("type", ""):
            p = d.get("payload", {}) or {}
            if p.get("pnl_usd") is not None:
                pnls.append(float(p["pnl_usd"]))
            exits[p.get("exit_reason", "?")] += 1
            syms.add(p.get("symbol"))
    return {"n": len(pnls), "pnls": pnls, "exits": dict(exits), "syms": syms}

def boot_ci(xs, n=20000):
    if len(xs) < 3:
        return None
    m = len(xs)
    means = sorted(sum(xs[random.randrange(m)] for _ in range(m)) / m for _ in range(n))
    return means[int(.025*n)], means[int(.975*n)]

print(f"{'win':<4} {'arm':<4} {'trades':>6} {'roi%':>7} {'exp$':>7} {'win%':>6} {'maxDD%':>7} {'SPY%':>6}  exits / bootstrap 95% CI on mean P&L")
store = {}
for tag, label in TAGS:
    for arm in ("C", "P"):
        try:
            d = json.load(open(f"/tmp/arm{arm}-{tag}.json"))["data"]
        except Exception as e:
            print(f"{tag:<4} {arm:<4} missing json ({e})"); continue
        fs = d.get("final_stats", {})
        bc = d.get("benchmark_comparison", {}) or {}
        tj = trades(f"/tmp/trades-arm{arm}-{tag}.jsonl") or {"n": 0, "pnls": [], "exits": {}}
        ci = boot_ci(tj["pnls"])
        store[(tag, arm)] = {"n": tj["n"], "pnls": tj["pnls"], "mean": (sum(tj["pnls"])/tj["n"] if tj["n"] else None),
                             "ci": ci, "roi": fs.get("roi_pct"), "fs": fs,
                             "sym_n": len(tj["syms"]), "symbols_cached": d.get("symbols_cached")}
        print(f"{tag:<4} {arm:<4} {tj['n']:>6} {round(fs.get('roi_pct') or 0,2):>7} "
              f"{round(fs.get('expectancy_usd') or 0,1):>7} {round(fs.get('win_rate_pct') or 0,1):>6} "
              f"{round(fs.get('max_drawdown_pct') or 0,1):>7} {bc.get('buy_hold_roi_pct'):>6}  "
              f"{tj['exits']}  CI=[{ci[0]:.1f},{ci[1]:.1f}]" if ci else
              f"{tag:<4} {arm:<4} {tj['n']:>6} {round(fs.get('roi_pct') or 0,2):>7} "
              f"{round(fs.get('expectancy_usd') or 0,1):>7} {round(fs.get('win_rate_pct') or 0,1):>6} "
              f"{round(fs.get('max_drawdown_pct') or 0,1):>7} {bc.get('buy_hold_roi_pct'):>6}  {tj['exits']}  CI=n<3")
        print(f"        symbols_cached={d.get('symbols_cached')} distinct_symbols_traded={len(tj['syms'])} cache={d.get('cache_path')}")

print("\n=== pairing ===")
for tag, _ in TAGS:
    c, p = store.get((tag, "C")), store.get((tag, "P"))
    if not c or not p or c["mean"] is None or p["mean"] is None:
        print(f"{tag}: insufficient trades for a paired read (C n={c['n'] if c else '-'}, P n={p['n'] if p else '-'})"); continue
    d = p["mean"] - c["mean"]
    pooled = c["pnls"] + p["pnls"]
    sd = (sum((x - sum(pooled)/len(pooled))**2 for x in pooled) / max(1, len(pooled)-1))**0.5
    se = sd * (1/len(c["pnls"]) + 1/len(p["pnls"]))**0.5
    print(f"{tag}: E_P - E_C = {d:+.1f}$  (P {p['mean']:.1f} vs C {c['mean']:.1f})  pooled sd={sd:.1f} se={se:.1f} "
          f"CI=[{d-1.96*se:+.1f},{d+1.96*se:+.1f}]  roi P-C = {((p['roi'] or 0)-(c['roi'] or 0)):+.2f}pp")