#!/usr/bin/env python3
"""Write rules/earnings-calendar.json from FMP when FMP_API_KEY is set.

The file is `{ "SYMBOL": "YYYY-MM-DD" }` for the next announced date.
Without a key, an existing file is left alone and a missing file becomes {}.
Shadow arms with earnings_source: calendar read this file and fall back to
the 91-day heuristic per symbol when a date is absent.
"""
from __future__ import annotations

import json
import os
import urllib.parse
import urllib.request
from datetime import date, timedelta
from pathlib import Path


def main() -> int:
    out = Path(os.environ.get("EARNINGS_CALENDAR_PATH", "rules/earnings-calendar.json"))
    key = os.environ.get("FMP_API_KEY", "").strip()
    if not key:
        if not out.is_file():
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_text("{}\n")
        print(json.dumps({"status": "no_key", "path": str(out)}))
        return 0
    start = date.today().isoformat()
    end = (date.today() + timedelta(days=120)).isoformat()
    qs = urllib.parse.urlencode({"from": start, "to": end, "apikey": key})
    url = f"https://financialmodelingprep.com/stable/earnings-calendar?{qs}"
    try:
        with urllib.request.urlopen(url, timeout=30) as resp:
            rows = json.loads(resp.read().decode())
    except Exception as exc:  # noqa: BLE001
        print(json.dumps({"status": "fetch_failed", "error": str(exc)}))
        return 0
    cal: dict[str, str] = {}
    if isinstance(rows, list):
        for row in rows:
            sym = (row.get("symbol") or "").upper()
            day = (row.get("date") or "")[:10]
            if sym and len(day) == 10 and sym not in cal:
                cal[sym] = day
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(cal, indent=2) + "\n")
    print(json.dumps({"status": "ok", "symbols": len(cal), "path": str(out)}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
