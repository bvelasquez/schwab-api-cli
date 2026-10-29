#!/usr/bin/env python3
"""Tests for epoch accounting.

The contract: current metrics are a function of the active epoch only. These
tests pin the boundary behaviour, the drift detector, and the two consumers
(scorecard journal discovery and the promotion gate) so a future edit cannot
silently start mixing retired results into current numbers.
"""
from __future__ import annotations

import datetime as dt
import json
import sys
from pathlib import Path

import pytest

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
sys.path.insert(0, str(SCRIPTS))

import epochs  # noqa: E402
import promotion_gate  # noqa: E402


EPOCH = {
    "id": "swing-9947-test",
    "started_at": "2026-10-23T19:52:00Z",
    "ended_at": None,
    "strategy_version": "test",
}


@pytest.mark.parametrize(
    "ts,expected",
    [
        ("2026-10-23T19:51:59Z", False),  # one second before the reset
        ("2026-10-23T19:52:00Z", True),  # exactly at the boundary
        ("2026-10-24T15:30:00Z", True),
        ("2026-10-23T12:52:00-07:00", True),  # same instant, other offset
        ("2026-10-23T12:51:00-07:00", False),
        (None, False),
        ("", False),
        ("not-a-timestamp", False),
    ],
)
def test_includes_boundaries(ts, expected):
    assert epochs.includes(ts, EPOCH) is expected


def test_closed_epoch_excludes_at_end():
    closed = dict(EPOCH, ended_at="2026-10-25T00:00:00Z")
    assert epochs.includes("2026-10-24T23:59:59Z", closed)
    assert not epochs.includes("2026-10-25T00:00:00Z", closed)


def test_epoch_days_counts_from_start():
    now = dt.datetime(2026, 11, 2, 19, 52, tzinfo=dt.timezone.utc)
    assert epochs.epoch_days(EPOCH, now) == 10
    assert epochs.epoch_days(None, now) == 0


def test_fingerprint_ignores_comments_and_blank_lines(tmp_path):
    rules = tmp_path / "r.yaml"
    rules.write_text("a: 1\n# note\n\nb: 2\n")
    base = epochs.strategy_fingerprint(rules)
    rules.write_text("a: 1\nb: 2\n# different note\n")
    assert epochs.strategy_fingerprint(rules) == base
    rules.write_text("a: 1\nb: 3\n")
    assert epochs.strategy_fingerprint(rules) != base


def _ledger(tmp_path, rules_text="a: 1\n", active=True, fingerprint=None):
    rules = tmp_path / "rules"
    rules.mkdir(exist_ok=True)
    rules_file = rules / "trader-swing-9947.yaml"
    rules_file.write_text(rules_text)
    epoch = dict(
        EPOCH,
        strategy_fingerprint=fingerprint or epochs.strategy_fingerprint(rules_file),
    )
    ledger = {"schema": 1, "books": {"swing-9947": {"book": "swing-9947",
              "rules_file": rules_file.name, "active": epoch if active else None,
              "closed": []}}}
    (rules / "epochs.json").write_text(json.dumps(ledger))
    return rules


def test_status_reports_no_drift_when_rules_unchanged(tmp_path):
    rules = _ledger(tmp_path)
    status = epochs.epoch_status(rules, "swing-9947")
    assert status["ledger_present"] is True
    assert status["active"]["id"] == "swing-9947-test"
    assert status["drift"] is False


def test_status_flags_drift_after_rules_change(tmp_path):
    rules = _ledger(tmp_path)
    (rules / "trader-swing-9947.yaml").write_text("a: 2\n")
    status = epochs.epoch_status(rules, "swing-9947")
    assert status["drift"] is True
    assert status["current_fingerprint"] != status["active"]["strategy_fingerprint"]


def test_status_without_ledger_fails_closed(tmp_path):
    rules = tmp_path / "rules"
    rules.mkdir()
    status = epochs.epoch_status(rules, "swing-9947")
    assert status["ledger_present"] is False
    assert status["active"] is None


def test_lifetime_totals_only_closed_epochs(tmp_path):
    rules = _ledger(tmp_path, active=False)
    ledger = epochs.load_ledger(rules)
    ledger["books"]["swing-9947"]["closed"] = [
        {"id": "old", "summary": {"closed_trades": 2, "realized_usd": -10.5}},
        {"id": "older", "summary": {"closed_trades": 3, "realized_usd": 4.25}},
    ]
    epochs.save_ledger(rules, ledger)
    payload = epochs.write_lifetime(rules, "swing-9947")
    assert payload["totals"] == {"trades": 5, "realized_usd": -6.25, "epochs": 2}
    assert len(payload["closed_epochs"]) == 2


def test_promotion_gate_drops_days_outside_epoch():
    days = {
        "2026-10-20": {"equity_usd": 1},
        "2026-10-23": {"equity_usd": 2},
        "2026-10-24": {"equity_usd": 3},
    }
    assert set(promotion_gate.filter_days(days, EPOCH)) == {"2026-10-23", "2026-10-24"}
    assert promotion_gate.filter_days(days, None) == {}


def test_promotion_gate_book_key_from_trader_stem():
    assert promotion_gate.book_from_stem("trader-swing-9947") == "swing-9947"
    assert promotion_gate.book_from_stem("swing-beneficiary-9947") == "swing-9947"


def test_scorecard_journal_discovery_skips_archive(tmp_path):
    import scorecard

    rules = tmp_path / "rules"
    (rules / "archive" / "swing-reset-2026-10-23").mkdir(parents=True)
    (rules / "archive" / "swing-reset-2026-10-23" / "trader-journal-trader-swing-9947.jsonl").write_text("")
    (rules / "trader-journal-trader-swing-9947.jsonl").write_text("")
    paths = scorecard.swing_journal_paths(rules, EPOCH)
    assert paths, "expected the active journal"
    assert all("archive" not in str(p) for p in paths)


def test_scorecard_book_key_matches_ledger():
    import scorecard

    assert scorecard.SWING_BOOK == "swing-9947"
