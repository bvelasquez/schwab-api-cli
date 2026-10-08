#!/usr/bin/env python3
"""Write a candidate-pool YAML from symbols present in a daily backtest cache."""
from __future__ import annotations

import argparse
import json
from pathlib import Path


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--cache", type=Path, required=True)
    p.add_argument("--out", type=Path, default=Path("rules/universe/cache-symbols.yaml"))
    p.add_argument("--min-bars", type=int, default=200)
    args = p.parse_args()
    cache = json.loads(args.cache.read_text())
    symbols = sorted(
        sym
        for sym, bars in (cache.get("symbols") or {}).items()
        if isinstance(bars, list) and len(bars) >= args.min_bars and sym != "SPY"
    )
    lines = [
        "version: 1",
        "label: cache-symbols",
        f"# {len(symbols)} symbols with at least {args.min_bars} bars in {args.cache.name}",
        "symbols:",
    ]
    lines.extend(f"  - {sym}" for sym in symbols)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text("\n".join(lines) + "\n")
    print(f"wrote {len(symbols)} symbols to {args.out}")


if __name__ == "__main__":
    main()
