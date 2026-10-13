#!/usr/bin/env python3
"""Calibrate paper-fill slippage against recorded option chains.

Reads rules/chains/YYYY-MM-DD/<SYM>-<HHMM>ET.json.gz (written by chain-snapshot.sh) and
measures, for 12-20 delta $5-wide vertical credit spreads, how much a realistic fill costs
relative to the midpoint. Then compares that with what the paper agents actually booked.

Read-only: writes rules/analysis/slippage-calibration.json and prints a summary. It never
edits a rules file. `simulation.fill_slippage_pct` changes need the operator's OK.

Cost model: a fill lands a fraction `f` of the way from the midpoint to the natural price
(sell at bid, buy at ask). f = 0.5 and 0.66 are reported (0.66 is the convention used in
public realistic-fill studies, e.g. OptionKrafter).

Usage:
  python3 scripts/calibrate_slippage.py [--repo PATH] [--width 5] [--out PATH]
"""
from __future__ import annotations

import argparse
import datetime as dt
import gzip
import json
import re
import statistics
import sys
from pathlib import Path

FRACTIONS = (0.5, 0.66)
DELTA_MIN, DELTA_MAX = 0.12, 0.20
DTE_BUCKETS = {"21-30": (21, 30), "32-45": (32, 45), "55-70": (55, 70)}
PROFIT_TARGET = 0.5  # exit debit ~ (1 - target) * credit at a 50% profit target
MIN_MID_CREDIT = 0.10
LEG_RE = re.compile(r"([PC])(\d+(?:\.\d+)?)L_[PC](\d+(?:\.\d+)?)S")
SNAP_RE = re.compile(r"^(?P<sym>[A-Z]+)-(?P<hhmm>\d{4})ET\.json\.gz$")


def num(value) -> float | None:
    try:
        out = float(value)
    except (TypeError, ValueError):
        return None
    return None if out != out else out


def bucket_for(dte: int) -> str | None:
    for name, (lo, hi) in DTE_BUCKETS.items():
        if lo <= dte <= hi:
            return name
    return None


def leg(map_for_expiry: dict, strike: float) -> dict | None:
    for key in (f"{strike:.1f}", f"{strike:g}", f"{strike:.2f}"):
        rows = map_for_expiry.get(key)
        if rows:
            return rows[0]
    return None


def quote(contract: dict) -> tuple[float, float] | None:
    bid, ask = num(contract.get("bid")), num(contract.get("ask"))
    if bid is None or ask is None or bid <= 0 or ask <= 0 or ask < bid:
        return None
    return bid, ask


def spread_metrics(short_c: dict, long_c: dict) -> dict | None:
    """Credit-spread quote metrics, or None when either leg lacks a two-sided quote."""
    sq, lq = quote(short_c), quote(long_c)
    if not sq or not lq:
        return None
    mid_credit = (sq[0] + sq[1]) / 2 - (lq[0] + lq[1]) / 2
    natural_credit = sq[0] - lq[1]
    if mid_credit < MIN_MID_CREDIT:
        return None
    spread_sum = mid_credit - natural_credit  # half-spread of both legs
    return {
        "mid_credit": mid_credit,
        "natural_credit": natural_credit,
        "spread_sum": spread_sum,
    }


def enumerate_spreads(doc: dict, width: float):
    """Yield (dte_bucket, side, metrics, short_strike, expiry) for qualifying verticals."""
    data = doc.get("data") or {}
    for side, map_name in (("put", "putExpDateMap"), ("call", "callExpDateMap")):
        for exp_key, strikes in (data.get(map_name) or {}).items():
            exp_str, _, dte_str = exp_key.partition(":")
            try:
                dte = int(dte_str)
            except ValueError:
                continue
            bucket = bucket_for(dte)
            if bucket is None:
                continue
            for strike_str, rows in strikes.items():
                if not rows:
                    continue
                short_c = rows[0]
                delta = num(short_c.get("delta"))
                if delta is None or not (DELTA_MIN <= abs(delta) <= DELTA_MAX):
                    continue
                strike = float(strike_str)
                long_strike = strike - width if side == "put" else strike + width
                long_c = leg(strikes, long_strike)
                if not long_c:
                    continue
                metrics = spread_metrics(short_c, long_c)
                if metrics:
                    yield bucket, side, metrics, strike, exp_str


def cost_rows(metrics: dict) -> dict:
    out = {}
    for f in FRACTIONS:
        entry_cost = f * metrics["spread_sum"]
        exit_debit = (1 - PROFIT_TARGET) * metrics["mid_credit"]
        out[f] = {
            "entry_pct": 100.0 * entry_cost / metrics["mid_credit"],
            "exit_pct": 100.0 * entry_cost / exit_debit if exit_debit > 0 else None,
            "round_trip_usd": 2 * entry_cost * 100.0,
        }
    return out


def percentile(values: list[float], q: float) -> float:
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, int(round(q * (len(ordered) - 1)))))
    return ordered[idx]


def summarize(values: list[float]) -> dict:
    return {
        "median": statistics.median(values),
        "p25": percentile(values, 0.25),
        "p75": percentile(values, 0.75),
    }


def scan_snapshots(chains_dir: Path, width: float):
    samples: dict[tuple[str, str], dict] = {}
    days: set[str] = set()
    n_files = 0
    index: dict[tuple[str, str], list[tuple[str, Path]]] = {}
    for day_dir in sorted(p for p in chains_dir.iterdir() if p.is_dir()):
        for f in sorted(day_dir.glob("*.json.gz")):
            m = SNAP_RE.match(f.name)
            if not m:
                continue
            try:
                doc = json.load(gzip.open(f, "rt"))
            except (OSError, json.JSONDecodeError):
                continue
            n_files += 1
            days.add(day_dir.name)
            index.setdefault((m["sym"], day_dir.name), []).append((m["hhmm"], f))
            for bucket, _side, metrics, _strike, _exp in enumerate_spreads(doc, width):
                slot = samples.setdefault(
                    (m["sym"], bucket),
                    {"spread_sum": [], "credit": [], **{f"e{f}": [] for f in FRACTIONS},
                     **{f"x{f}": [] for f in FRACTIONS}, **{f"r{f}": [] for f in FRACTIONS}},
                )
                slot["spread_sum"].append(metrics["spread_sum"])
                slot["credit"].append(metrics["mid_credit"])
                for f, row in cost_rows(metrics).items():
                    slot[f"e{f}"].append(row["entry_pct"])
                    if row["exit_pct"] is not None:
                        slot[f"x{f}"].append(row["exit_pct"])
                    slot[f"r{f}"].append(row["round_trip_usd"])
    return samples, sorted(days), n_files, index


def paper_entries(rules: Path):
    for jp in sorted(rules.glob("agent-sim-journal-options-*.jsonl")):
        agent = jp.stem.removeprefix("agent-sim-journal-options-")
        with jp.open(encoding="utf-8", errors="replace") as fh:
            for line in fh:
                if "sim_entry_filled" not in line:
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if rec.get("type") != "sim_entry_filled":
                    continue
                payload = rec.get("payload") or {}
                yield agent, rec.get("ts", ""), payload


def compare_paper(rules: Path, index: dict, width: float) -> list[dict]:
    rows = []
    seen: set[tuple] = set()
    for agent, ts, payload in paper_entries(rules):
        pid = payload.get("position_id") or ""
        parts = pid.split("|")
        m = LEG_RE.search(parts[-1]) if parts else None
        if not m or len(parts) < 3 or payload.get("entry_credit") is None:
            continue
        kind, long_strike, short_strike = m.group(1), float(m.group(2)), float(m.group(3))
        symbol, expiry = parts[1], parts[2]
        day = ts[:10]
        key = (agent, pid, day)
        if key in seen:
            continue
        # Entries journaled after 2026-10-12 carry the leg quotes the engine priced from.
        # Those are exact, so prefer them to the time-misaligned snapshot lookup below.
        quotes = fill_time_quotes(payload)
        if quotes:
            seen.add(key)
            rows.append(paper_row(agent, pid, float(payload["entry_credit"]), quotes["mid_credit"], quotes["natural_credit"],
                                  f"{ts} (fill-time quotes)", quotes.get("estimated_credit")))
            continue
        snaps = index.get((symbol, day))
        if not snaps:
            continue
        seen.add(key)
        # nearest snapshot at or after the fill is not guaranteed; use the earliest of the day
        hhmm, path = snaps[0]
        try:
            doc = json.load(gzip.open(path, "rt"))
        except (OSError, json.JSONDecodeError):
            continue
        map_name = "putExpDateMap" if kind == "P" else "callExpDateMap"
        exp_map = None
        for exp_key, strikes in (doc.get("data") or {}).get(map_name, {}).items():
            if exp_key.startswith(expiry):
                exp_map = strikes
                break
        if exp_map is None:
            continue
        short_c, long_c = leg(exp_map, short_strike), leg(exp_map, long_strike)
        if not short_c or not long_c:
            continue
        metrics = spread_metrics(short_c, long_c)
        if not metrics:
            continue
        rows.append(paper_row(agent, pid, float(payload["entry_credit"]), metrics["mid_credit"],
                              metrics["natural_credit"], f"{day} {hhmm}ET (nearest snapshot, not fill time)"))
    return rows


def fill_time_quotes(payload: dict) -> dict | None:
    """Mid and natural credit recorded with the entry signal, or None for older entries."""
    ctx = (payload.get("signal") or {}).get("market_context") or {}
    q = ctx.get("leg_quotes") or {}
    mid, nat = num(q.get("mid_credit")), num(q.get("natural_credit"))
    if mid is None or nat is None or mid <= 0:
        return None
    return {"mid_credit": mid, "natural_credit": nat, "estimated_credit": num(payload.get("signal", {}).get("estimated_credit"))}


def paper_row(agent: str, pid: str, paper: float, mid: float, natural: float, when: str, estimated: float | None = None) -> dict:
    """Compare a paper fill credit with the quoted mid and natural credit."""
    denom = mid - natural
    implied_f = (mid - paper) / denom if denom > 0 else None
    row = {
        "agent": agent,
        "position_id": pid,
        "paper_entry_credit": round(paper, 4),
        "quotes_from": when,
        "quoted_mid_credit": round(mid, 4),
        "quoted_natural_credit": round(natural, 4),
        "paper_cost_vs_mid_pct": round(100.0 * (mid - paper) / mid, 2),
        "implied_fraction_of_spread": None if implied_f is None else round(implied_f, 3),
    }
    if estimated is not None:
        row["engine_estimated_credit"] = round(estimated, 4)
    return row


def current_setting(rules: Path) -> float | None:
    path = rules / "options-pilot-8709.yaml"
    if not path.is_file():
        return None
    m = re.search(r"^\s*fill_slippage_pct:\s*([0-9.]+)", path.read_text(), re.MULTILINE)
    return float(m.group(1)) if m else None


def build_report(repo: Path, width: float) -> dict:
    rules = repo / "rules"
    chains = rules / "chains"
    if not chains.is_dir():
        raise SystemExit(f"no chain snapshots at {chains}")
    samples, days, n_files, index = scan_snapshots(chains, width)
    buckets = {}
    for (symbol, bucket), slot in sorted(samples.items()):
        entry = {"n": len(slot["spread_sum"])}
        entry["spread_sum_usd_per_share"] = summarize(slot["spread_sum"])
        entry["mid_credit"] = summarize(slot["credit"])
        for f in FRACTIONS:
            entry[f"f{f}"] = {
                "entry_cost_pct_of_credit": summarize(slot[f"e{f}"]),
                "exit_cost_pct_of_debit_at_50pct_target": summarize(slot[f"x{f}"]) if slot[f"x{f}"] else None,
                "round_trip_usd_per_contract": summarize(slot[f"r{f}"]),
            }
        buckets.setdefault(symbol, {})[bucket] = entry

    core = [b for (sym, bucket), b in samples.items() if bucket == "32-45"]
    rec_entry = rec_exit = None
    if core:
        all_e = [v for b in core for v in b["e0.5"]]
        all_x = [v for b in core for v in b["x0.5"]]
        rec_entry = round(statistics.median(all_e), 1) if all_e else None
        rec_exit = round(statistics.median(all_x), 1) if all_x else None
    compare = compare_paper(rules, index, width)
    current = current_setting(rules)
    verdict = None
    if current is not None and rec_entry is not None:
        verdict = (
            "current setting looks too light" if current < rec_entry * 0.75
            else "current setting is in range" if current <= rec_entry * 1.5
            else "current setting looks heavy"
        )
    return {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "snapshots": {"files": n_files, "days": days, "first": days[0] if days else None, "last": days[-1] if days else None},
        "spread_filter": {"short_abs_delta": [DELTA_MIN, DELTA_MAX], "width": width, "dte_buckets": DTE_BUCKETS},
        "model": {
            "fractions_of_spread_crossed": list(FRACTIONS),
            "note": "fill = mid -/+ f * (half spread of both legs); exit cost shown at a 50% profit-target debit",
        },
        "by_symbol_and_dte": buckets,
        "current_8709_fill_slippage_pct": current,
        "recommended": {
            "entry_fill_slippage_pct_at_f0.5": rec_entry,
            "exit_fill_slippage_pct_at_f0.5": rec_exit,
            "verdict": verdict,
            "note": "sim.rs applies one percentage to entry credit and exit debit; exits are costlier in percent because the debit is smaller. Applying a change to 8709 needs the operator's OK.",
        },
        "paper_fills_vs_quotes": compare,
    }


def print_summary(report: dict) -> None:
    snaps = report["snapshots"]
    print(f"Slippage calibration: {snaps['files']} snapshots over {len(snaps['days'])} days ({snaps['first']} to {snaps['last']})")
    for symbol, by_bucket in report["by_symbol_and_dte"].items():
        for bucket, entry in by_bucket.items():
            f5 = entry["f0.5"]["entry_cost_pct_of_credit"]["median"]
            f66 = entry["f0.66"]["entry_cost_pct_of_credit"]["median"]
            rt = entry["f0.66"]["round_trip_usd_per_contract"]["median"]
            print(f"  {symbol} {bucket:>5} DTE: n={entry['n']:>5}  entry cost {f5:5.1f}% (f=.5) {f66:5.1f}% (f=.66) of credit; round trip ${rt:.2f}/contract at f=.66")
    rec = report["recommended"]
    print(f"8709 setting: {report['current_8709_fill_slippage_pct']}%  recommended entry {rec['entry_fill_slippage_pct_at_f0.5']}% / exit {rec['exit_fill_slippage_pct_at_f0.5']}%  -> {rec['verdict']}")
    for row in report["paper_fills_vs_quotes"][:12]:
        print(f"  paper {row['agent']}: filled {row['paper_entry_credit']} vs mid {row['quoted_mid_credit']} / natural {row['quoted_natural_credit']} (cost vs mid {row['paper_cost_vs_mid_pct']}%, implied f {row['implied_fraction_of_spread']})")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo", type=Path, default=Path.home() / "projects/schwabinvestbot")
    ap.add_argument("--width", type=float, default=5.0)
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()
    repo = args.repo.expanduser().resolve()
    report = build_report(repo, args.width)
    out = args.out or repo / "rules" / "analysis" / "slippage-calibration.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_name(out.name + ".tmp")
    tmp.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    tmp.replace(out)
    print_summary(report)
    return 0


if __name__ == "__main__":
    sys.exit(main())
