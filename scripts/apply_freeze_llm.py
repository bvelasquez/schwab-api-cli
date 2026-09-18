#!/usr/bin/env python3
"""Freeze the LLM layer's decision authority in a live rules file.

Why: the 2026-09-17 live-loop diagnosis (`rules/analysis-20260917/live-loop-diagnosis.md`)
showed the paper bot was not running its rules file. Over the window the journaled
`effective_playbook.active_profile_source` was `llm` on 75% of ticks, `veto_entries: true`
blocked 245 of 651 admissible candidates (38%), and LLM-adapted parameters were applied
live but never persisted, so the effective config drifted within each session. Consequence:
no static backtest can model the bot and no live measurement is attributable to the rules.

This applier removes that authority and nothing else. Deterministic regime adaptation
(`adaptation.enabled` + `regime_auto_select`, whose profiles were scaled x4 by the
approved risk-budget change) is deliberately left ON - it is the mechanism the sizing
change was designed around. The LLM layer keeps running for research/monitoring/journaling;
it simply no longer gates entries, selects profiles, or edits parameters.

Four toggles, each asserted to match exactly once WITHIN its own top-level section
(the file carries two identical `allow_rule_adaptation: true` lines in different blocks,
so a whole-file regex would be ambiguous):

  llm.veto_entries:              true -> false   (entry gate; runner.rs:995 -> llm.rs:549 returns true)
  llm.allow_rule_adaptation:     true -> false   (rules.rs:1343 / learn.rs:47 gate)
  simulation.allow_rule_adaptation: true -> false (rules.rs:112 -> backtest_learn_enabled)
  adaptation.llm_profile_select: true -> false   (adaptation.rs:212 gate)

Usage:
  python3 scripts/apply_freeze_llm.py           # apply in place, with backup
  python3 scripts/apply_freeze_llm.py --check   # dry run: report, change nothing
"""
import re
import shutil
import sys
import time

try:
    import yaml
except ImportError:      # macOS system python3 has no PyYAML; --check still works
    yaml = None

TARGET = "rules/trader-swing-9947.yaml"
CHECK = "--check" in sys.argv

# (top-level section, exact line to find within it, replacement)
CHANGES = [
    ("llm", "  veto_entries: true", "  veto_entries: false"),
    ("llm", "  allow_rule_adaptation: true", "  allow_rule_adaptation: false"),
    ("simulation", "  allow_rule_adaptation: true", "  allow_rule_adaptation: false"),
    ("adaptation", "  llm_profile_select: true", "  llm_profile_select: false"),
]


def split_sections(lines):
    """Map each top-level key (^name:) to its [start, end) line range."""
    starts = []
    for i, line in enumerate(lines):
        m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*):", line)
        if m:
            starts.append((i, m.group(1)))
    out = {}
    for idx, (i, name) in enumerate(starts):
        end = starts[idx + 1][0] if idx + 1 < len(starts) else len(lines)
        out[name] = (i, end)
    return out


def report(label, text):
    if yaml is None:
        print(f"  {label}: (PyYAML absent - raw line values)")
        lines = text.split("\n")
        sect = split_sections(lines)
        for section, key in [("llm", "enabled"), ("llm", "veto_entries"),
                             ("llm", "allow_rule_adaptation"),
                             ("simulation", "allow_rule_adaptation"),
                             ("adaptation", "enabled"), ("adaptation", "regime_auto_select"),
                             ("adaptation", "llm_profile_select")]:
            a, b = sect[section]
            for i in range(a, b):
                if re.match(rf"^  {key}:", lines[i]):
                    print(f"      {section}.{key} = {lines[i].split(':', 1)[1].strip()}")
        return
    d = yaml.safe_load(text)
    llm, sim, adapt = d["llm"], d["simulation"], d["adaptation"]
    print(f"  {label}:")
    print(f"      llm.veto_entries={llm['veto_entries']}"
          f"  llm.allow_rule_adaptation={llm['allow_rule_adaptation']}"
          f"  llm.enabled={llm['enabled']}")
    print(f"      simulation.allow_rule_adaptation={sim['allow_rule_adaptation']}")
    print(f"      adaptation.enabled={adapt['enabled']}"
          f"  regime_auto_select={adapt['regime_auto_select']}"
          f"  llm_profile_select={adapt['llm_profile_select']}")


original = open(TARGET).read()
print(f"target: {TARGET}")
report("before", original)

lines = original.split("\n")
sect = split_sections(lines)

for section, old, new in CHANGES:
    if section not in sect:
        sys.exit(f"ABORT: top-level section {section!r} not found - file not modified")
    a, b = sect[section]
    hits = [i for i in range(a, b) if lines[i] == old]
    if len(hits) != 1:
        sys.exit(f"ABORT: {section}.{old.strip()} matched {len(hits)} times "
                 f"(expected exactly 1) - file not modified")
    lines[hits[0]] = new

s = "\n".join(lines)
report("after ", s)

if CHECK:
    print("--check: nothing written")
    sys.exit(0)

if yaml is None:
    sys.exit("ABORT: PyYAML required to APPLY (the post-write parse check is not "
             "optional); run this on jarvis, or install PyYAML. --check works without it.")

bak = f"{TARGET}.bak-{time.strftime('%Y%m%d-%H%M%S')}-freeze-llm"
shutil.copy(TARGET, bak)
open(TARGET, "w").write(s)
yaml.safe_load(open(TARGET))          # must still parse
print(f"applied. backup: {bak}")