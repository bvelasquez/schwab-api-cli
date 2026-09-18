import json, datetime as dt, collections, statistics as st

P = "rules/analysis-20260918/.backtest-cache-aligned.json"
d = json.load(open(P))
syms = d.get("symbols") or d

def bars(s):
    v = syms[s]
    return v.get("bars") if isinstance(v, dict) else v

rows = []
for s in sorted(syms):
    b = bars(s) or []
    ds = []
    for x in b:
        t = x.get("datetime_ms") if isinstance(x, dict) else None
        if t:
            ds.append(dt.datetime.utcfromtimestamp(t / 1000).date())
    ds.sort()
    rows.append((s, len(b), ds[0] if ds else None, ds[-1] if ds else None, ds))

print(f"cache fetched_at={d.get('fetched_at')} from={d.get('from')} to={d.get('to')} symbols={len(syms)}")
print(f"{'sym':6} {'bars':>5} {'first':>12} {'last':>12}")
for s, n, f, l, _ in rows[:60]:
    print(f"{s:6} {n:5} {str(f):>12} {str(l):>12}")

counts = [n for _, n, _, _, _ in rows]
firsts = collections.Counter(str(f) for _, _, f, _, _ in rows)
print(f"\nbar_count: min={min(counts)} median={int(st.median(counts))} max={max(counts)}")
print(f"symbols with >=480 bars (full 2y): {sum(1 for n in counts if n >= 480)}/{len(rows)}")
print("first-bar date histogram:")
for k, v in sorted(firsts.items()):
    print(f"   {k}: {v}")

# gaps: trading days are ~253/yr; a symbol covering the window should have ~500 bars
short = [r for r in rows if r[1] < 480]
print(f"\nSHORT-HISTORY SYMBOLS ({len(short)}):")
for s, n, f, l, _ in short[:40]:
    print(f"   {s:6} {n:5} {f} -> {l}")

# gap detection on the longest series: consecutive-bar gaps > 5 calendar days
print("\nGAPS >5 calendar days (top offenders):")
gapreport = []
for s, n, f, l, ds in rows:
    g = [(ds[i - 1], ds[i], (ds[i] - ds[i - 1]).days) for i in range(1, len(ds)) if (ds[i] - ds[i - 1]).days > 5]
    if g:
        gapreport.append((len(g), s, g[:3]))
gapreport.sort(reverse=True)
for c, s, g in gapreport[:12]:
    print(f"   {s:6} {c:3} gaps  e.g. {g}")
if not gapreport:
    print("   none")
