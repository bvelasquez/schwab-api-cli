#!/usr/bin/env python3
"""Daily paper-trading scorecard for paper-host swing + options sim agents."""
from __future__ import annotations

import argparse
import collections
import datetime as dt
import json
import math
import random
import re
import statistics
import sys
import urllib.parse
import urllib.request
from pathlib import Path

SWING_STATE = "trader-state-trader-swing.json"
SWING_JOURNAL = "trader-journal-trader-swing.jsonl"
OPTIONS_STATE = "agent-sim-state-options-pilot.json"
OPTIONS_JOURNAL = "agent-sim-journal-options-pilot.jsonl"
SWING_STARTING_CASH = 4000.0
OPTIONS_STARTING_BUDGET = 4000.0
SWING_HALT_PCT = 12.0
OPTIONS_HALT_PCT = 10.0
BOOTSTRAP_SEED = 42
BOOTSTRAP_N = 2000


def parse_args():
    p = argparse.ArgumentParser(description="Daily swing + options scorecard")
    p.add_argument(
        "--repo",
        type=Path,
        default=Path.home() / "projects/schwabinvestbot",
        help="Repo root (default: ~/projects/schwabinvestbot)",
    )
    p.add_argument(
        "--out-dir",
        type=Path,
        default=None,
        help="Output directory (default: <repo>/rules/scorecards)",
    )
    p.add_argument("--telegram", action="store_true", help="Send compact summary via Telegram")
    p.add_argument("--json", action="store_true", help="Print full JSON to stdout")
    return p.parse_args()


def load_json(path: Path):
    if not path.is_file():
        return None
    return json.loads(path.read_text())


def swing_journal_paths(rules: Path) -> list[Path]:
    """Archived/rotated journals first, active journal last (chronological merge)."""
    active = rules / SWING_JOURNAL
    rotated = sorted(
        p
        for p in rules.glob(f"{SWING_JOURNAL}.*")
        if p.is_file()
    )
    archived = sorted(rules.glob(f"**/{SWING_JOURNAL}*"))
    ordered: list[Path] = []
    seen: set[Path] = set()
    for p in [*archived, *rotated]:
        if p == active or not p.is_file() or p in seen:
            continue
        seen.add(p)
        ordered.append(p)
    if active.is_file():
        ordered.append(active)
    return ordered


def iter_journal_lines(paths: list[Path]):
    for path in paths:
        with path.open("r", encoding="utf-8", errors="replace") as fh:
            for line in fh:
                line = line.strip()
                if line:
                    yield line


def session_date(ts: str) -> str:
    try:
        t = dt.datetime.fromisoformat(ts.replace("Z", "+00:00"))
        return t.astimezone(dt.timezone(dt.timedelta(hours=-4))).date().isoformat()
    except Exception:
        return ts[:10]


def reason_bucket(reason: str, reason_code: str | None) -> str:
    if reason_code:
        return str(reason_code)
    if not reason:
        return "unknown"
    base = re.sub(r"\d+(?:\.\d+)?", "", reason).strip()
    base = re.sub(r"\s+", " ", base)
    return base or reason


def stop_gap_slippage_pct(payload: dict) -> float | None:
    if payload.get("stop_gap_slippage_pct") is not None:
        return float(payload["stop_gap_slippage_pct"])
    entry = payload.get("entry_price")
    exit_p = payload.get("exit_price")
    stop = payload.get("stop_price")
    if not entry or not exit_p or not stop:
        return None
    entry, exit_p, stop = float(entry), float(exit_p), float(stop)
    if entry <= 0:
        return None
    return 100.0 * (exit_p - stop) / entry


def bootstrap_mean_ci(values: list[float], seed: int = BOOTSTRAP_SEED, n: int = BOOTSTRAP_N):
    if not values:
        return None, None, None
    mean = statistics.mean(values)
    if len(values) == 1:
        return mean, mean, mean
    rng = random.Random(seed)
    means = []
    for _ in range(n):
        sample = [values[rng.randrange(len(values))] for _ in range(len(values))]
        means.append(statistics.mean(sample))
    means.sort()
    lo = means[int(0.025 * len(means))]
    hi = means[int(0.975 * len(means)) - 1]
    return mean, lo, hi


def pct(x: float | None, digits: int = 2) -> str:
    if x is None or (isinstance(x, float) and math.isnan(x)):
        return "n/a"
    return f"{x:.{digits}f}%"


def usd(x: float | None, digits: int = 2) -> str:
    if x is None:
        return "n/a"
    return f"${x:,.{digits}f}"


def scan_swing_journals(paths: list[Path]):
    exits: list[dict] = []
    entries: list[dict] = []
    tick_by_day: dict[str, dict] = {}
    spy_first: float | None = None
    spy_last: float | None = None
    for line in iter_journal_lines(paths):
        if "sim_exit_filled" not in line and "sim_entry_filled" not in line and "sim_tick_summary" not in line:
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        typ = rec.get("type")
        ts = rec.get("ts", "")
        payload = rec.get("payload") or {}
        if typ == "sim_exit_filled":
            exits.append({"ts": ts, "session": session_date(ts), **payload})
        elif typ == "sim_entry_filled":
            entries.append({"ts": ts, "session": session_date(ts), **payload})
        elif typ == "sim_tick_summary":
            regime = payload.get("regime") or {}
            bench = regime.get("benchmark_last")
            if bench is not None:
                if spy_first is None:
                    spy_first = float(bench)
                spy_last = float(bench)
            day = session_date(ts)
            cap = payload.get("capital_check") or {}
            deployed = cap.get("equity_deployed_usd")
            dd = payload.get("drawdown") or {}
            rejected = (payload.get("scan") or {}).get("rejected") or []
            prev = tick_by_day.get(day) or {}
            tick_by_day[day] = {
                "deployed_usd": float(deployed) if deployed is not None else prev.get("deployed_usd"),
                "drawdown_pct": float(dd["drawdown_pct"]) if dd.get("drawdown_pct") is not None else prev.get("drawdown_pct"),
                "rejected": rejected,
                "ts": ts,
            }
    return exits, entries, tick_by_day, spy_first, spy_last


def swing_section(state_path: Path, journal_paths: list[Path]) -> dict:
    exits, entries, tick_by_day, spy_first, spy_last = scan_swing_journals(journal_paths)
    pnls_pct = [float(e["pnl_pct"]) for e in exits if e.get("pnl_pct") is not None]
    pnls_usd = [float(e["pnl_usd"]) for e in exits if e.get("pnl_usd") is not None]
    wins = [p for p in pnls_pct if p > 0]
    losses = [p for p in pnls_pct if p <= 0]
    gross_win = sum(p for p in pnls_usd if p > 0)
    gross_loss = abs(sum(p for p in pnls_usd if p < 0))
    profit_factor = (gross_win / gross_loss) if gross_loss > 0 else None
    mean_pct, ci_lo, ci_hi = bootstrap_mean_ci(pnls_pct)
    exit_mix = collections.Counter(e.get("exit_reason") or "unknown" for e in exits)
    last20 = pnls_pct[-20:]
    slippages = [
        s
        for e in exits
        if e.get("exit_reason") == "stop_loss" and (s := stop_gap_slippage_pct(e)) is not None
    ]

    state = load_json(state_path) or {}
    sim = state.get("sim") if isinstance(state.get("sim"), dict) else {}
    start_eq = float(sim.get("starting_cash_usd") or SWING_STARTING_CASH)
    snaps = sim.get("equity_snapshots") or []
    if snaps:
        now_eq = float(snaps[-1].get("equity_usd") or start_eq)
        peak_eq = max(float(s.get("equity_usd") or start_eq) for s in snaps)
    else:
        now_eq = start_eq
        peak_eq = start_eq
    sleeve_return_pct = 100.0 * (now_eq - start_eq) / start_eq if start_eq else None
    drawdown_pct = 100.0 * (peak_eq - now_eq) / peak_eq if peak_eq > 0 else 0.0

    spy_return_pct = None
    if spy_first and spy_last and spy_first > 0:
        spy_return_pct = 100.0 * (spy_last - spy_first) / spy_first

    deploy_pcts = [
        100.0 * float(v["deployed_usd"]) / SWING_STARTING_CASH
        for v in tick_by_day.values()
        if v.get("deployed_usd") is not None
    ]
    avg_deploy_pct = statistics.mean(deploy_pcts) if deploy_pcts else None
    exposure_spy_pct = (spy_return_pct * avg_deploy_pct / 100.0) if spy_return_pct is not None and avg_deploy_pct is not None else None
    excess_vs_spy = (sleeve_return_pct - spy_return_pct) if sleeve_return_pct is not None and spy_return_pct is not None else None
    excess_vs_exposure = (
        (sleeve_return_pct - exposure_spy_pct)
        if sleeve_return_pct is not None and exposure_spy_pct is not None
        else None
    )

    sessions_sorted = sorted(tick_by_day.keys())
    last5 = sessions_sorted[-5:]
    funnel: dict[str, collections.Counter] = {}
    for day in last5:
        funnel[day] = collections.Counter()
        for rej in tick_by_day[day].get("rejected") or []:
            funnel[day][reason_bucket(rej.get("reason", ""), rej.get("reason_code"))] += 1

    entry_sessions = {session_date(e["ts"]) for e in entries}
    sessions_since_entry = 0
    if sessions_sorted:
        if not entry_sessions:
            sessions_since_entry = len(sessions_sorted)
        else:
            for day in reversed(sessions_sorted):
                if day in entry_sessions:
                    break
                sessions_since_entry += 1

    profile_pnl: collections.Counter = collections.Counter()
    for e in exits:
        prof = e.get("active_profile") or "unknown"
        if e.get("pnl_usd") is not None:
            profile_pnl[prof] += float(e["pnl_usd"])

    last10_exits = exits[-10:]
    stop_share = None
    if last10_exits:
        stop_like = sum(
            1
            for e in last10_exits
            if "stop" in (e.get("exit_reason") or "").lower()
        )
        stop_share = 100.0 * stop_like / len(last10_exits)

    last10_sessions = sessions_sorted[-10:]
    last10_deploy = [
        100.0 * float(tick_by_day[d]["deployed_usd"]) / SWING_STARTING_CASH
        for d in last10_sessions
        if tick_by_day[d].get("deployed_usd") is not None
    ]
    avg_deploy_last10 = statistics.mean(last10_deploy) if last10_deploy else None

    return {
        "closed_trades": len(exits),
        "win_rate_pct": 100.0 * len(wins) / len(pnls_pct) if pnls_pct else None,
        "mean_pnl_pct": mean_pct,
        "mean_pnl_pct_ci95": [ci_lo, ci_hi] if mean_pct is not None else None,
        "profit_factor": profit_factor,
        "avg_win_pct": statistics.mean(wins) if wins else None,
        "avg_loss_pct": statistics.mean(losses) if losses else None,
        "exit_reason_mix": dict(exit_mix),
        "last_20_expectancy_pct": statistics.mean(last20) if last20 else None,
        "mean_stop_gap_slippage_pct": statistics.mean(slippages) if slippages else None,
        "equity_usd": {"start": start_eq, "now": now_eq, "return_pct": sleeve_return_pct},
        "drawdown_pct": drawdown_pct,
        "spy": {"first": spy_first, "last": spy_last, "return_pct": spy_return_pct},
        "avg_deployment_pct": avg_deploy_pct,
        "exposure_matched_spy_return_pct": exposure_spy_pct,
        "excess_return_vs_spy_pct": excess_vs_spy,
        "excess_return_vs_exposure_spy_pct": excess_vs_exposure,
        "rejection_funnel_last_5_sessions": {d: dict(funnel[d]) for d in last5},
        "sessions_since_last_entry": sessions_since_entry,
        "realized_usd_by_profile": dict(profile_pnl),
        "_alerts_inputs": {
            "sessions_since_entry": sessions_since_entry,
            "stop_share_last10_pct": stop_share,
            "avg_deploy_last10_pct": avg_deploy_last10,
            "drawdown_pct": drawdown_pct,
        },
    }


def scan_options_journal(path: Path):
    exits: list[dict] = []
    entries: list[dict] = []
    rolls: list[dict] = []
    if not path.is_file():
        return exits, entries, rolls
    with path.open("r", encoding="utf-8", errors="replace") as fh:
        for line in fh:
            if "sim_exit_filled" not in line and "sim_entry_filled" not in line and "defensive_roll" not in line:
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            typ = rec.get("type") or rec.get("kind")
            payload = rec.get("payload") or {}
            ts = rec.get("ts", "")
            row = {"ts": ts, **payload}
            if typ == "sim_exit_filled":
                exits.append(row)
            elif typ == "sim_entry_filled":
                entries.append(row)
            elif typ == "defensive_roll":
                rolls.append(row)
            elif typ == "sim_exit_relabeled":
                for ex in reversed(exits):
                    if (
                        ex.get("position_id") == payload.get("position_id")
                        and ex.get("exit_reason") == payload.get("old_reason")
                    ):
                        ex["exit_reason"] = payload.get("new_reason")
                        ex["roll_attempt"] = payload.get("roll_attempt")
                        break
    return exits, entries, rolls


def credit_width_ratio(entry: dict) -> float | None:
    credit = entry.get("entry_credit")
    params = entry.get("entry_params") or entry.get("signal", {}).get("entry_params") or {}
    if not params and entry.get("signal"):
        sig = entry["signal"]
        if isinstance(sig, dict):
            ctx = sig.get("market_context") or {}
            analytics = ctx.get("analytics") or {}
            cw = analytics.get("credit_to_width_pct")
            if cw is not None:
                return float(cw)
            credit = credit or analytics.get("credit")
            long_s = sig.get("long_strike")
            short_s = sig.get("short_strike")
            if credit and long_s is not None and short_s is not None:
                width = abs(float(short_s) - float(long_s))
                if width > 0:
                    return 100.0 * float(credit) / width
    long_s = params.get("long_strike")
    short_s = params.get("short_strike")
    if credit is None or long_s is None or short_s is None:
        return None
    width = abs(float(short_s) - float(long_s))
    if width <= 0:
        return None
    return 100.0 * float(credit) / width


def options_section(state_path: Path, journal_path: Path) -> dict:
    state = load_json(state_path) or {}
    sim = state.get("sim") if isinstance(state.get("sim"), dict) else {}
    realized = float(sim.get("realized_pnl_usd") or 0.0)
    start = float(sim.get("starting_budget_usd") or OPTIONS_STARTING_BUDGET)

    exits, entries, rolls = scan_options_journal(journal_path)
    wins = sum(1 for e in exits if float(e.get("pnl_usd") or 0) > 0)
    exit_mix = collections.Counter(e.get("exit_reason") or "unknown" for e in exits)

    credits = [float(e["entry_credit"]) for e in entries if e.get("entry_credit") is not None]
    cw_ratios = [r for e in entries if (r := credit_width_ratio(e)) is not None]

    win_pnls = [float(e["pnl_usd"]) for e in exits if float(e.get("pnl_usd") or 0) > 0]
    loss_pnls = [abs(float(e["pnl_usd"])) for e in exits if float(e.get("pnl_usd") or 0) < 0]
    breakeven = None
    breakeven_note = None
    if win_pnls and loss_pnls:
        aw = statistics.mean(win_pnls)
        al = statistics.mean(loss_pnls)
        breakeven = 100.0 * al / (aw + al)
    else:
        breakeven_note = "insufficient win/loss history"

    open_mark = 0.0
    for pos in (state.get("open_positions") or {}).values():
        credit = pos.get("entry_credit")
        debit = pos.get("last_good_debit_to_close")
        contracts = pos.get("contracts") or 0
        if credit is not None and debit is not None:
            open_mark += (float(credit) - float(debit)) * 100.0 * contracts

    degraded_exits = 0
    for e in exits:
        mark = e.get("mark") or {}
        if mark.get("quote_degraded") or mark.get("source") == "chain_degraded":
            degraded_exits += 1

    mislabeled_rolls: list[str] = []
    entry_times = [(e["ts"], e.get("underlying") or _underlying_from_position(e.get("position_id", ""))) for e in entries]
    for ex in exits:
        if ex.get("exit_reason") != "defensive_roll":
            continue
        ex_ts = ex["ts"]
        und = ex.get("underlying") or _underlying_from_position(ex.get("position_id", ""))
        found = False
        for ets, u in entry_times:
            if ets > ex_ts and (not und or u == und):
                found = True
                break
        if not found:
            mislabeled_rolls.append(ex.get("position_id") or ex_ts)

    equity = start + realized + open_mark
    peak_equity = start
    cum = start
    for e in sorted(exits, key=lambda x: x.get("ts", "")):
        cum += float(e.get("pnl_usd") or 0)
        peak_equity = max(peak_equity, cum)
    peak_equity = max(peak_equity, equity)
    drawdown_pct = 100.0 * (peak_equity - equity) / peak_equity if peak_equity > 0 else 0.0

    return {
        "closed_trades": len(exits),
        "wins": wins,
        "realized_pnl_usd": realized,
        "exit_reason_mix": dict(exit_mix),
        "avg_entry_credit": statistics.mean(credits) if credits else None,
        "avg_credit_to_width_pct": statistics.mean(cw_ratios) if cw_ratios else None,
        "breakeven_win_rate_pct": breakeven,
        "breakeven_win_rate_note": breakeven_note,
        "open_position_mark_usd": round(open_mark, 2),
        "equity_usd_est": round(equity, 2),
        "exits_on_degraded_quotes": degraded_exits,
        "defensive_roll_mislabels": mislabeled_rolls,
        "_alerts_inputs": {
            "avg_credit_width_pct": statistics.mean(cw_ratios) if cw_ratios else None,
            "drawdown_pct": drawdown_pct,
            "mislabeled_rolls": mislabeled_rolls,
        },
    }


def _underlying_from_position(pid: str) -> str | None:
    parts = pid.split("|")
    return parts[1] if len(parts) > 1 else None


def build_drift_alerts(swing: dict, options: dict) -> list[str]:
    alerts: list[str] = []
    si = swing.get("_alerts_inputs") or {}
    oi = options.get("_alerts_inputs") or {}

    sse = si.get("sessions_since_entry")
    if sse is not None and sse >= 5:
        alerts.append(f"swing: no entries in {sse} sessions (>=5)")

    stop_share = si.get("stop_share_last10_pct")
    if stop_share is not None and stop_share > 60:
        alerts.append(f"swing: stop exits {stop_share:.0f}% of last 10 closes (>60%)")

    deploy10 = si.get("avg_deploy_last10_pct")
    if deploy10 is not None and deploy10 < 20:
        alerts.append(f"swing: avg deployment {deploy10:.1f}% over last 10 sessions (<20%)")

    sdd = si.get("drawdown_pct")
    if sdd is not None and sdd > SWING_HALT_PCT / 2:
        alerts.append(f"swing: drawdown {sdd:.1f}% (> half of {SWING_HALT_PCT:.0f}% halt)")

    ocw = oi.get("avg_credit_width_pct")
    if ocw is not None and ocw < 10:
        alerts.append(f"options: avg credit/width {ocw:.1f}% (<10% edge warning)")

    odd = oi.get("drawdown_pct")
    if odd is not None and odd > OPTIONS_HALT_PCT / 2:
        alerts.append(f"options: drawdown {odd:.1f}% (> half of {OPTIONS_HALT_PCT:.0f}% halt)")

    for pid in oi.get("mislabeled_rolls") or []:
        alerts.append(f"options: defensive_roll with no follow-up entry ({pid})")

    return alerts


def compact_summary(report: dict) -> str:
    sw = report["swing"]
    op = report["options"]
    lines = [
        f"Scorecard {report['date']}",
        (
            f"Swing: {sw['closed_trades']} closes, WR {pct(sw.get('win_rate_pct'), 1)}, "
            f"mean pnl {pct(sw.get('mean_pnl_pct'), 2)} "
            f"CI [{pct((sw.get('mean_pnl_pct_ci95') or [None, None])[0], 2)}, "
            f"{pct((sw.get('mean_pnl_pct_ci95') or [None, None])[1], 2)}], "
            f"eq {usd(sw['equity_usd']['now'])} ({pct(sw['equity_usd'].get('return_pct'), 2)})"
        ),
        (
            f"  SPY {pct(sw['spy'].get('return_pct'), 2)} | deploy avg {pct(sw.get('avg_deployment_pct'), 1)} | "
            f"excess vs SPY {pct(sw.get('excess_return_vs_spy_pct'), 2)}"
        ),
        (
            f"Options: {op['closed_trades']} closes, {op['wins']}W, real {usd(op.get('realized_pnl_usd'))}, "
            f"open mark {usd(op.get('open_position_mark_usd'))}, degraded exits {op.get('exits_on_degraded_quotes')}"
        ),
    ]
    alerts = report.get("drift_alerts") or []
    if alerts:
        lines.append("Alerts: " + "; ".join(alerts[:5]))
        if len(alerts) > 5:
            lines.append(f"  (+{len(alerts) - 5} more)")
    else:
        lines.append("Alerts: none")
    return "\n".join(lines)


def send_telegram(text: str, env_file: Path | None):
    import os

    token = os.environ.get("TELEGRAM_BOT_TOKEN")
    chat = os.environ.get("TELEGRAM_CHAT_ID")
    if (not token or not chat) and env_file and env_file.is_file():
        for line in env_file.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, _, v = line.partition("=")
            k, v = k.strip(), v.strip().strip("'\"")
            if k == "TELEGRAM_BOT_TOKEN" and not token:
                token = v
            if k == "TELEGRAM_CHAT_ID" and not chat:
                chat = v
    if not token or not chat:
        print("telegram: skipped (TELEGRAM_BOT_TOKEN / TELEGRAM_CHAT_ID unset)", file=sys.stderr)
        return
    data = urllib.parse.urlencode({"chat_id": chat, "text": text}).encode()
    req = urllib.request.Request(
        f"https://api.telegram.org/bot{token}/sendMessage",
        data=data,
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            resp.read()
    except Exception as exc:
        print(f"telegram: send failed: {exc}", file=sys.stderr)


def main() -> int:
    args = parse_args()
    repo = args.repo.expanduser().resolve()
    rules = repo / "rules"
    out_dir = (args.out_dir or rules / "scorecards").expanduser()
    out_dir.mkdir(parents=True, exist_ok=True)
    today = dt.date.today().isoformat()

    journal_paths = swing_journal_paths(rules)
    swing = swing_section(rules / SWING_STATE, journal_paths)
    options = options_section(rules / OPTIONS_STATE, rules / OPTIONS_JOURNAL)
    drift_alerts = build_drift_alerts(swing, options)
    swing.pop("_alerts_inputs", None)
    options.pop("_alerts_inputs", None)

    report = {
        "date": today,
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "swing": swing,
        "options": options,
        "drift_alerts": drift_alerts,
    }

    out_path = out_dir / f"{today}.json"
    out_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")

    summary = compact_summary(report)
    print(summary)
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))

    if args.telegram:
        env_file = Path.home() / ".config/environment.d/schwab-paper.conf"
        send_telegram(summary, env_file)

    return 0


if __name__ == "__main__":
    sys.exit(main())
