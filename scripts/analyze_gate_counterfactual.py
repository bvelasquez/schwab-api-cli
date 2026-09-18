#!/usr/bin/env python3
"""What the live swing bot's own per-tick decision grid implied, read back as evidence.

The agent journals every tick's `scan` block: `candidates` (setups that passed the entry
gate) and `rejected` (everything else, with a reason string). That grid is a natural
experiment nobody has read yet:

  * ADMITTED          - passed the gate. 71 distinct symbol-days over the window.
  * PORTFOLIO_BLOCKED - passed the gate but could not be taken (already held, symbol-group
                        cap reached, no capital). The counterfactual the bot pays for.
  * GATE_REJECTED     - filtered by the entry gate itself (trend / RSI band / RS / distance
                        from high). This is the gate doing its job; included for contrast.
  * BLOCKED_SYMBOL    - symbols suppressed by a separate mechanism (mechanism to confirm).

Rows are deduplicated to ONE observation per (symbol, day) - the same candidate reappears on
every tick it stays true, so the raw row count (169k) is not a sample size. Precedence:
ADMITTED > any rejection.

Forward returns are measured from the decision-minute price recorded in the row
(`technical_context.last`, i.e. what the bot actually saw) to the close N trading days later
from the daily cache. CIs are t-based AND symbol-clustered bootstrap, because overlapping
horizons and repeated symbols violate independence.

Read-only. Outputs markdown + JSON to an analysis directory.
"""
import argparse
import collections
import datetime
import json
import math
import random
import re
import statistics
import sys

GATE_PATTERNS = [
    ("gate:trend_below_sma", r"below sma"),
    ("gate:trend_above_sma", r"above sma"),
    ("gate:rsi_band", r"rsi .*outside range"),
    ("gate:dist_from_high", r"52w high|from 52w"),
    ("gate:relative_strength", r"relative strength|\brs\b"),
    ("gate:spread", r"spread"),
    ("gate:volume", r"relative volume|volume"),
    ("gate:earnings", r"earnings"),
    ("gate:range_cap", r"range"),
]
PORTFOLIO_PATTERNS = [
    ("portfolio:already_open", r"already_open"),
    ("portfolio:group_cap", r"symbol_group_cap"),
    ("portfolio:max_positions", r"max_position"),
    ("portfolio:entries_per_day", r"max_new_entries|entries_per_day|trades_per_day"),
    ("portfolio:capital", r"capital|budget|heat|insufficient|free cash"),
]
SPECIAL_PATTERNS = [
    ("blocked_symbol", r"blocked_symbol"),
    ("llm_veto", r"llm"),
]


def classify(reason):
    r = reason.lower()
    for name, pat in SPECIAL_PATTERNS + GATE_PATTERNS + PORTFOLIO_PATTERNS:
        if re.search(pat, r):
            return name
    return f"other:{reason[:40]}"


def load_obs(journal_path):
    """Deduplicate the per-tick decision grid to one observation per (symbol, day)."""
    obs = {}
    taxonomy = collections.Counter()
    unmatched = collections.Counter()

    def key(sym, day):
        return (sym, day)

    for line in open(journal_path):
        line = line.strip()
        if not line:
            continue
        try:
            d = json.loads(line)
        except Exception:
            continue
        if d.get("type") != "sim_tick_summary":
            continue
        day = d["ts"][:10]
        sc = (d.get("payload") or {}).get("scan") or {}

        for c in (sc.get("candidates") or []):
            sym = c.get("symbol")
            if not sym:
                continue
            ctx = c.get("technical_context") or {}
            prev = obs.get(key(sym, day))
            # ADMITTED wins over any rejection seen earlier the same day
            if prev and prev["cls"] == "ADMITTED":
                continue
            obs[key(sym, day)] = {
                "symbol": sym, "day": day, "cls": "ADMITTED", "reason": "passed gate",
                "ts": d["ts"], "price": ctx.get("last"), "ask": ctx.get("ask"),
                "rsi": ctx.get("rsi_14"), "score": c.get("adjusted_score"),
                "pct_from_52w_high": (ctx.get("history_features") or {}).get("pct_from_52w_high"),
            }

        for r in (sc.get("rejected") or []):
            sym = r.get("symbol")
            if not sym:
                continue
            raw = str(r.get("reason") or "?")
            cls = classify(raw)
            taxonomy[cls] += 1
            if cls.startswith("other:"):
                unmatched[raw] += 1
            if key(sym, day) in obs:
                continue
            ctx = r.get("technical_context") or {}
            obs[key(sym, day)] = {
                "symbol": sym, "day": day, "cls": cls, "reason": raw, "ts": d["ts"],
                "price": ctx.get("last"), "ask": ctx.get("ask"),
                "rsi": ctx.get("rsi_14"), "score": None,
                "pct_from_52w_high": (ctx.get("history_features") or {}).get("pct_from_52w_high"),
            }
    return obs, taxonomy, unmatched


def load_daily(cache_path):
    d = json.load(open(cache_path))
    out = {}
    for sym, bars in (d.get("symbols") or {}).items():
        rows = []
        for b in bars:
            dt = datetime.datetime.fromtimestamp(b["datetime_ms"] / 1000, datetime.UTC).date()
            rows.append((dt, b["close"]))
        rows.sort()
        out[sym.upper()] = rows
    return out, d


def fwd_return(bars, day_str, p0, horizon):
    """Close-to-close return from p0 to the close `horizon` trading days after day_str."""
    if not bars or not p0 or p0 <= 0:
        return None
    day = datetime.date.fromisoformat(day_str)
    idx = None
    for i, (dt, _) in enumerate(bars):
        if dt >= day:
            idx = i
            break
    if idx is None or idx + horizon >= len(bars):
        return None
    return bars[idx + horizon][1] / p0 - 1.0


def t_ci(xs):
    n = len(xs)
    if n < 2:
        return (None, None)
    m = statistics.mean(xs)
    sd = statistics.stdev(xs)
    se = sd / math.sqrt(n)
    t = 1.96 if n > 120 else {1: 12.7, 2: 4.3, 3: 3.18, 4: 2.78, 5: 2.57, 10: 2.23, 20: 2.09,
                              30: 2.04, 60: 2.0, 120: 1.98}.get(n, 2.0)
    return (m - t * se, m + t * se)


def clustered_ci(rows_by_symbol, horizon, iters=4000, seed=7):
    """Bootstrap resampling SYMBOLS, not rows - repeated/overlapping obs are not independent."""
    syms = [s for s, rs in rows_by_symbol.items() if rs]
    if len(syms) < 3:
        return (None, None)
    rng = random.Random(seed)
    means = []
    for _ in range(iters):
        pick = [rng.choice(syms) for _ in syms]
        vals = [v for s in pick for v in rows_by_symbol[s]]
        if vals:
            means.append(statistics.mean(vals))
    if not means:
        return (None, None)
    means.sort()
    lo = means[int(0.025 * len(means))]
    hi = means[int(0.975 * len(means)) - 1]
    return (lo, hi)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--journal", default="rules/trader-journal-trader-swing-9947.jsonl")
    ap.add_argument("--cache", default="rules/.backtest-cache-trader-swing-9947.json")
    ap.add_argument("--outdir", default="rules/analysis-20260918")
    ap.add_argument("--horizons", default="1,3,5,10,20")
    args = ap.parse_args()

    horizons = [int(h) for h in args.horizons.split(",")]
    obs, taxonomy, unmatched = load_obs(args.journal)
    daily, cache_meta = load_daily(args.cache)

    by_cls = collections.Counter(o["cls"] for o in obs.values())
    print(f"journal: {args.journal}")
    print(f"cache:   fetched_at={cache_meta.get('fetched_at')} symbols={len(daily)}")
    print(f"distinct (symbol, day) observations: {len(obs)}")
    print()
    print("=== class counts (deduplicated) ===")
    for k, v in by_cls.most_common():
        print(f"  {v:5d}  {k}")
    if unmatched:
        print()
        print("=== UNCLASSIFIED reason strings (need mapping) ===")
        for k, v in unmatched.most_common(15):
            print(f"  {v:6d}  {k}")

    results = {}
    print()
    print("=== forward return from the decision-minute price, by class ===")
    for cls in sorted(by_cls, key=lambda c: -by_cls[c]):
        rows = [o for o in obs.values() if o["cls"] == cls]
        entry = {}
        for h in horizons:
            vals, per_sym = [], collections.defaultdict(list)
            for o in rows:
                bars = daily.get(o["symbol"].upper())
                r = fwd_return(bars, o["day"], o["price"], h)
                if r is None:
                    continue
                vals.append(r)
                per_sym[o["symbol"]].append(r)
            if not vals:
                entry[h] = {"n": 0}
                continue
            lo, hi = t_ci(vals)
            clo, chi = clustered_ci(per_sym, h)
            entry[h] = {
                "n": len(vals), "mean_pct": 100 * statistics.mean(vals),
                "median_pct": 100 * statistics.median(vals),
                "win_rate_pct": 100 * sum(1 for v in vals if v > 0) / len(vals),
                "ci95_t_pct": [100 * lo, 100 * hi] if lo is not None else None,
                "ci95_symbol_clustered_pct": [100 * clo, 100 * chi] if clo is not None else None,
                "symbols": len(per_sym),
            }
        results[cls] = entry
        print(f"\n  {cls}  (n={len(rows)} observations, {len(set(o['symbol'] for o in rows))} symbols)")
        hdr = "    horizon   n    mean%   median%  win%   95% CI (t)            95% CI (symbol-clustered)"
        print(hdr)
        for h in horizons:
            e = entry[h]
            if not e.get("n"):
                print(f"    +{h:<3d}d    -    (no observations with full horizon)")
                continue
            ci = e["ci95_t_pct"]
            cc = e["ci95_symbol_clustered_pct"]
            print(f"    +{h:<3d}d {e['n']:5d}  {e['mean_pct']:+7.2f}  {e['median_pct']:+7.2f}  "
                  f"{e['win_rate_pct']:5.1f}  [{ci[0]:+6.2f}, {ci[1]:+6.2f}]"
                  + (f"   [{cc[0]:+6.2f}, {cc[1]:+6.2f}]" if cc else ""))

    summary = {
        "journal": args.journal, "cache": args.cache,
        "cache_fetched_at": cache_meta.get("fetched_at"),
        "cache_from": cache_meta.get("from"), "cache_to": cache_meta.get("to"),
        "observations": len(obs), "class_counts": dict(by_cls),
        "horizons": horizons, "results": results,
        "taxonomy_raw_rows": dict(taxonomy),
    }
    return summary, obs


if __name__ == "__main__":
    summary, obs = main()
    outdir = [a.split("=", 1)[1] for a in sys.argv if a.startswith("--outdir=")]
    outdir = outdir[0] if outdir else "rules/analysis-20260918"
    import os
    os.makedirs(outdir, exist_ok=True)
    path = os.path.join(outdir, "gate-counterfactual.json")
    with open(path, "w") as f:
        json.dump(summary, f, indent=2)
    print(f"\nwrote {path}")