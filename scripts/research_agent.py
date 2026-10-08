#!/usr/bin/env python3
"""Nightly research clerk. Reads scorecards and outcomes, writes at most three
arm YAML proposals under rules/arms/proposed/. Nothing is applied.

Each proposal increments rules/arms/proposed/trial-log.jsonl. Significance
reported elsewhere should be Benjamini-Hochberg adjusted by that count
(see promotion_gate.py).
"""
from __future__ import annotations

import argparse
import json
from datetime import datetime, timezone
from pathlib import Path

MAX_PROPOSALS = 3


def bh_threshold(rank: int, trials: int, q: float = 0.10) -> float:
    if trials < 1 or rank < 1:
        return q
    return min(1.0, q * rank / trials)


def load_jsonl(path: Path) -> list[dict]:
    if not path.is_file():
        return []
    out = []
    for line in path.read_text(errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return out


def rejection_counts(outcomes: list[dict]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for row in outcomes:
        if row.get("admitted"):
            continue
        code = row.get("reason_code") or "other"
        counts[code] = counts.get(code, 0) + 1
    return counts


def propose(outcomes: list[dict], ic_report: dict | None) -> list[dict]:
    """Deterministic hypotheses from measured rejection mix and a failed IC.

    Returns proposal dicts. Empty when there is nothing to say.
    """
    ideas: list[dict] = []
    counts = rejection_counts(outcomes)
    if not counts and not ic_report:
        return []
    below = counts.get("below_sma", 0)
    total = sum(counts.values()) or 1
    if below / total >= 0.25 and below >= 30:
        ideas.append(
            {
                "id": "proposed-pullback",
                "hypothesis": "below_sma is the largest reject and the 2026-09-18 counterfactual had the wrong sign on the trend gate",
                "overrides": {"playbook": {"entry": {"require_below_sma": [9]}}},
            }
        )
    if counts.get("reward_risk_below_min", 0) >= 20:
        ideas.append(
            {
                "id": "proposed-rangecap",
                "hypothesis": "reward/risk rejects may be the 0% recent-range ceiling on names at the 60d high",
                "overrides": {
                    "playbook": {
                        "exit": {
                            "profit_target_recent_range_cap": {
                                "enabled": True,
                                "skip_nonpositive_ceiling": True,
                            }
                        }
                    }
                },
            }
        )
    if ic_report and ic_report.get("pass") is False:
        ideas.append(
            {
                "id": "proposed-llm-off",
                "hypothesis": "IC gate failed. Keep llm_signal.policy.mode off. Do not add another in-loop veto.",
                "overrides": {"llm_signal": {"policy": {"mode": "off"}}},
            }
        )
    return ideas[:MAX_PROPOSALS]


def render(idea: dict) -> str:
    body = json.dumps({"id": idea["id"], "overrides": idea["overrides"]}, indent=2)
    return (
        f"# NOT AUTO-APPLIED. Operator OK required before this overlay is pasted\n"
        f"# into a shadow arm. Hypothesis: {idea['hypothesis']}\n"
        f"{body}\n"
    )


def write_proposals(ideas: list[dict], dest: Path, log_path: Path) -> list[Path]:
    dest.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    written = []
    prior = sum(1 for line in log_path.read_text().splitlines() if line.strip()) if log_path.is_file() else 0
    for i, idea in enumerate(ideas, start=1):
        path = dest / f"{stamp}-{idea['id']}.json"
        path.write_text(render(idea))
        written.append(path)
        record = {
            "ts": datetime.now(timezone.utc).isoformat(),
            "id": idea["id"],
            "trial_index": prior + i,
            "bh_p_threshold_at_rank": bh_threshold(prior + i, prior + len(ideas)),
            "file": str(path),
        }
        with log_path.open("a") as fh:
            fh.write(json.dumps(record) + "\n")
    return written


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--outcomes", type=Path, default=Path("rules/candidate-outcomes.jsonl"))
    p.add_argument("--ic", type=Path, default=None)
    p.add_argument("--dest", type=Path, default=Path("rules/arms/proposed"))
    p.add_argument("--self-test", action="store_true")
    args = p.parse_args()
    if args.self_test:
        ideas = propose(
            [{"admitted": False, "reason_code": "below_sma"}] * 40
            + [{"admitted": False, "reason_code": "reward_risk_below_min"}] * 25,
            {"pass": False},
        )
        assert len(ideas) == 3, ideas
        assert bh_threshold(1, 10) == 0.01
        print(json.dumps({"self_test": "ok", "ideas": [i["id"] for i in ideas]}))
        return 0
    ic = None
    if args.ic and args.ic.is_file():
        ic = json.loads(args.ic.read_text())
    ideas = propose(load_jsonl(args.outcomes), ic)
    if not ideas:
        print("no hypotheses. Nothing written. Trial log unchanged.")
        return 0
    paths = write_proposals(ideas, args.dest, args.dest / "trial-log.jsonl")
    print(f"wrote {len(paths)} proposals. None were applied.")
    for path in paths:
        print(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
