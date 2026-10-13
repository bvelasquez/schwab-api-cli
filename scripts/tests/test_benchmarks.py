#!/usr/bin/env python3
"""Tests for scripts/benchmarks.py. Run: python3 -m unittest discover -s scripts/tests"""
from __future__ import annotations

import datetime as dt
import gzip
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from zoneinfo import ZoneInfo

SCRIPT = Path(__file__).resolve().parents[1] / "benchmarks.py"
spec = importlib.util.spec_from_file_location("benchmarks", SCRIPT)
bench = importlib.util.module_from_spec(spec)
sys.modules["benchmarks"] = bench
spec.loader.exec_module(bench)

ET = ZoneInfo("America/New_York")


def put_chain(price: float, expiry: str, dte: int, bid: float = 2.0, ask: float = 2.2, strike: float = 700.0, delta: float = -0.25) -> dict:
    return {
        "data": {
            "underlyingPrice": price,
            "putExpDateMap": {f"{expiry}:{dte}": {f"{strike:.1f}": [{"bid": bid, "ask": ask, "delta": delta}]}},
            "callExpDateMap": {},
        }
    }


def snap(day: str, hhmm: str, doc: dict, sym: str = "SPY"):
    when = dt.datetime.strptime(f"{day} {hhmm}", "%Y-%m-%d %H%M").replace(tzinfo=ET)
    return (when, sym, doc)


class Putwrite(unittest.TestCase):
    def test_otm_expiry_keeps_the_credit_and_settles_after_expiry_day(self):
        expiry = "2026-10-16"
        snaps = [
            snap("2026-10-12", "0945", put_chain(720, expiry, 35)),
            snap("2026-10-16", "0945", put_chain(650, expiry, 0)),   # morning dip below the strike
            snap("2026-10-16", "1545", put_chain(720, expiry, 0)),   # closes back above it
            snap("2026-10-19", "0945", put_chain(720, "2026-11-20", 32)),
        ]
        out = bench.simulate_putwrite(snaps, "SPY", 0.25, fill_fraction=0.5, tbill_pct=4.0)
        self.assertEqual(out["cycles_settled"], 1)
        row = out["ledger"][0]
        # Settles on the 10-16 close (720), not the morning dip (650)
        self.assertEqual(row["settle_price"], 720)
        credit = 2.1 - 0.5 * (2.1 - 2.0)
        self.assertAlmostEqual(row["pnl_usd"], round(credit * 100, 2))

    def test_itm_expiry_pays_intrinsic_value(self):
        expiry = "2026-10-16"
        snaps = [
            snap("2026-10-12", "0945", put_chain(720, expiry, 35)),
            snap("2026-10-16", "1545", put_chain(690, expiry, 0)),
            snap("2026-10-19", "0945", put_chain(690, "2026-11-20", 32)),
        ]
        out = bench.simulate_putwrite(snaps, "SPY", 0.25, fill_fraction=0.5, tbill_pct=4.0)
        credit = 2.1 - 0.5 * 0.1
        self.assertAlmostEqual(out["ledger"][0]["pnl_usd"], round((credit - 10.0) * 100, 2))

    def test_open_cycle_is_marked_not_settled(self):
        snaps = [
            snap("2026-10-12", "0945", put_chain(720, "2026-11-20", 39, bid=2.0, ask=2.2)),
            snap("2026-10-13", "0945", put_chain(725, "2026-11-20", 38, bid=1.0, ask=1.2)),
        ]
        out = bench.simulate_putwrite(snaps, "SPY", 0.25, fill_fraction=0.5, tbill_pct=4.0)
        self.assertEqual(out["cycles_settled"], 0)
        self.assertIsNotNone(out["open_cycle"])
        self.assertGreater(out["open_mtm_usd"], 0)  # sold at ~2.05, buy back ~1.15
        self.assertEqual(out["total_usd"], out["open_mtm_usd"])

    def test_no_chain_gives_empty_result(self):
        out = bench.simulate_putwrite([], "SPY", 0.25, 0.5, 4.0)
        self.assertEqual(out["total_usd"], 0)
        self.assertIsNone(out["open_cycle"])


class Delta(unittest.TestCase):
    def test_net_delta_is_short_minus_long_in_shares(self):
        entry = {"contracts": 2, "signal": {"market_context": {"analytics": {"short_delta": -0.15, "long_delta": -0.11}}}}
        # sold -0.15 delta, bought -0.11: net = +0.15 - 0.11 = +0.04 per share
        self.assertAlmostEqual(bench.net_delta_shares(entry), (0.15 - 0.11) * 100 * 2)
        self.assertIsNone(bench.net_delta_shares({"contracts": 1}))

    def test_price_at_uses_first_price_at_or_after(self):
        series = [(dt.datetime(2026, 10, 12, 9, 45, tzinfo=ET), 100.0), (dt.datetime(2026, 10, 12, 15, 45, tzinfo=ET), 110.0)]
        self.assertEqual(bench.price_at(series, dt.datetime(2026, 10, 12, 10, 0, tzinfo=ET)), 110.0)
        self.assertEqual(bench.price_at(series, dt.datetime(2026, 10, 13, 10, 0, tzinfo=ET)), 110.0)
        self.assertIsNone(bench.price_at([], dt.datetime(2026, 10, 12, tzinfo=ET)))


class Report(unittest.TestCase):
    def test_agent_metrics_and_report_on_fixture_repo(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            rules = repo / "rules"
            day = rules / "chains" / "2026-10-12"
            day.mkdir(parents=True)
            for hhmm, price in (("0945", 100.0), ("1545", 110.0)):
                with gzip.open(day / f"QQQ-{hhmm}ET.json.gz", "wt") as fh:
                    json.dump({"data": {"underlyingPrice": price, "putExpDateMap": {}, "callExpDateMap": {}}}, fh)
            pid = "HASH|QQQ|2026-11-20|vertical|P699L_P704S"
            state = {
                "open_positions": {
                    pid: {"underlying": "QQQ", "entry_credit": 0.60, "last_good_debit_to_close": 0.40, "contracts": 1,
                          "opened_at": "2026-10-12T13:45:00+00:00", "max_loss_usd": 440.0}
                },
                "sim": {"closed_trades": [], "realized_pnl_usd": 0},
            }
            (rules / "agent-sim-state-options-arm-o0.json").write_text(json.dumps(state))
            entry = {
                "type": "sim_entry_filled", "ts": "2026-10-12T13:45:00+00:00",
                "payload": {"position_id": pid, "contracts": 1, "max_loss_usd": 440.0,
                            "signal": {"market_context": {"analytics": {"short_delta": -0.15, "long_delta": -0.11}}}},
            }
            (rules / "agent-sim-journal-options-arm-o0.jsonl").write_text(json.dumps(entry) + "\n")
            (rules / "agent-sim-state-options-broken.json").write_text("{not json")

            report = bench.build_report(repo)
            a = report["agents"]["arm-o0"]
            self.assertEqual(a["open_positions"], 1)
            self.assertAlmostEqual(a["open_mark_usd"], 20.0)  # (0.60 - 0.40) * 100
            self.assertAlmostEqual(a["return_on_max_risk_pct"], round(100 * 20 / 440, 3))
            # net delta +4 shares, underlying +10 => +40 delta-equivalent, alpha = 20 - 40
            self.assertAlmostEqual(a["delta_equiv_pnl_usd"], 40.0)
            self.assertAlmostEqual(a["beta_adjusted_alpha_usd"], -20.0)
            self.assertIn("error", report["agents"]["broken"])
            self.assertEqual(report["underlying"]["QQQ"]["return_pct"], 10.0)
            self.assertEqual(report["manual_constants"]["ira_equity_usd"], 637000.0)


if __name__ == "__main__":
    unittest.main()
