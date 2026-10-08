#!/usr/bin/env python3
"""Tier C labeler. Delegates to the engine so exits match the backtest walker.

Timer entry point. The binary writes rules/candidate-outcomes.jsonl.
"""
from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--rules-file", type=Path, required=True)
    p.add_argument("--cache", type=Path, default=None)
    p.add_argument("--output", type=Path, default=Path("rules/candidate-outcomes.jsonl"))
    p.add_argument("--bin", default=None, help="schwab-trader binary (default: PATH)")
    p.add_argument("--max-symbols", type=int, default=None)
    args = p.parse_args()
    binary = args.bin or shutil.which("schwab-trader")
    if not binary:
        print("schwab-trader is not on PATH; build and install the trader first", file=sys.stderr)
        return 1
    cmd = [
        binary,
        "research",
        "outcomes",
        "--rules-file",
        str(args.rules_file),
        "--output",
        str(args.output),
        "--json",
    ]
    if args.cache:
        cmd.extend(["--cache", str(args.cache)])
    if args.max_symbols is not None:
        cmd.extend(["--max-symbols", str(args.max_symbols)])
    proc = subprocess.run(cmd, check=False)
    return proc.returncode


if __name__ == "__main__":
    raise SystemExit(main())
