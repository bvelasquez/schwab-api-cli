#!/usr/bin/env python3
"""Open/close/audit accounting epochs for a trading book.

Examples
--------
    scripts/epoch.py status --book swing-9947
    scripts/epoch.py open --book swing-9947 --rules-file trader-swing-9947.yaml \
        --strategy-version pullback-v2 --close-reason "strategy change: adaptive sizing"
    scripts/epoch.py close --book swing-9947 --reason "strategy change"
    scripts/epoch.py import-archive --book swing-9947 \
        --archive rules/archive/swing-reset-2026-10-23 --id swing-9947-legacy

A change to the rules file does NOT automatically move money or metrics: it
only marks the active epoch as drifted, and the scorecard/gate refuse to treat
drifted results as comparable until you close and open deliberately.
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import epochs  # noqa: E402


def _now() -> dt.datetime:
    return dt.datetime.now(dt.timezone.utc)


def cmd_status(args) -> int:
    status = epochs.epoch_status(args.rules_dir, args.book)
    print(json.dumps(status, indent=2, sort_keys=True))
    if not status["ledger_present"]:
        print("epoch: no ledger yet", file=sys.stderr)
        return 2
    if status["active"] is None:
        print("epoch: no active epoch — current metrics are undefined", file=sys.stderr)
        return 2
    if status["drift"]:
        print(
            "epoch: DRIFT — rules file changed since this epoch opened. "
            "Close the epoch and open a new one before comparing results.",
            file=sys.stderr,
        )
        return 3
    return 0


def cmd_open(args) -> int:
    rules_dir = args.rules_dir
    ledger = epochs.load_ledger(rules_dir)
    entry = epochs.book_entry(ledger, args.book)
    started = epochs.parse_ts(args.started_at) if args.started_at else _now()
    if started is None:
        print(f"bad --started-at {args.started_at!r}", file=sys.stderr)
        return 2

    rules_path = (rules_dir / args.rules_file) if args.rules_file else None
    if rules_path is None or not rules_path.is_file():
        print("--rules-file must name an existing file under the rules dir", file=sys.stderr)
        return 2

    if entry.get("active"):
        if not args.close_reason:
            print(
                f"book {args.book} already has active epoch {entry['active']['id']}; "
                "pass --close-reason to close it first",
                file=sys.stderr,
            )
            return 2
        _close(rules_dir, ledger, entry, args.close_reason, ended_at=started)

    epoch = {
        "id": args.id or f"{args.book}-{started.strftime('%Y%m%dT%H%M')}",
        "started_at": epochs.iso(started),
        "ended_at": None,
        "start_equity_usd": float(args.start_equity),
        "strategy_version": args.strategy_version,
        "strategy_fingerprint": epochs.strategy_fingerprint(rules_path),
        "opened_by": args.by,
        "note": args.note or "",
        "summary": {},
    }
    entry["book"] = args.book
    entry["rules_file"] = args.rules_file
    entry["active"] = epoch
    epochs.save_ledger(rules_dir, ledger)
    epochs.write_lifetime(rules_dir, args.book)
    print(json.dumps(epoch, indent=2, sort_keys=True))
    return 0


def _close(rules_dir: Path, ledger: dict, entry: dict, reason: str, ended_at=None) -> dict:
    active = entry.get("active") or {}
    summary = active.get("summary") or {}
    try:  # best-effort: reuse the scorecard's own view of the epoch
        import scorecard  # noqa: WPS433

        epoch_copy = dict(active)
        epoch_copy["ended_at"] = epochs.iso(ended_at or _now())
        journal = scorecard.swing_journal_paths(rules_dir, epoch_copy)
        section = scorecard.swing_section(rules_dir / scorecard.SWING_STATE, journal, epoch_copy)
        summary = {
            "closed_trades": section.get("closed_trades"),
            "realized_usd": (section.get("equity_usd") or {}).get("now", 0.0)
            - (section.get("equity_usd") or {}).get("start", 0.0),
            "win_rate_pct": section.get("win_rate_pct"),
            "profit_factor": section.get("profit_factor"),
            "return_pct": (section.get("equity_usd") or {}).get("return_pct"),
        }
    except Exception as exc:  # noqa: BLE001 - closing must not hinge on metrics
        print(f"epoch: summary unavailable ({exc})", file=sys.stderr)
    closed = dict(active)
    closed["ended_at"] = epochs.iso(ended_at or _now())
    closed["close_reason"] = reason
    closed["summary"] = summary
    entry.setdefault("closed", []).append(closed)
    entry["active"] = None
    return closed


def cmd_close(args) -> int:
    rules_dir = args.rules_dir
    ledger = epochs.load_ledger(rules_dir)
    entry = epochs.book_entry(ledger, args.book)
    if not entry.get("active"):
        print(f"book {args.book} has no active epoch", file=sys.stderr)
        return 2
    closed = _close(rules_dir, ledger, entry, args.reason)
    epochs.save_ledger(rules_dir, ledger)
    epochs.write_lifetime(rules_dir, args.book)
    print(json.dumps(closed, indent=2, sort_keys=True))
    return 0


def cmd_import_archive(args) -> int:
    """Record a pre-reset history block as a CLOSED epoch (never current)."""
    journal = args.archive / f"trader-journal-{args.book}.jsonl"
    if not journal.is_file():
        candidates = sorted(args.archive.glob("trader-journal-*.jsonl"))
        if not candidates:
            print(f"no journal found under {args.archive}", file=sys.stderr)
            return 2
        journal = candidates[0]
    exits, entries, first, last = [], [], None, None
    for line in journal.read_text(errors="replace").splitlines():
        if "sim_entry_filled" not in line and "sim_exit_filled" not in line:
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        stamp = epochs.parse_ts(rec.get("ts"))
        if stamp is not None:
            first = stamp if first is None or stamp < first else first
            last = stamp if last is None or stamp > last else last
        typ = rec.get("type")
        payload = rec.get("payload") or {}
        if typ == "sim_exit_filled":
            exits.append(payload)
        elif typ == "sim_entry_filled":
            entries.append(payload)
    pnls = [float(e["pnl_usd"]) for e in exits if e.get("pnl_usd") is not None]
    pcts = [float(e["pnl_pct"]) for e in exits if e.get("pnl_pct") is not None]
    summary = {
        "closed_trades": len(exits),
        "entries": len(entries),
        "realized_usd": round(sum(pnls), 2),
        "win_rate_pct": round(100.0 * sum(1 for p in pcts if p > 0) / len(pcts), 2) if pcts else None,
    }
    rules_dir = args.rules_dir
    ledger = epochs.load_ledger(rules_dir)
    entry = epochs.book_entry(ledger, args.book)
    entry["book"] = args.book
    if args.rules_file:
        entry["rules_file"] = args.rules_file
    entry.setdefault("closed", []).append(
        {
            "id": args.id,
            "started_at": epochs.iso(first) if first else None,
            "ended_at": epochs.iso(last) if last else None,
            "strategy_version": "pre-reset",
            "strategy_fingerprint": None,
            "opened_by": "import",
            "note": f"imported from {journal}",
            "close_reason": args.reason,
            "summary": summary,
        }
    )
    epochs.save_ledger(rules_dir, ledger)
    epochs.write_lifetime(rules_dir, args.book)
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--rules-dir", type=Path, default=Path("rules"))
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("status")
    p.add_argument("--book", required=True)
    p.set_defaults(func=cmd_status)

    p = sub.add_parser("open")
    p.add_argument("--book", required=True)
    p.add_argument("--rules-file", required=True)
    p.add_argument("--strategy-version", required=True)
    p.add_argument("--start-equity", type=float, required=True)
    p.add_argument("--started-at", default=None)
    p.add_argument("--id", default=None)
    p.add_argument("--note", default="")
    p.add_argument("--close-reason", default=None)
    p.add_argument("--by", default="operator")
    p.set_defaults(func=cmd_open)

    p = sub.add_parser("close")
    p.add_argument("--book", required=True)
    p.add_argument("--reason", required=True)
    p.set_defaults(func=cmd_close)

    p = sub.add_parser("import-archive")
    p.add_argument("--book", required=True)
    p.add_argument("--archive", type=Path, required=True)
    p.add_argument("--id", required=True)
    p.add_argument("--reason", default="reset")
    p.add_argument("--rules-file", default=None)
    p.set_defaults(func=cmd_import_archive)

    args = ap.parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
