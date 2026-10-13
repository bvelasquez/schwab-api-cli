#!/usr/bin/env python3
"""Benchmarks and return metrics for the options paper agents.

Answers three questions the raw P&L cannot:
  1. How much of each agent's P&L is just market direction? (delta-equivalent P&L and
     beta-adjusted alpha, first-order: net position delta x underlying move over the hold)
  2. What is the return on the capital actually at risk, and on the whole IRA?
  3. What would a plain cash-secured put-write on SPY have done over the same snapshots,
     after paying a realistic fill and charging the collateral at a T-bill rate?

Read-only. Inputs: rules/agent-sim-state-options-*.json, matching journals, and the
recorded chains in rules/chains. Output: rules/benchmarks/latest.json.

Manual constants (labelled as such in the output; set via env):
  BENCH_IRA_EQUITY_USD   total IRA equity for return-on-IRA (default 637000)
  BENCH_TBILL_PCT        annual T-bill rate for the collateral charge (default 4.0)
  BENCH_FILL_FRACTION    fraction of the half-spread crossed on put-write fills (default 0.66)
"""
from __future__ import annotations

import argparse
import datetime as dt
import gzip
import json
import os
import re
import sys
from pathlib import Path
from zoneinfo import ZoneInfo

ET = ZoneInfo("America/New_York")
SNAP_RE = re.compile(r"^(?P<sym>[A-Z]+)-(?P<hhmm>\d{4})ET\.json\.gz$")
PUTWRITE_VARIANTS = {"put25d": 0.25, "putatm": 0.50}
PUTWRITE_DTE = (30, 45)


def env_float(name: str, default: float) -> float:
    try:
        return float(os.environ.get(name, default))
    except ValueError:
        return default


def num(value) -> float | None:
    try:
        out = float(value)
    except (TypeError, ValueError):
        return None
    return None if out != out else out


def parse_ts(ts: str) -> dt.datetime | None:
    try:
        t = dt.datetime.fromisoformat(ts.replace("Z", "+00:00"))
    except ValueError:
        return None
    if t.tzinfo is None:
        t = t.replace(tzinfo=dt.timezone.utc)
    return t.astimezone(ET)


def load_snapshots(chains_dir: Path, symbols: tuple[str, ...]):
    """Chronological list of (et_datetime, symbol, doc)."""
    out = []
    if not chains_dir.is_dir():
        return out
    for day_dir in sorted(p for p in chains_dir.iterdir() if p.is_dir()):
        for f in sorted(day_dir.glob("*.json.gz")):
            m = SNAP_RE.match(f.name)
            if not m or m["sym"] not in symbols:
                continue
            try:
                doc = json.load(gzip.open(f, "rt"))
            except (OSError, json.JSONDecodeError):
                continue
            when = dt.datetime.strptime(f"{day_dir.name} {m['hhmm']}", "%Y-%m-%d %H%M").replace(tzinfo=ET)
            out.append((when, m["sym"], doc))
    out.sort(key=lambda r: r[0])
    return out


def price_series(snapshots) -> dict[str, list[tuple[dt.datetime, float]]]:
    series: dict[str, list[tuple[dt.datetime, float]]] = {}
    for when, sym, doc in snapshots:
        price = num((doc.get("data") or {}).get("underlyingPrice"))
        if price:
            series.setdefault(sym, []).append((when, price))
    return series


def price_at(series: list[tuple[dt.datetime, float]], when: dt.datetime) -> float | None:
    """First recorded price at or after `when`; the last price if `when` is past the data."""
    if not series:
        return None
    for t, p in series:
        if t >= when:
            return p
    return series[-1][1]


def net_delta_shares(entry_payload: dict) -> float | None:
    """Net delta of a credit vertical in shares (short leg sold, long leg bought)."""
    analytics = (((entry_payload.get("signal") or {}).get("market_context") or {}).get("analytics")) or {}
    contracts = num(entry_payload.get("contracts")) or 1.0
    short_d = num(analytics.get("short_delta"))
    long_d = num(analytics.get("long_delta"))
    if short_d is None or long_d is None:
        return None
    return (-short_d + long_d) * 100.0 * contracts


def read_entries(journal: Path) -> dict[str, list[dict]]:
    by_pid: dict[str, list[dict]] = {}
    if not journal.is_file():
        return by_pid
    with journal.open(encoding="utf-8", errors="replace") as fh:
        for line in fh:
            if "sim_entry_filled" not in line:
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            if rec.get("type") != "sim_entry_filled":
                continue
            payload = dict(rec.get("payload") or {})
            payload["_ts"] = rec.get("ts", "")
            by_pid.setdefault(payload.get("position_id") or "", []).append(payload)
    return by_pid


def agent_metrics(state_path: Path, journal_path: Path, series: dict, ira_equity: float) -> dict:
    state = json.loads(state_path.read_text())
    sim = state.get("sim") or {}
    entries = read_entries(journal_path)
    trades = []
    for t in sim.get("closed_trades") or []:
        pid = t.get("position_id") or ""
        cands = entries.get(pid) or []
        entry = min(cands, key=lambda e: abs((parse_ts(e["_ts"]) or dt.datetime.min.replace(tzinfo=ET)) - (parse_ts(t.get("opened_at", "")) or dt.datetime.min.replace(tzinfo=ET)))) if cands else None
        trades.append(_trade_row(t, entry, series, closed=True))
    for pid, pos in (state.get("open_positions") or {}).items():
        cands = entries.get(pid) or []
        entry = cands[-1] if cands else None
        credit, debit = num(pos.get("entry_credit")), num(pos.get("last_good_debit_to_close"))
        contracts = num(pos.get("contracts")) or 1.0
        mark = (credit - debit) * 100.0 * contracts if credit is not None and debit is not None else 0.0
        row = {
            "underlying": pos.get("underlying"),
            "position_id": pid,
            "opened_at": pos.get("opened_at"),
            "pnl_usd": round(mark, 2),
            "max_loss_usd": num(pos.get("max_loss_usd")),
        }
        row.update(_delta_fields(row, entry, series, closed_at=None))
        row["open"] = True
        trades.append(row)

    realized = sum(r["pnl_usd"] for r in trades if not r.get("open"))
    open_mark = sum(r["pnl_usd"] for r in trades if r.get("open"))
    total = realized + open_mark
    risks = [r["max_loss_usd"] for r in trades if r.get("max_loss_usd")]
    avg_risk = sum(risks) / len(risks) if risks else None
    delta_pnl = [r["delta_equiv_pnl_usd"] for r in trades if r.get("delta_equiv_pnl_usd") is not None]
    covered = [r for r in trades if r.get("delta_equiv_pnl_usd") is not None]
    return {
        "closed_trades": sum(1 for r in trades if not r.get("open")),
        "open_positions": sum(1 for r in trades if r.get("open")),
        "realized_usd": round(realized, 2),
        "open_mark_usd": round(open_mark, 2),
        "total_usd": round(total, 2),
        "avg_max_risk_usd": None if avg_risk is None else round(avg_risk, 2),
        "return_on_max_risk_pct": None if not avg_risk else round(100.0 * total / avg_risk, 3),
        "return_on_ira_pct": round(100.0 * total / ira_equity, 4) if ira_equity else None,
        "delta_equiv_pnl_usd": round(sum(delta_pnl), 2) if delta_pnl else None,
        "beta_adjusted_alpha_usd": round(sum(r["pnl_usd"] for r in covered) - sum(delta_pnl), 2) if covered else None,
        "positions_with_delta_data": len(covered),
        "trades": trades,
    }


def _delta_fields(row: dict, entry: dict | None, series: dict, closed_at: dt.datetime | None) -> dict:
    out = {"net_delta_shares": None, "delta_equiv_pnl_usd": None}
    if not entry:
        return out
    nd = net_delta_shares(entry)
    sym = row.get("underlying")
    opened = parse_ts(row.get("opened_at") or entry.get("_ts") or "")
    if nd is None or not sym or not opened or sym not in series:
        return out
    s0 = price_at(series[sym], opened)
    s1 = price_at(series[sym], closed_at) if closed_at else series[sym][-1][1]
    if s0 is None or s1 is None:
        return out
    out["net_delta_shares"] = round(nd, 3)
    out["delta_equiv_pnl_usd"] = round(nd * (s1 - s0), 2)
    return out


def _trade_row(t: dict, entry: dict | None, series: dict, closed: bool) -> dict:
    closed_at = parse_ts(t.get("closed_at") or "")
    row = {
        "underlying": t.get("underlying"),
        "position_id": t.get("position_id"),
        "opened_at": t.get("opened_at"),
        "closed_at": t.get("closed_at"),
        "exit_reason": t.get("exit_reason"),
        "pnl_usd": round(float(t.get("pnl_usd") or 0.0), 2),
        "max_loss_usd": num((entry or {}).get("max_loss_usd")),
    }
    row.update(_delta_fields(row, entry, series, closed_at))
    return row


def best_put(doc: dict, target_abs_delta: float, dte_range: tuple[int, int]):
    """Put nearest the target |delta| in the DTE window: (expiry, dte, strike, contract)."""
    best = None
    for exp_key, strikes in ((doc.get("data") or {}).get("putExpDateMap") or {}).items():
        exp, _, dte_s = exp_key.partition(":")
        try:
            dte = int(dte_s)
        except ValueError:
            continue
        if not dte_range[0] <= dte <= dte_range[1]:
            continue
        for strike, rows in strikes.items():
            if not rows:
                continue
            c = rows[0]
            d = num(c.get("delta"))
            bid, ask = num(c.get("bid")), num(c.get("ask"))
            if d is None or not bid or not ask or bid <= 0 or ask < bid:
                continue
            gap = abs(abs(d) - target_abs_delta)
            if best is None or gap < best[0]:
                best = (gap, exp, dte, float(strike), c)
    return None if best is None else best[1:]


def find_put(doc: dict, expiry: str, strike: float) -> dict | None:
    for exp_key, strikes in ((doc.get("data") or {}).get("putExpDateMap") or {}).items():
        if exp_key.startswith(expiry):
            for key in (f"{strike:.1f}", f"{strike:g}"):
                if strikes.get(key):
                    return strikes[key][0]
    return None


def simulate_putwrite(snapshots, symbol: str, target_delta: float, fill_fraction: float, tbill_pct: float) -> dict:
    """One short put at a time, entered at the first snapshot of a day, held to expiry."""
    ledger: list[dict] = []
    pos: dict | None = None
    last_day: dt.date | None = None
    last_price: dict[dt.date, float] = {}
    for when, sym, doc in snapshots:
        if sym != symbol:
            continue
        day = when.date()
        price = num((doc.get("data") or {}).get("underlyingPrice"))
        if price:
            last_price[day] = price
        first_of_day = day != last_day
        last_day = day
        if pos and day > pos["expiry_date"]:
            # Settle only after the expiry day is over, from its last recorded price
            # (or the latest earlier price if that day has no snapshots).
            settle_day = pos["expiry_date"]
            prior = [d for d in last_price if d <= settle_day]
            settle_px = last_price[max(prior)] if prior else None
            if settle_px:
                pnl = (pos["credit"] - max(0.0, pos["strike"] - settle_px)) * 100.0
                pos.update(status="settled", settle_price=settle_px, pnl_usd=round(pnl, 2), closed=settle_day.isoformat())
                ledger.append(pos)
                pos = None
        if pos is None and first_of_day:
            pick = best_put(doc, target_delta, PUTWRITE_DTE)
            if pick:
                expiry, dte, strike, c = pick
                bid, ask = num(c["bid"]), num(c["ask"])
                mid = (bid + ask) / 2
                credit = mid - fill_fraction * (mid - bid)
                pos = {
                    "opened": day.isoformat(),
                    "expiry": expiry,
                    "expiry_date": dt.date.fromisoformat(expiry),
                    "strike": strike,
                    "dte": dte,
                    "entry_delta": num(c.get("delta")),
                    "credit": round(credit, 4),
                    "collateral_usd": strike * 100.0,
                    "status": "open",
                }
        if pos and pos["status"] == "open":
            c = find_put(doc, pos["expiry"], pos["strike"])
            if c:
                bid, ask = num(c.get("bid")), num(c.get("ask"))
                if bid is not None and ask is not None and ask > 0:
                    mid = (bid + ask) / 2
                    buyback = mid + fill_fraction * (ask - mid)
                    pos["last_mark_debit"] = round(buyback, 4)
                    pos["mtm_usd"] = round((pos["credit"] - buyback) * 100.0, 2)
                    pos["mark_time"] = when.isoformat()
    rows = [dict(r, expiry_date=r["expiry_date"].isoformat()) for r in ledger]
    realized = sum(r["pnl_usd"] for r in rows)
    open_row = None
    mtm = 0.0
    if pos:
        open_row = dict(pos, expiry_date=pos["expiry_date"].isoformat())
        mtm = pos.get("mtm_usd", 0.0)
    all_rows = rows + ([open_row] if open_row else [])
    collateral = sum(r["collateral_usd"] for r in all_rows) / len(all_rows) if all_rows else None
    held_days = sum(
        (dt.date.fromisoformat(r.get("closed") or r["expiry_date"]) - dt.date.fromisoformat(r["opened"])).days
        if r["status"] == "settled"
        else max(0, (dt.date.fromisoformat(open_row["mark_time"][:10]) - dt.date.fromisoformat(r["opened"])).days if r.get("mark_time") else 0)
        for r in all_rows
    )
    total = realized + mtm
    tbill = (collateral * tbill_pct / 100.0 * held_days / 365.0) if collateral else None
    return {
        "symbol": symbol,
        "target_abs_delta": target_delta,
        "fill_fraction": fill_fraction,
        "cycles_settled": len(rows),
        "open_cycle": open_row,
        "realized_usd": round(realized, 2),
        "open_mtm_usd": round(mtm, 2),
        "total_usd": round(total, 2),
        "avg_collateral_usd": None if collateral is None else round(collateral, 2),
        "return_on_collateral_pct": None if not collateral else round(100.0 * total / collateral, 3),
        "tbill_on_collateral_usd": None if tbill is None else round(tbill, 2),
        "excess_over_tbill_usd": None if tbill is None else round(total - tbill, 2),
        "days_held": held_days,
        "ledger": rows,
    }


def build_report(repo: Path) -> dict:
    rules = repo / "rules"
    ira = env_float("BENCH_IRA_EQUITY_USD", 637000.0)
    tbill = env_float("BENCH_TBILL_PCT", 4.0)
    fill = env_float("BENCH_FILL_FRACTION", 0.66)
    snaps = load_snapshots(rules / "chains", ("SPY", "QQQ"))
    series = price_series(snaps)
    agents = {}
    for sp in sorted(rules.glob("agent-sim-state-options-*.json")):
        name = sp.stem.removeprefix("agent-sim-state-options-")
        jp = rules / f"agent-sim-journal-options-{name}.jsonl"
        try:
            agents[name] = agent_metrics(sp, jp, series, ira)
        except (OSError, json.JSONDecodeError) as exc:
            agents[name] = {"error": str(exc)}
    underlying = {}
    for sym, rows in series.items():
        first, last = rows[0][1], rows[-1][1]
        underlying[sym] = {
            "first": first,
            "last": last,
            "return_pct": round(100.0 * (last - first) / first, 3),
            "first_time": rows[0][0].isoformat(),
            "last_time": rows[-1][0].isoformat(),
        }
    putwrite = {
        name: simulate_putwrite(snaps, "SPY", delta, fill, tbill) for name, delta in PUTWRITE_VARIANTS.items()
    }
    return {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "manual_constants": {
            "ira_equity_usd": ira,
            "tbill_pct": tbill,
            "putwrite_fill_fraction": fill,
            "note": "manually set via BENCH_* env vars; not read from the brokerage",
        },
        "snapshot_range": {
            "first": snaps[0][0].isoformat() if snaps else None,
            "last": snaps[-1][0].isoformat() if snaps else None,
            "count": len(snaps),
        },
        "underlying": underlying,
        "agents": agents,
        "putwrite": putwrite,
        "caveats": [
            "delta-equivalent P&L is first-order (net delta at entry x underlying move); gamma and vega are in the alpha term",
            "put-write enters once per cycle at the first snapshot of a day and settles from the last recorded price on expiry day, once a later day appears in the data",
            "only snapshots since 2026-09-28 exist, so cycle counts are tiny; read these as a harness, not a result",
        ],
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo", type=Path, default=Path.home() / "projects/schwabinvestbot")
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()
    repo = args.repo.expanduser().resolve()
    report = build_report(repo)
    out = args.out or repo / "rules" / "benchmarks" / "latest.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_name(out.name + ".tmp")
    tmp.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    tmp.replace(out)
    for name, a in report["agents"].items():
        if "error" in a:
            print(f"{name}: error {a['error']}")
            continue
        print(
            f"{name}: total ${a['total_usd']:.2f} (realized {a['realized_usd']:.2f}, open {a['open_mark_usd']:.2f}) "
            f"ret/max-risk {a['return_on_max_risk_pct']}% ret/IRA {a['return_on_ira_pct']}% "
            f"delta-equiv {a['delta_equiv_pnl_usd']} alpha {a['beta_adjusted_alpha_usd']}"
        )
    for name, p in report["putwrite"].items():
        print(f"putwrite {name}: cycles {p['cycles_settled']} total ${p['total_usd']} ret/collateral {p['return_on_collateral_pct']}% excess over T-bill {p['excess_over_tbill_usd']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
