#!/usr/bin/env python3
"""Tests for scripts/morning_review_agent.py."""
from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "morning_review_agent.py"
spec = importlib.util.spec_from_file_location("morning_review_agent", SCRIPT)
mra = importlib.util.module_from_spec(spec)
sys.modules["morning_review_agent"] = mra
spec.loader.exec_module(mra)


class DerivedSignals(unittest.TestCase):
    def test_small_sample_flag(self):
        swing = {"exit_reason_mix": {"stop_loss": 26, "profit_target": 13}, "last_20_expectancy_pct": 1.0}
        options = {"closed_trades": 4, "exits_on_stale_marks": 1}
        arms = {"agents": {"arm-o0": {"closed_trades": 1}}, "nominal_entries": 10, "effective_entries": 6}
        d = mra.build_derived_signals(swing, options, arms, {"stop_share_last10_pct": 55}, {})
        self.assertTrue(d["options_pilot_small_sample"])
        self.assertEqual(d["swing_stop_share_lifetime_pct"], round(100 * 26 / 39, 1))


class FactPack(unittest.TestCase):
    def test_journal_filters_by_ts_day_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            rules = Path(tmp) / "rules"
            rules.mkdir()
            j = rules / "agent-sim-journal-options-arm-o0.jsonl"
            j.write_text(
                json.dumps(
                    {
                        "ts": "2026-10-15T13:00:00+00:00",
                        "type": "entry_funnel",
                        "payload": {"day": "2026-10-16", "admitted": 1},
                    }
                )
                + "\n"
                + json.dumps(
                    {
                        "ts": "2026-10-16T13:00:00+00:00",
                        "type": "sim_exit_filled",
                        "payload": {"underlying": "SPY", "exit_reason": "stop_loss", "pnl_usd": -10},
                    }
                )
                + "\n"
            )
            ev = mra.journal_events_today(rules, "2026-10-16")
            self.assertEqual(len(ev), 1)
            self.assertEqual(ev[0]["underlying"], "SPY")

    def test_fallback_telegram_includes_summary(self):
        pack = {
            "scorecard_summary": "Scorecard 2026-10-16\nSwing: ok",
            "stale_agents": [],
            "journal_today": [],
            "drift_alerts": [],
        }
        text = mra.fallback_telegram(pack)
        self.assertIn("Scorecard", text)
        self.assertIn("Jarvis morning review", text)


if __name__ == "__main__":
    unittest.main()
