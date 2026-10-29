#!/usr/bin/env python3
"""Fetch 10y of daily bars for the swing strategy lab (read-only market data).

Runs on the host that owns the Schwab OAuth token (Jarvis). Uses `schwab market history`,
which only reads price history; it never touches orders. Writes one compact JSON per symbol
to rules/lab-data/daily/<SYMBOL>.json as {"symbol", "fetched_at", "bars": [[date, o, h, l, c, v], ...]}.

    python3 scripts/swing_lab_fetch.py [--repo ~/projects/schwabinvestbot] [--symbols SPY,QQQ]

Credentials come from the environment (SCHWAB_APP_KEY etc., for example
`set -a; . ~/.config/environment.d/schwab-paper.conf; set +a`).
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

ETFS = [
    "SPY", "QQQ", "IWM", "DIA", "EFA", "EEM", "AGG", "TLT", "IEF", "GLD", "VNQ", "BIL", "SHY",
    "XLK", "XLF", "XLE", "XLV", "XLI", "XLP", "XLU", "XLY", "XLB", "SMH",
]
STOCKS = [
    "AAPL", "MSFT", "NVDA", "AMZN", "GOOGL", "META", "AVGO", "JPM", "XOM", "UNH",
    "COST", "LLY", "WMT", "HD", "PG", "V", "MA", "JNJ", "CVX", "CAT",
]
DEFAULT_SYMBOLS = ETFS + STOCKS


def schwab_bin() -> str:
    found = shutil.which("schwab") or str(Path.home() / ".cargo/bin/schwab")
    return found


def fetch(symbol: str) -> list[list]:
    cmd = [
        schwab_bin(), "market", "history", symbol,
        "--period-type", "year", "--period", "10",
        "--frequency-type", "daily", "--frequency", "1", "--json",
    ]
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    if out.returncode != 0:
        raise RuntimeError(f"{symbol}: exit {out.returncode}: {out.stderr.strip()[:200]} {out.stdout[:200]}")
    doc = json.loads(out.stdout)
    candles = (doc.get("data") or {}).get("candles") or []
    bars = []
    for c in candles:
        d = dt.datetime.fromtimestamp(c["datetime"] / 1000, tz=dt.timezone.utc).date().isoformat()
        bars.append([d, c["open"], c["high"], c["low"], c["close"], c.get("volume", 0)])
    return bars


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", type=Path, default=Path(os.environ.get("SCHWAB_REPO", Path.home() / "projects/schwabinvestbot")))
    ap.add_argument("--symbols", default=",".join(DEFAULT_SYMBOLS))
    ap.add_argument("--sleep", type=float, default=0.4, help="pause between requests")
    args = ap.parse_args()

    out_dir = args.repo.expanduser() / "rules" / "lab-data" / "daily"
    out_dir.mkdir(parents=True, exist_ok=True)
    failures = 0
    for sym in [s.strip().upper() for s in args.symbols.split(",") if s.strip()]:
        try:
            bars = fetch(sym)
            if len(bars) < 200:
                raise RuntimeError(f"only {len(bars)} bars")
            doc = {"symbol": sym, "fetched_at": dt.datetime.now(dt.timezone.utc).isoformat(), "bars": bars}
            (out_dir / f"{sym}.json").write_text(json.dumps(doc, separators=(",", ":")))
            print(f"{sym}: {len(bars)} bars {bars[0][0]}..{bars[-1][0]}")
        except Exception as exc:  # keep going; report at the end
            failures += 1
            print(f"{sym}: FAILED {exc}", file=sys.stderr)
        time.sleep(args.sleep)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
