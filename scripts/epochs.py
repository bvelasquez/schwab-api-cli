#!/usr/bin/env python3
"""Epoch accounting: current results never mix with lifetime results.

An *epoch* is one book running one strategy version. Rules:

  - The active epoch begins at ``started_at``. Events stamped before it, or
    after ``ended_at`` for a closed epoch, are NOT current results and must
    never enter a current-window metric.
  - Any functional change to the rules file changes ``strategy_fingerprint``.
    ``epoch_status`` reports that as drift; a drifted epoch has to be closed
    and a new one opened before its results can be compared again.
  - Lifetime is the sum of CLOSED epochs, stored in ``rules/lifetime/<book>.json``.
    Current views never read it. It exists for the record, not for decisions.

The ledger lives in ``rules/epochs.json`` and is plain JSON so a human can
read and audit it.
"""
from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
from pathlib import Path
from typing import Any, Iterable

SCHEMA = 1
LEDGER_NAME = "epochs.json"
LIFETIME_DIR = "lifetime"


def ledger_path(rules_dir: Path) -> Path:
    return Path(rules_dir) / LEDGER_NAME


def load_ledger(rules_dir: Path) -> dict:
    path = ledger_path(rules_dir)
    if not path.is_file():
        return {"schema": SCHEMA, "books": {}}
    data = json.loads(path.read_text())
    if not isinstance(data, dict):
        raise ValueError(f"{path}: ledger root must be an object")
    data.setdefault("schema", SCHEMA)
    data.setdefault("books", {})
    return data


def save_ledger(rules_dir: Path, ledger: dict) -> Path:
    path = ledger_path(rules_dir)
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(ledger, indent=2, sort_keys=True) + "\n")
    tmp.replace(path)
    return path


def book_entry(ledger: dict, book: str) -> dict:
    return ledger.setdefault("books", {}).setdefault(
        book, {"book": book, "active": None, "closed": []}
    )


def active_epoch(rules_dir: Path, book: str) -> dict | None:
    return book_entry(load_ledger(rules_dir), book).get("active")


def parse_ts(value: Any) -> dt.datetime | None:
    if not value or not isinstance(value, str):
        return None
    text = value.strip()
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    try:
        stamp = dt.datetime.fromisoformat(text)
    except ValueError:
        return None
    if stamp.tzinfo is None:
        stamp = stamp.replace(tzinfo=dt.timezone.utc)
    return stamp.astimezone(dt.timezone.utc)


def iso(stamp: dt.datetime) -> str:
    return stamp.astimezone(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def window(epoch: dict | None) -> tuple[dt.datetime | None, dt.datetime | None]:
    if not epoch:
        return None, None
    return parse_ts(epoch.get("started_at")), parse_ts(epoch.get("ended_at"))


def includes(ts: Any, epoch: dict | None) -> bool:
    """True when ``ts`` belongs to ``epoch``.

    With no epoch we return True for compatibility, but every caller that
    renders *current* numbers must pass an epoch: scorecard and the promotion
    gate both fail closed when the ledger has no active epoch.
    """
    if epoch is None:
        return True
    start, end = window(epoch)
    stamp = parse_ts(ts)
    if stamp is None:
        # Unparseable timestamps cannot be proven current -> exclude.
        return False
    if start is not None and stamp < start:
        return False
    if end is not None and stamp >= end:
        return False
    return True


def epoch_days(epoch: dict | None, now: dt.datetime | None = None) -> int:
    start, end = window(epoch)
    if start is None:
        return 0
    stop = end or (now or dt.datetime.now(dt.timezone.utc))
    return max(0, (stop - start).days)


def strategy_fingerprint(rules_path: Path) -> str:
    """Hash the functional content of a rules file.

    Comments and blank lines are ignored so documentation edits do not force a
    new epoch; every other character counts, so any real knob change does.
    """
    text = Path(rules_path).read_text(encoding="utf-8", errors="replace")
    kept = []
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        kept.append(stripped)
    return hashlib.sha256("\n".join(kept).encode("utf-8")).hexdigest()


def rules_file_for(rules_dir: Path, book: str) -> Path | None:
    entry = book_entry(load_ledger(rules_dir), book)
    name = entry.get("rules_file")
    if name:
        candidate = Path(rules_dir) / name
        if candidate.is_file():
            return candidate
    return None


def epoch_status(rules_dir: Path, book: str) -> dict:
    """Describe the active epoch and whether the rules have drifted from it."""
    ledger = load_ledger(rules_dir)
    entry = book_entry(ledger, book)
    epoch = entry.get("active")
    out: dict = {
        "book": book,
        "active": epoch,
        "closed_count": len(entry.get("closed") or []),
        "days": epoch_days(epoch),
        "drift": False,
        "current_fingerprint": None,
        "ledger_present": ledger_path(rules_dir).is_file(),
    }
    rules_path = rules_file_for(rules_dir, book)
    if epoch and rules_path is not None:
        current = strategy_fingerprint(rules_path)
        out["current_fingerprint"] = current
        out["drift"] = current != epoch.get("strategy_fingerprint")
    return out


def lifetime_path(rules_dir: Path, book: str) -> Path:
    return Path(rules_dir) / LIFETIME_DIR / f"{book}.json"


def load_lifetime(rules_dir: Path, book: str) -> dict:
    path = lifetime_path(rules_dir, book)
    if not path.is_file():
        return {"book": book, "closed_epochs": [], "totals": {}}
    return json.loads(path.read_text())


def write_lifetime(rules_dir: Path, book: str) -> dict:
    """Rebuild the lifetime record from the ledger's closed epochs."""
    ledger = load_ledger(rules_dir)
    entry = book_entry(ledger, book)
    closed = entry.get("closed") or []
    totals = {
        "trades": sum(int((e.get("summary") or {}).get("closed_trades") or 0) for e in closed),
        "realized_usd": round(
            sum(float((e.get("summary") or {}).get("realized_usd") or 0.0) for e in closed), 2
        ),
        "epochs": len(closed),
    }
    payload = {
        "book": book,
        "closed_epochs": [
            {
                "id": e.get("id"),
                "started_at": e.get("started_at"),
                "ended_at": e.get("ended_at"),
                "strategy_version": e.get("strategy_version"),
                "close_reason": e.get("close_reason"),
                "summary": e.get("summary") or {},
            }
            for e in closed
        ],
        "totals": totals,
        "note": "Lifetime is retired history. Never merge into current-window metrics.",
    }
    path = lifetime_path(rules_dir, book)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")
    return payload


def filter_records(records: Iterable[dict], epoch: dict | None, key: str = "ts") -> list[dict]:
    return [r for r in records if includes(r.get(key), epoch)]
