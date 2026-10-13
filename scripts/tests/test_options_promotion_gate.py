#!/usr/bin/env python3
"""Tests for scripts/options_promotion_gate.py. Run: python3 -m unittest discover -s scripts/tests"""
from __future__ import annotations

import importlib.util
import json
import random
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "options_promotion_gate.py"
spec = importlib.util.spec_from_file_location("options_promotion_gate", SCRIPT)
gate = importlib.util.module_from_spec(spec)
sys.modules["options_promotion_gate"] = gate
spec.loader.exec_module(gate)


def pid(und: str, legs: str) -> str:
    return f"HASH|{und}|2026-11-20|vertical|{legs}"


def agent(trades: list[tuple[str, str, float]], start: str = "2026-08-01", end: str = "2026-10-12") -> dict:
    """trades: (underlying, legs, pnl). Entries all on `start`, journal runs to `end`."""
    entries, exits = {}, []
    for und, legs, pnl in trades:
        p = pid(und, legs)
        entries[p] = {"ts": f"{start}T14:00:00+00:00", "position_id": p}
        exits.append({"ts": f"{end}T14:00:00+00:00", "position_id": p, "pnl_usd": pnl})
    return {"entries": entries, "exits": exits, "last_ts": f"{end}T20:00:00+00:00"}


def legs(i: int) -> str:
    return f"P{600 + i}L_P{605 + i}S"


class Helpers(unittest.TestCase):
    def test_weekdays_between_counts_inclusive_weekdays(self):
        self.assertEqual(gate.weekdays_between("2026-10-12", "2026-10-16"), 5)
        self.assertEqual(gate.weekdays_between("2026-10-10", "2026-10-11"), 0)
        self.assertEqual(gate.weekdays_between("", "2026-10-11"), 0)
        self.assertEqual(gate.weekdays_between("2026-10-12", "2026-10-01"), 0)

    def test_profit_factor_and_drawdown(self):
        self.assertEqual(gate.profit_factor([100, -50]), 2.0)
        self.assertEqual(gate.profit_factor([100]), float("inf"))
        self.assertIsNone(gate.profit_factor([]))
        self.assertEqual(gate.max_drawdown_usd([100, -30, -20, 60, -10]), 50.0)
        self.assertEqual(gate.max_drawdown_usd([10, 20]), 0.0)

    def test_benjamini_hochberg_known_values(self):
        adj = gate.benjamini_hochberg({"a": 0.01, "b": 0.04, "c": 0.03}, total=3)
        self.assertAlmostEqual(adj["a"], 0.03)
        self.assertAlmostEqual(adj["c"], 0.04)
        self.assertAlmostEqual(adj["b"], 0.04)
        # untested arms count toward m
        self.assertAlmostEqual(gate.benjamini_hochberg({"a": 0.01}, total=5)["a"], 0.05)

    def test_bootstrap_is_deterministic(self):
        vals = [1.0, -2.0, 3.0, 4.0, -1.0]
        a = gate.bootstrap_mean_ci(vals, random.Random(1), n=500)
        b = gate.bootstrap_mean_ci(vals, random.Random(1), n=500)
        self.assertEqual(a, b)
        self.assertLessEqual(a[1], a[0])
        self.assertGreaterEqual(a[2], a[0])


class Verdicts(unittest.TestCase):
    def test_too_little_data_is_insufficient_not_fail(self):
        agents = {
            "arm-o0": agent([("QQQ", legs(0), 20.0)], start="2026-10-01", end="2026-10-12"),
            "arm-o1": agent([("QQQ", legs(1), -50.0)], start="2026-10-01", end="2026-10-12"),
        }
        rows = gate.evaluate(agents)
        self.assertEqual(rows["arm-o0"]["verdict"], "control")
        self.assertEqual(rows["arm-o1"]["verdict"], "insufficient")
        joined = " ".join(rows["arm-o1"]["failed"])
        self.assertIn("distinct trades", joined)
        self.assertIn("trading days", joined)

    def test_shared_trades_do_not_count_as_distinct(self):
        shared = [("QQQ", legs(i), 10.0) for i in range(25)]
        agents = {"arm-o0": agent(shared), "arm-o1": agent(shared)}
        rows = gate.evaluate(agents)
        self.assertEqual(rows["arm-o1"]["n_closed"], 25)
        self.assertEqual(rows["arm-o1"]["n_distinct"], 0)
        self.assertEqual(rows["arm-o1"]["verdict"], "insufficient")

    def test_clearly_better_arm_passes(self):
        rng = random.Random(7)
        ctrl = [("QQQ", legs(i), rng.choice([-30.0, -10.0, 5.0, 8.0])) for i in range(30)]
        # Distinct strikes, consistently positive
        arm = [("QQQ", legs(100 + i), 40.0 + rng.random()) for i in range(30)]
        rows = gate.evaluate({"arm-o0": agent(ctrl), "arm-o1": agent(arm)})
        self.assertEqual(rows["arm-o1"]["verdict"], "pass", rows["arm-o1"])
        self.assertEqual(rows["arm-o1"]["failed"], [])
        self.assertGreater(rows["arm-o1"]["diff_vs_control_usd"], 0)

    def test_enough_data_but_worse_than_control_fails(self):
        ctrl = [("QQQ", legs(i), 20.0 + (i % 3)) for i in range(30)]
        arm = [("QQQ", legs(100 + i), -15.0 + (i % 3)) for i in range(30)]
        rows = gate.evaluate({"arm-o0": agent(ctrl), "arm-o1": agent(arm)})
        self.assertEqual(rows["arm-o1"]["verdict"], "fail")
        self.assertIn("expectancy CI not above zero", rows["arm-o1"]["failed"])

    def test_pilot_is_reference_only(self):
        ctrl = [("QQQ", legs(i), 5.0) for i in range(3)]
        rows = gate.evaluate({"arm-o0": agent(ctrl), "pilot-8709": agent(ctrl)})
        self.assertEqual(rows["pilot-8709"]["verdict"], "reference")


class Cli(unittest.TestCase):
    def test_main_writes_report_from_journals(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            rules = repo / "rules"
            rules.mkdir()
            ev = [
                {"type": "sim_entry_filled", "ts": "2026-10-01T14:00:00+00:00", "payload": {"position_id": pid("QQQ", legs(0))}},
                {"type": "sim_exit_filled", "ts": "2026-10-05T14:00:00+00:00", "payload": {"position_id": pid("QQQ", legs(0)), "pnl_usd": 12.5}},
            ]
            (rules / "agent-sim-journal-options-arm-o0.jsonl").write_text("".join(json.dumps(e) + "\n" for e in ev))
            (rules / "agent-sim-journal-options-arm-o1.jsonl.bak").write_text("garbage")
            self.assertEqual(gate.main(["--repo", str(repo)]), 0)
            out = json.loads((repo / "rules/analysis/options-promotion-gate.json").read_text())
            self.assertEqual(list(out["arms"]), ["arm-o0"])
            self.assertEqual(out["arms"]["arm-o0"]["realized_usd"], 12.5)
            self.assertIn("generated_at", out)
            self.assertFalse((repo / "rules/analysis/options-promotion-gate.json.tmp").exists())

    def test_main_with_no_journals_still_writes(self):
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "rules").mkdir()
            self.assertEqual(gate.main(["--repo", tmp]), 0)
            out = json.loads((Path(tmp) / "rules/analysis/options-promotion-gate.json").read_text())
            self.assertEqual(out["arms"], {})


if __name__ == "__main__":
    unittest.main()
