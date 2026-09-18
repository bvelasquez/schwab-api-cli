#!/usr/bin/env python3
"""Per-trade statistics for the gate probes: expectancy with a t-based 95% CI, and a
Welch comparison of the control against each variant.

n is small (a pullback arm yields 13-15 trades/window), so the CI is the point: if it spans
zero the arm is not distinguishable from noise no matter how the means look.
"""
import json, math, pathlib, statistics as st

AGD = pathlib.Path.home() / "projects/schwabinvestbot/rules/analysis-20260918"
ARMS = ["C", "P", "Y", "Z", "P2", "P3"]


def pnls(arm, tag):
    p = AGD / f"trades-arm{arm}-{tag}.jsonl"
    out = []
    if not p.exists():
        return out
    for line in open(p, encoding="utf-8", errors="replace"):
        try:
            e = json.loads(line)
        except Exception:
            continue
        if "exit_filled" in e.get("type", ""):
            out.append(float((e.get("payload") or {}).get("pnl_usd") or 0.0))
    return out


T95 = {0:0.0,1:12.71,2:4.303,3:3.182,4:2.776,5:2.571,6:2.447,7:2.365,8:2.306,9:2.262,
       10:2.228,11:2.201,12:2.179,13:2.160,14:2.145,15:2.131,16:2.120,17:2.110,18:2.101,
       19:2.093,20:2.086,21:2.080,22:2.074,23:2.069,24:2.064,25:2.060,26:2.056,27:2.052,
       28:2.048,29:2.045,30:2.042,40:2.021,50:2.009,60:2.000,80:1.990,100:1.984}


def tcrit(df):
    if df in T95:
        return T95[df]
    ks = sorted(T95)
    lo = max([k for k in ks if k <= df] or [1])
    hi = min([k for k in ks if k >= df] or [max(ks)])
    return T95[lo] + (T95[hi] - T95[lo]) * (df - lo) / max(1, (hi - lo))


def describe(v):
    n = len(v)
    if n < 2:
        return {"n": n, "mean": (v[0] if v else None), "sd": None, "ci": None,
                "ci_lo": None, "ci_hi": None, "total": sum(v)}
    m, sd = st.mean(v), st.stdev(v)
    se = sd / math.sqrt(n)
    h = tcrit(n - 1) * se
    return {"n": n, "mean": m, "sd": sd, "ci": h, "ci_lo": m - h, "ci_hi": m + h,
            "total": sum(v)}


def welch(a, b):
    if len(a) < 2 or len(b) < 2:
        return None
    ma, mb = st.mean(a), st.mean(b)
    va, vb = st.variance(a) / len(a), st.variance(b) / len(b)
    if va + vb == 0:
        return None
    t = (ma - mb) / math.sqrt(va + vb)
    df = (va + vb) ** 2 / (va**2 / (len(a) - 1) + vb**2 / (len(b) - 1))
    return {"t": t, "df": df, "diff": ma - mb, "crit": tcrit(int(round(df)))}


res = {}
for tag in ("W1", "W2"):
    for arm in ARMS:
        res[f"{arm}-{tag}"] = describe(pnls(arm, tag))

print(f"{'arm':4} {'win':3} {'n':>3} {'exp$/trade':>11} {'95% CI':>20} {'total$':>9}   verdict")
for tag in ("W1", "W2"):
    for arm in ARMS:
        s = res[f"{arm}-{tag}"]
        if s["ci"] is None:
            ci = f"{str(s['mean'])[:8]}" if s["mean"] is not None else "n/a"
            v = "n<2"
        else:
            ci = f"[{s['ci_lo']:7.2f},{s['ci_hi']:7.2f}]"
            v = "spans 0 -> noise" if s["ci_lo"] <= 0 <= s["ci_hi"] else "excludes 0"
        print(f"{arm:4} {tag:3} {s['n']:3} {s['mean'] if s['mean'] is not None else 0:11.2f} "
              f"{ci:>20} {s['total']:9.2f}   {v}")

print("\nWelch: control C vs each variant (per-trade $)")
for tag in ("W1", "W2"):
    for arm in ("P", "P2", "Y", "P3"):
        w = welch(pnls(arm, tag), pnls("C", tag))
        if w:
            sig = "SIGNIFICANT" if abs(w["t"]) > w["crit"] else "not significant"
            print(f"  {arm} vs C ({tag}): diff {w['diff']:7.2f}  t={w['t']:5.2f} (df {w['df']:.1f}, "
                  f"crit {w['crit']:.2f})  -> {sig}")

json.dump(res, open(AGD / "gate-probe-stats.json", "w"), indent=1)
print("\nwrote", AGD / "gate-probe-stats.json")
