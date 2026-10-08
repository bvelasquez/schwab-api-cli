#!/usr/bin/env python3
"""Nightly research agent.

Reads the Tier C summary and the trial log, proposes at most three arm YAML
files under rules/arms/proposed/, and applies a Benjamini-Hochberg adjustment
to any p-values it is given. It does not edit production rules and it does
not place orders. An optional --llm pass asks OpenRouter for a one-paragraph
note; the arm files stay template YAML either way.
"""
from __future__ import annotations

import argparse
import json
import os
import urllib.request
from pathlib import Path

# Hypotheses already encoded as shadow arms. The agent will not re-propose
# an id that is in this list or already in the trial log.
KNOWN = [
    "research-pullback",
    "research-dip",
    "research-regime",
    "research-rangecap",
    "research-no-blocklist",
    "research-wide-universe",
    "research-gap",
    "research-skip-event",
]


def benjamini_hochberg(pvals: list[float], q: float = 0.10) -> list[bool]:
    """Return which hypotheses survive BH at level q. p-values in input order."""
    m = len(pvals)
    if m == 0:
        return []
    order = sorted(range(m), key=lambda i: pvals[i])
    survive = [False] * m
    cutoff = -1
    for rank, i in enumerate(order, start=1):
        if pvals[i] <= (rank / m) * q:
            cutoff = rank
    if cutoff >= 0:
        for rank, i in enumerate(order, start=1):
            if rank <= cutoff:
                survive[i] = True
    return survive


def propose(arm_id: str, note: str) -> str:
    return f"""# Proposed by scripts/research_agent.py. Not applied.
# {note}
id: {arm_id}
base: research-base
overrides:
  # one variable. Fill this in before the arm is copied into the shadow list.
  playbook: {{}}
"""


def maybe_llm_note(summary: dict) -> str | None:
    key = os.environ.get("OPENROUTER_API_KEY", "").strip()
    if not key:
        return None
    body = json.dumps({
        "model": "google/gemini-2.5-flash",
        "temperature": 0,
        "max_tokens": 300,
        "messages": [
            {"role": "system", "content": "Suggest at most one testable swing-entry hypothesis. No orders. One paragraph."},
            {"role": "user", "content": json.dumps(summary)[:4000]},
        ],
    }).encode()
    req = urllib.request.Request(
        "https://openrouter.ai/api/v1/chat/completions",
        data=body,
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            doc = json.loads(resp.read().decode())
        return doc["choices"][0]["message"]["content"]
    except Exception as exc:  # noqa: BLE001 — a failed note must not block the job
        return f"(llm note failed: {exc})"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rules-dir", type=Path, default=Path("rules"))
    ap.add_argument("--llm", action="store_true")
    ap.add_argument("--max-proposals", type=int, default=3)
    args = ap.parse_args()
    summary_path = args.rules_dir / "analysis" / "candidate-outcomes-summary.json"
    summary = json.loads(summary_path.read_text()) if summary_path.is_file() else {}
    log_path = args.rules_dir / "arms" / "trial-log.json"
    log = json.loads(log_path.read_text()) if log_path.is_file() else {"trials": [], "pvals": []}
    rules_text = ""
    live = args.rules_dir / "trader-swing-9947.yaml"
    if live.is_file():
        rules_text = live.read_text()
    tried = {t.get("id") for t in log.get("trials", [])}
    for arm_id in KNOWN:
        if arm_id in tried:
            continue
        if f"id: {arm_id}" in rules_text:
            log["trials"].append({"id": arm_id, "status": "deployed_shadow"})
            tried.add(arm_id)
    proposed_dir = args.rules_dir / "arms" / "proposed"
    proposed_dir.mkdir(parents=True, exist_ok=True)
    new = []
    # One open slot for a hypothesis that is not already a shadow arm.
    extras = ["research-rs-off"]
    for arm_id in extras:
        if arm_id in tried or f"id: {arm_id}" in rules_text:
            continue
        if len(new) >= args.max_proposals:
            break
        path = proposed_dir / f"{arm_id}.yaml"
        path.write_text(propose(arm_id, "not yet in the shadow list; do not apply without the promotion gate"))
        new.append(arm_id)
        log["trials"].append({"id": arm_id, "status": "proposed"})
    pvals = [float(p) for p in log.get("pvals", []) if isinstance(p, (int, float))]
    log["bh_survive"] = benjamini_hochberg(pvals) if pvals else []
    log["trial_count"] = len(log["trials"])
    if args.llm:
        log["last_note"] = maybe_llm_note(summary)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_path.write_text(json.dumps(log, indent=2))
    print(json.dumps({"proposed": new, "trial_count": log["trial_count"], "bh_survive": log["bh_survive"]}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
