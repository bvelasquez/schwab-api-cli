#!/usr/bin/env python3
"""Tests for scripts/scorecard.py. Run: python3 -m unittest discover -s scripts/tests"""
from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "scorecard.py"
spec = importlib.util.spec_from_file_location("scorecard", SCRIPT)
scorecard = importlib.util.module_from_spec(spec)
sys.modules["scorecard"] = scorecard
spec.loader.exec_module(scorecard)


def write_json(path: Path, doc) -> None:
    path.write_text(json.dumps(doc))


def write_journal(path: Path, events: list[dict]) -> None:
    path.write_text("".join(json.dumps(e) + "\n" for e in events))


def entry_event(position_id: str, ts: str, credit: float = 0.5) -> dict:
    return {
        "type": "sim_entry_filled",
        "ts": ts,
        "payload": {
            "position_id": position_id,
            "underlying": "QQQ",
            "entry_credit": credit,
            "contracts": 1,
        },
    }


def options_state(realized: float = 0.0, closed: list | None = None, open_positions: dict | None = None) -> dict:
    return {
        "open_positions": open_positions or {},
        "sim": {
            "starting_budget_usd": 30000,
            "realized_pnl_usd": realized,
            "closed_trades": closed or [],
        },
    }


class StemDefaults(unittest.TestCase):
    def test_default_stems_match_paper_host_files(self):
        self.assertEqual(scorecard.SWING_STATE, "trader-state-trader-swing-9947.json")
        self.assertEqual(scorecard.OPTIONS_STATE, "agent-sim-state-options-pilot-8709.json")


class MissingFiles(unittest.TestCase):
    def test_missing_state_files_raise_alerts(self):
        with tempfile.TemporaryDirectory() as tmp:
            alerts = scorecard.build_missing_file_alerts(Path(tmp))
        self.assertEqual(len(alerts), 2)
        self.assertTrue(all("state file missing" in a for a in alerts))

    def test_present_files_raise_no_alert(self):
        with tempfile.TemporaryDirectory() as tmp:
            rules = Path(tmp)
            write_json(rules / scorecard.SWING_STATE, {})
            write_json(rules / scorecard.OPTIONS_STATE, {})
            self.assertEqual(scorecard.build_missing_file_alerts(rules), [])


class EffectiveN(unittest.TestCase):
    def test_clones_collapse_to_one_distinct_entry(self):
        pid = "HASH|QQQ|2026-11-20|vertical|P699L_P704S"
        with tempfile.TemporaryDirectory() as tmp:
            rules = Path(tmp)
            for name in ("arm-o0", "arm-o1"):
                write_json(rules / f"agent-sim-state-options-{name}.json", options_state())
                write_journal(
                    rules / f"agent-sim-journal-options-{name}.jsonl",
                    [entry_event(pid, "2026-10-09T13:30:00+00:00")],
                )
            out = scorecard.options_arms_section(rules)
        self.assertEqual(out["nominal_entries"], 2)
        self.assertEqual(out["effective_entries"], 1)
        self.assertEqual(out["entries_shared_by_multiple_agents"], 1)

    def test_different_strikes_or_days_stay_distinct(self):
        with tempfile.TemporaryDirectory() as tmp:
            rules = Path(tmp)
            write_json(rules / "agent-sim-state-options-arm-o0.json", options_state())
            write_journal(
                rules / "agent-sim-journal-options-arm-o0.jsonl",
                [
                    entry_event("H|QQQ|2026-11-20|vertical|P699L_P704S", "2026-10-09T13:30:00+00:00"),
                    entry_event("H|QQQ|2026-11-20|vertical|P694L_P699S", "2026-10-09T13:30:00+00:00"),
                    entry_event("H|QQQ|2026-11-20|vertical|P699L_P704S", "2026-10-12T13:30:00+00:00"),
                ],
            )
            out = scorecard.options_arms_section(rules)
        self.assertEqual(out["effective_entries"], 3)

    def test_bak_state_files_are_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            rules = Path(tmp)
            write_json(rules / "agent-sim-state-options-pilot-8709.json", options_state())
            write_json(rules / "agent-sim-state-options-pilot-8709.json.bak-20260908", options_state())
            # the .bak file does not match the glob; also guard the explicit stem check
            out = scorecard.options_arms_section(rules)
        self.assertEqual(list(out["agents"]), ["pilot-8709"])

    def test_alert_when_arms_are_mostly_clones(self):
        alerts = scorecard.build_arm_alerts({"nominal_entries": 11, "effective_entries": 5})
        self.assertEqual(len(alerts), 1)
        self.assertIn("effective n", alerts[0])
        self.assertEqual(scorecard.build_arm_alerts({"nominal_entries": 3, "effective_entries": 3}), [])


class SwingLedger(unittest.TestCase):
    def test_state_ledger_is_used_when_journal_was_rotated(self):
        closed = [
            {"closed_at": f"2026-09-{day:02d}T14:00:00Z", "pnl_pct": pct, "pnl_usd": pct * 10, "exit_reason": reason}
            for day, pct, reason in [(1, 5.0, "profit_target"), (2, -4.0, "stop_loss"), (3, -4.0, "stop_loss")]
        ]
        with tempfile.TemporaryDirectory() as tmp:
            rules = Path(tmp)
            state = rules / "state.json"
            write_json(state, {"sim": {"starting_cash_usd": 4000, "closed_trades": closed}})
            out = scorecard.swing_section(state, [])
        self.assertEqual(out["closed_trades"], 3)
        self.assertEqual(out["exit_reason_mix"], {"profit_target": 1, "stop_loss": 2})
        self.assertAlmostEqual(out["win_rate_pct"], 100.0 / 3, places=3)


class Bootstrap(unittest.TestCase):
    def test_bootstrap_is_deterministic(self):
        values = [1.0, -2.0, 3.0, -1.5, 0.5]
        self.assertEqual(scorecard.bootstrap_mean_ci(values), scorecard.bootstrap_mean_ci(values))

    def test_atomic_write_replaces_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "latest.json"
            scorecard.write_atomic(path, "one")
            scorecard.write_atomic(path, "two")
            self.assertEqual(path.read_text(), "two")
            self.assertFalse((Path(tmp) / "latest.json.tmp").exists())


if __name__ == "__main__":
    unittest.main()
