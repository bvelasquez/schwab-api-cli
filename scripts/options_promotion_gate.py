#!/usr/bin/env python3
"""Promotion gate for the options paper arms (O1..On) versus the control arm (O0).

An arm earns a "pass" only when all of these hold:
  - at least MIN_TRADING_DAYS weekdays between its first entry and its last journal event
  - at least MIN_DISTINCT_TRADES closed trades that the control did not also take
    (two arms opening the same spread on the same day are one bet, not two)
  - expectancy per closed trade has a bootstrap 95% CI above zero
  - difference in expectancy versus the control has a bootstrap 95% CI above zero
  - profit factor at least MIN_PROFIT_FACTOR
  - max drawdown of realized P/L no worse than the control's
  - the Benjamini-Hochberg adjusted p-value across all tested arms is at most BH_Q

Data shortfalls report "insufficient"; unmet criteria with enough data report "fail".
This script writes a JSON verdict and stops. It never edits rules and never places
orders. Applying a pass needs the operator's OK, and real money is a separate decision.

Output: <repo>/rules/analysis/options-promotion-gate.json (atomic write).
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import random
import statistics
import sys
from pathlib import Path

CONTROL = "arm-o0"
TEST_PREFIX = "arm-"
MIN_TRADING_DAYS = 40
MIN_DISTINCT_TRADES = 20
MIN_PROFIT_FACTOR = 1.2
BH_Q = 0.10
BOOTSTRAP_N = 4000
SEED = 20261012
JOURNAL_PREFIX = "agent-sim-journal-options-"

NOTE = (
    "A pass is not a production change. Operator OK is required, and real money "
    "is a separate decision after paper proof."
)


def trade_key(position_id: str, ts: str | None = None) -> tuple[str, str, str]:
    """(underlying, entry day, legs) parsed from the position id.

    Position ids look like `HASH|QQQ|2026-11-20|vertical|P699L_P704S`; the entry day
    is not in the id, so callers pass the entry timestamp when they have it.
    """
    parts = (position_id or "").split("|")
    und = parts[1] if len(parts) > 1 else ""
    legs = parts[-1] if parts else ""
    return (und, (ts or "")[:10], legs)


def read_journal(path: Path) -> dict:
    """Entries and exits for one agent from its sim journal."""
    entries: dict[str, dict] = {}
    exits: list[dict] = []
    last_ts = ""
    if not path.is_file():
        return {"entries": entries, "exits": exits, "last_ts": last_ts}
    with path.open("r", encoding="utf-8", errors="replace") as fh:
        for line in fh:
            if "sim_exit_filled" not in line and "sim_entry_filled" not in line:
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            typ = rec.get("type") or rec.get("kind")
            payload = rec.get("payload") or {}
            ts = rec.get("ts", "") or ""
            last_ts = max(last_ts, ts)
            pid = payload.get("position_id")
            if not pid:
                continue
            if typ == "sim_entry_filled":
                entries[pid] = {"ts": ts, "position_id": pid}
            elif typ == "sim_exit_filled" and payload.get("pnl_usd") is not None:
                exits.append({"ts": ts, "position_id": pid, "pnl_usd": float(payload["pnl_usd"])})
    return {"entries": entries, "exits": exits, "last_ts": last_ts}


def weekdays_between(start: str, end: str) -> int:
    """Weekdays from start date to end date inclusive (ISO strings; empty gives 0)."""
    try:
        a = dt.date.fromisoformat(start[:10])
        b = dt.date.fromisoformat(end[:10])
    except ValueError:
        return 0
    if b < a:
        return 0
    n = 0
    d = a
    while d <= b:
        if d.weekday() < 5:
            n += 1
        d += dt.timedelta(days=1)
    return n


def profit_factor(pnls: list[float]) -> float | None:
    wins = sum(p for p in pnls if p > 0)
    losses = -sum(p for p in pnls if p < 0)
    if losses == 0:
        return None if wins == 0 else float("inf")
    return wins / losses


def max_drawdown_usd(pnls: list[float]) -> float:
    """Worst peak-to-trough drop of cumulative realized P/L (a non-negative number)."""
    cum = peak = worst = 0.0
    for p in pnls:
        cum += p
        peak = max(peak, cum)
        worst = max(worst, peak - cum)
    return worst


def bootstrap_mean_ci(values: list[float], rng: random.Random, n: int = BOOTSTRAP_N):
    if not values:
        return None, None, None
    mean = statistics.fmean(values)
    if len(values) == 1:
        return mean, mean, mean
    k = len(values)
    means = sorted(statistics.fmean(rng.choices(values, k=k)) for _ in range(n))
    return mean, means[int(0.025 * n)], means[int(0.975 * n) - 1]


def bootstrap_diff(a: list[float], b: list[float], rng: random.Random, n: int = BOOTSTRAP_N):
    """Difference in means (a - b) with a 95% CI and a one-sided p for a <= b."""
    if not a or not b:
        return None, None, None, None
    diffs = sorted(
        statistics.fmean(rng.choices(a, k=len(a))) - statistics.fmean(rng.choices(b, k=len(b)))
        for _ in range(n)
    )
    point = statistics.fmean(a) - statistics.fmean(b)
    p = sum(1 for d in diffs if d <= 0) / n
    return point, diffs[int(0.025 * n)], diffs[int(0.975 * n) - 1], p


def benjamini_hochberg(pvals: dict[str, float], total: int) -> dict[str, float]:
    """BH adjusted p-values. `total` is the number of tests including any without a p-value."""
    items = sorted(pvals.items(), key=lambda kv: kv[1])
    m = max(total, len(items))
    adj: dict[str, float] = {}
    running = 1.0
    for rank in range(len(items), 0, -1):
        name, p = items[rank - 1]
        running = min(running, p * m / rank)
        adj[name] = min(1.0, running)
    return adj


def agent_name(journal: Path) -> str:
    return journal.name.removeprefix(JOURNAL_PREFIX).removesuffix(".jsonl")


def discover(rules: Path) -> dict[str, dict]:
    agents: dict[str, dict] = {}
    for jp in sorted(rules.glob(f"{JOURNAL_PREFIX}*.jsonl")):
        if ".bak" in jp.name:
            continue
        agents[agent_name(jp)] = read_journal(jp)
    return agents


def evaluate(agents: dict[str, dict], control: str = CONTROL, seed: int = SEED) -> dict:
    rng = random.Random(seed)
    ctrl = agents.get(control)
    ctrl_pnls = [e["pnl_usd"] for e in ctrl["exits"]] if ctrl else []
    ctrl_keys = set()
    if ctrl:
        for e in ctrl["exits"]:
            ent = ctrl["entries"].get(e["position_id"])
            ctrl_keys.add(trade_key(e["position_id"], ent["ts"] if ent else None))

    rows: dict[str, dict] = {}
    pvals: dict[str, float] = {}
    tested = [n for n in agents if n.startswith(TEST_PREFIX) and n != control]

    for name, data in agents.items():
        pnls = [e["pnl_usd"] for e in data["exits"]]
        keys = []
        for e in data["exits"]:
            ent = data["entries"].get(e["position_id"])
            keys.append(trade_key(e["position_id"], ent["ts"] if ent else None))
        unique_pnls = [p for p, k in zip(pnls, keys) if k not in ctrl_keys] if name != control else pnls
        first_entry = min((v["ts"] for v in data["entries"].values()), default="")
        days = weekdays_between(first_entry, data["last_ts"]) if first_entry else 0
        mean, lo, hi = bootstrap_mean_ci(pnls, rng)
        pf = profit_factor(pnls)
        row = {
            "n_closed": len(pnls),
            "n_distinct": len(unique_pnls),
            "trading_days": days,
            "expectancy_usd": None if mean is None else round(mean, 2),
            "ci95_usd": None if lo is None else [round(lo, 2), round(hi, 2)],
            "profit_factor": None if pf is None else (round(pf, 2) if pf != float("inf") else "inf"),
            "max_drawdown_usd": round(max_drawdown_usd(pnls), 2),
            "realized_usd": round(sum(pnls), 2),
        }
        if name == control:
            row["verdict"] = "control"
            row["failed"] = []
        elif name not in tested:
            row["verdict"] = "reference"
            row["failed"] = []
        else:
            diff, dlo, dhi, p = bootstrap_diff(unique_pnls, ctrl_pnls, rng)
            row["diff_vs_control_usd"] = None if diff is None else round(diff, 2)
            row["diff_ci95_usd"] = None if dlo is None else [round(dlo, 2), round(dhi, 2)]
            row["p_one_sided"] = p
            if p is not None:
                pvals[name] = p
            row["_pf"] = pf
        rows[name] = row

    adjusted = benjamini_hochberg(pvals, len(tested))
    ctrl_dd = rows[control]["max_drawdown_usd"] if control in rows else None

    for name in tested:
        row = rows[name]
        pf = row.pop("_pf", None)
        shortfalls: list[str] = []
        unmet: list[str] = []
        if row["trading_days"] < MIN_TRADING_DAYS:
            shortfalls.append(f"{row['trading_days']} of {MIN_TRADING_DAYS} trading days")
        if row["n_distinct"] < MIN_DISTINCT_TRADES:
            shortfalls.append(f"{row['n_distinct']} of {MIN_DISTINCT_TRADES} distinct trades")
        if control not in agents:
            shortfalls.append("no control journal")
        row["bh_adjusted_p"] = None if name not in adjusted else round(adjusted[name], 4)
        ci = row["ci95_usd"]
        if ci is None or ci[0] <= 0:
            unmet.append("expectancy CI not above zero")
        dci = row.get("diff_ci95_usd")
        if dci is None or dci[0] <= 0:
            unmet.append("not better than control (CI)")
        if pf is None or (pf != float("inf") and pf < MIN_PROFIT_FACTOR):
            unmet.append(f"profit factor below {MIN_PROFIT_FACTOR}")
        if ctrl_dd is not None and row["max_drawdown_usd"] > ctrl_dd:
            unmet.append("drawdown worse than control")
        if row["bh_adjusted_p"] is None or row["bh_adjusted_p"] > BH_Q:
            unmet.append(f"BH-adjusted p above {BH_Q}")
        if shortfalls:
            row["verdict"] = "insufficient"
            row["failed"] = shortfalls
        elif unmet:
            row["verdict"] = "fail"
            row["failed"] = unmet
        else:
            row["verdict"] = "pass"
            row["failed"] = []
    return rows


def build_report(rules: Path) -> dict:
    agents = discover(rules)
    rows = evaluate(agents)
    return {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "control": CONTROL,
        "criteria": {
            "min_trading_days": MIN_TRADING_DAYS,
            "min_distinct_trades": MIN_DISTINCT_TRADES,
            "min_profit_factor": MIN_PROFIT_FACTOR,
            "bh_q": BH_Q,
            "bootstrap_n": BOOTSTRAP_N,
        },
        "arms": rows,
        "note": NOTE,
    }


def write_atomic(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(text)
    tmp.replace(path)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo", type=Path, default=Path.home() / "projects/schwabinvestbot")
    ap.add_argument("--out", type=Path, default=None)
    ap.add_argument("--stdout", action="store_true", help="print the report instead of writing it")
    args = ap.parse_args(argv)
    rules = args.repo / "rules"
    report = build_report(rules)
    text = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.stdout:
        sys.stdout.write(text)
        return 0
    out = args.out or rules / "analysis" / "options-promotion-gate.json"
    write_atomic(out, text)
    for name, row in report["arms"].items():
        extra = "; ".join(row["failed"]) if row["failed"] else ""
        print(f"{name:12} {row['verdict']:12} closed {row['n_closed']:3} distinct {row['n_distinct']:3} {extra}")
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
