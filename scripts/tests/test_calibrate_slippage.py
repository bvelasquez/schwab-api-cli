#!/usr/bin/env python3
"""Tests for scripts/calibrate_slippage.py. Run: python3 -m unittest discover -s scripts/tests"""
from __future__ import annotations

import gzip
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "calibrate_slippage.py"
spec = importlib.util.spec_from_file_location("calibrate_slippage", SCRIPT)
cal = importlib.util.module_from_spec(spec)
sys.modules["calibrate_slippage"] = cal
spec.loader.exec_module(cal)


def contract(bid: float, ask: float, delta: float | None = None) -> dict:
    row = {"bid": bid, "ask": ask}
    if delta is not None:
        row["delta"] = delta
    return row


def chain(dte: int = 39) -> dict:
    """One QQQ put vertical: short 700 (delta -0.15) bid/ask 2.00/2.10, long 695 bid/ask 1.40/1.50."""
    return {
        "data": {
            "underlyingPrice": 740.0,
            "putExpDateMap": {
                f"2026-11-20:{dte}": {
                    "700.0": [contract(2.00, 2.10, -0.15)],
                    "695.0": [contract(1.40, 1.50, -0.11)],
                }
            },
            "callExpDateMap": {},
        }
    }


def write_snapshot(chains: Path, day: str, hhmm: str, doc: dict, sym: str = "QQQ") -> None:
    d = chains / day
    d.mkdir(parents=True, exist_ok=True)
    with gzip.open(d / f"{sym}-{hhmm}ET.json.gz", "wt") as fh:
        json.dump(doc, fh)


class SpreadMath(unittest.TestCase):
    def test_spread_metrics_credit_and_costs(self):
        m = cal.spread_metrics(contract(2.00, 2.10), contract(1.40, 1.50))
        self.assertAlmostEqual(m["mid_credit"], 2.05 - 1.45)
        self.assertAlmostEqual(m["natural_credit"], 2.00 - 1.50)
        self.assertAlmostEqual(m["spread_sum"], 0.10)

    def test_one_sided_or_crossed_quotes_are_skipped(self):
        self.assertIsNone(cal.spread_metrics(contract(0, 2.10), contract(1.40, 1.50)))
        self.assertIsNone(cal.spread_metrics(contract(2.20, 2.10), contract(1.40, 1.50)))

    def test_tiny_credit_is_skipped(self):
        self.assertIsNone(cal.spread_metrics(contract(1.00, 1.02), contract(0.95, 0.97)))

    def test_cost_rows_scale_with_fraction(self):
        m = cal.spread_metrics(contract(2.00, 2.10), contract(1.40, 1.50))
        rows = cal.cost_rows(m)
        # entry cost = f * 0.10 on a 0.60 credit
        self.assertAlmostEqual(rows[0.5]["entry_pct"], 100 * 0.05 / 0.60)
        self.assertAlmostEqual(rows[0.66]["entry_pct"], 100 * 0.066 / 0.60)
        # exit debit at a 50% target is 0.30, so exit cost % is double the entry %
        self.assertAlmostEqual(rows[0.5]["exit_pct"], 2 * rows[0.5]["entry_pct"])
        self.assertAlmostEqual(rows[0.5]["round_trip_usd"], 2 * 0.05 * 100)

    def test_bucket_edges(self):
        self.assertEqual(cal.bucket_for(32), "32-45")
        self.assertEqual(cal.bucket_for(45), "32-45")
        self.assertIsNone(cal.bucket_for(46))
        self.assertEqual(cal.bucket_for(55), "55-70")


class Enumeration(unittest.TestCase):
    def test_only_target_delta_shorts_in_dte_buckets_qualify(self):
        found = list(cal.enumerate_spreads(chain(39), 5.0))
        self.assertEqual(len(found), 1)
        bucket, side, metrics, strike, expiry = found[0]
        self.assertEqual((bucket, side, strike, expiry), ("32-45", "put", 700.0, "2026-11-20"))
        self.assertEqual(list(cal.enumerate_spreads(chain(50), 5.0)), [])  # DTE gap between buckets

    def test_delta_outside_range_is_skipped(self):
        doc = chain()
        doc["data"]["putExpDateMap"]["2026-11-20:39"]["700.0"][0]["delta"] = -0.30
        self.assertEqual(list(cal.enumerate_spreads(doc, 5.0)), [])


class FillTimeQuotes(unittest.TestCase):
    def entry(self, mid=0.60, nat=0.50, fill=0.57, with_quotes=True):
        ctx = {"leg_quotes": {"mid_credit": mid, "natural_credit": nat}} if with_quotes else {}
        return {"entry_credit": fill, "signal": {"estimated_credit": 0.6, "market_context": ctx}}

    def test_fill_time_quotes_extracted(self):
        q = cal.fill_time_quotes(self.entry())
        self.assertEqual((q["mid_credit"], q["natural_credit"], q["estimated_credit"]), (0.60, 0.50, 0.6))

    def test_missing_or_null_quotes_return_none(self):
        self.assertIsNone(cal.fill_time_quotes(self.entry(with_quotes=False)))
        self.assertIsNone(cal.fill_time_quotes({"signal": {"market_context": {"leg_quotes": {"mid_credit": None, "natural_credit": None}}}}))

    def test_paper_row_implied_fraction(self):
        row = cal.paper_row("arm-o0", "pid", 0.57, 0.60, 0.50, "when", 0.6)
        # filled 0.03 below mid on a 0.10 mid-to-natural gap
        self.assertAlmostEqual(row["implied_fraction_of_spread"], 0.3)
        self.assertAlmostEqual(row["paper_cost_vs_mid_pct"], 5.0)
        self.assertEqual(row["engine_estimated_credit"], 0.6)

    def test_compare_paper_uses_fill_time_quotes_without_any_snapshot(self):
        with tempfile.TemporaryDirectory() as tmp:
            rules = Path(tmp)
            pid = "HASH|QQQ|2026-11-20|vertical|P695L_P700S"
            ev = {"type": "sim_entry_filled", "ts": "2026-10-13T13:35:00+00:00", "payload": dict(self.entry(), position_id=pid)}
            (rules / "agent-sim-journal-options-arm-o0.jsonl").write_text(json.dumps(ev) + "\n")
            rows = cal.compare_paper(rules, {}, 5.0)
            self.assertEqual(len(rows), 1)
            self.assertIn("fill-time quotes", rows[0]["quotes_from"])
            self.assertAlmostEqual(rows[0]["implied_fraction_of_spread"], 0.3)


class Report(unittest.TestCase):
    def test_build_report_summarizes_snapshots(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            chains = repo / "rules" / "chains"
            write_snapshot(chains, "2026-10-09", "0945", chain())
            write_snapshot(chains, "2026-10-12", "0945", chain())
            (chains / "2026-10-12" / "BROKEN-0945ET.json.gz").write_bytes(b"not gzip")
            report = cal.build_report(repo, 5.0)
            self.assertEqual(report["snapshots"]["files"], 2)
            self.assertEqual(report["snapshots"]["days"], ["2026-10-09", "2026-10-12"])
            entry = report["by_symbol_and_dte"]["QQQ"]["32-45"]
            self.assertEqual(entry["n"], 2)
            self.assertAlmostEqual(entry["f0.5"]["entry_cost_pct_of_credit"]["median"], 100 * 0.05 / 0.60)

    def test_missing_chains_exit(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(SystemExit):
                cal.build_report(Path(tmp), 5.0)


if __name__ == "__main__":
    unittest.main()
