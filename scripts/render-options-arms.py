#!/usr/bin/env python3
"""Render the five $30k options paper arms from the live 8709 rules file.

The account hash is replaced with __ACCOUNT_HASH__. scripts/install-options-arms.sh
substitutes the real hash on the paper host into gitignored rules/options-arm-oN.yaml.
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "rules" / "arms" / "options"
PLACEHOLDER = "__ACCOUNT_HASH__"

HEADER = {
    "o0": (
        "O0 control. Same gates as 8709 after the mark and IV fixes, with the\n"
        "# pre-2026-09-28 floors (credit 0.30, 6% of width). One slot. SPY keeps\n"
        "# the 4% OTM and 6% break-even gates, so this arm is QQQ in practice."
    ),
    "o1": (
        "O1 ladder. Floors 0.30 / 6%. SPY distance gates relaxed to 3% OTM and\n"
        "# 3.5% to break-even. Vertical max_open is 3, but QQQ and SPY share one\n"
        "# correlation group capped at 2, so two slots is the real limit.\n"
        "# No duplicate expiry (engine already rejects a second short on an open expiry)."
    ),
    "o2": (
        "O2 wide. $10-wide, delta 0.12-0.20, credit at least 0.80 and 8% of width.\n"
        "# Two slots. SPY uses 3% OTM, 3.5% to break-even, and the 8% credit floor."
    ),
    "o3": (
        "O3 short cycle. 21-30 DTE, 50% profit target, calendar close at 10 DTE.\n"
        "# Floors 0.30 / 6%, delta 0.12-0.20, $5 wide, two slots. SPY is not relaxed."
    ),
    "o4": (
        "O4 higher-delta negative control. Delta 0.20-0.25, $5 wide, credit 0.50\n"
        "# and 10% of width. QQQ can meet that; SPY cannot. One slot, same exits as O0."
    ),
}


def replace_between(text: str, start: str, end: str, old: str, new: str, expected: int = 1) -> str:
    i = text.index(start)
    j = text.index(end, i + len(start))
    chunk = text[i:j]
    count = chunk.count(old)
    if count != expected:
        raise SystemExit(f"{old!r} found {count} times between {start!r} and {end!r}, want {expected}")
    return text[:i] + chunk.replace(old, new, expected) + text[j:]


def replace_block(text: str, start: str, end: str, new_block: str) -> str:
    i = text.index(start)
    j = text.index(end, i + len(start))
    if not new_block.endswith("\n"):
        new_block += "\n"
    return text[:i] + new_block + text[j:]


def watchlist(qqq_credit: str, qqq_ctw: str, spy_credit: str, spy_ctw: str, spy_distance: bool) -> str:
    spy_extra = ""
    if spy_distance:
        spy_extra = (
            "    # 12-20 delta SPY is about 4.4% OTM, under the global 4% / 6% gates.\n"
            "    min_short_otm_pct: 3.0\n"
            "    min_distance_to_be_pct: 3.5\n"
        )
    return (
        "watchlist:\n"
        "  - symbol: QQQ\n"
        "    role: primary\n"
        f"    min_credit: {qqq_credit}\n"
        f"    min_credit_to_width_pct: {qqq_ctw}\n"
        "  - symbol: SPY\n"
        "    role: primary\n"
        f"    min_credit: {spy_credit}\n"
        f"    min_credit_to_width_pct: {spy_ctw}\n"
        f"{spy_extra}"
    )


def base_from(path: Path) -> str:
    text = path.read_text()
    if "hash: " not in text:
        raise SystemExit(f"no account hash in {path}")
    lines = []
    for line in text.splitlines(keepends=True):
        if line.startswith("  - hash: "):
            lines.append(f"  - hash: {PLACEHOLDER}\n")
        else:
            lines.append(line)
    text = "".join(lines)
    if PLACEHOLDER not in text or text.count("hash: ") != 1:
        raise SystemExit("hash rewrite failed")
    return text


def common(text: str, arm: str) -> str:
    text = text.replace("agent_id: options-pilot-8709\n", f"agent_id: options-arm-{arm}\n", 1)
    note = (
        f"# Paper arm {arm} ($30k sleeve, LLM off, 300s tick). Does not change 8709.\n"
        f"# {HEADER[arm]}\n"
    )
    if not text.startswith("version: 1\n"):
        raise SystemExit("expected version: 1 at the top of the source rules")
    text = "version: 1\n" + note + text[len("version: 1\n") :]
    text = replace_between(
        text,
        "schedule:\n",
        "strategies:\n",
        "  tick_interval_seconds: 120\n",
        "  tick_interval_seconds: 300\n  tick_jitter_seconds: 90\n",
    )
    text = replace_between(
        text,
        "risk:\n",
        "regime:\n",
        "  max_portfolio_risk_usd: 600\n",
        "  max_portfolio_risk_usd: 3000\n",
    )
    text = replace_between(
        text,
        "risk:\n",
        "regime:\n",
        "  max_risk_per_trade_usd: 550\n",
        "  max_risk_per_trade_usd: 1100\n",
    )
    text = replace_between(
        text,
        "risk:\n",
        "regime:\n",
        "  drawdown_sleeve_usd: 4000\n",
        "  drawdown_sleeve_usd: 30000\n",
    )
    text = replace_between(
        text,
        "simulation:\n",
        "execution:\n",
        "  starting_budget_usd: 4000\n",
        "  starting_budget_usd: 30000\n",
    )
    return text


def vertical(text: str, **fields: str) -> str:
    for old, new in fields.items():
        text = replace_between(
            text, "entry_rules:\n  vertical:\n", "  iron_condor:\n", old, new
        )
    return text


def risk_caps(text: str, trades: int, per_underlying: int, correlation: int, correlation_note: str = "") -> str:
    note = f"      {correlation_note}\n" if correlation_note else ""
    text = replace_between(
        text,
        "risk:\n",
        "regime:\n",
        "  max_trades_per_day: 1\n",
        f"  max_trades_per_day: {trades}\n",
    )
    text = replace_between(
        text,
        "risk:\n",
        "regime:\n",
        "    QQQ: 1\n    SPY: 1\n",
        f"    QQQ: {per_underlying}\n    SPY: {per_underlying}\n",
    )
    text = replace_between(
        text,
        "risk:\n",
        "regime:\n",
        "      max_open: 1\n",
        f"{note}      max_open: {correlation}\n",
    )
    return text


def build(text: str, arm: str) -> str:
    text = common(text, arm)
    if arm == "o0":
        text = replace_block(
            text, "watchlist:\n", "entry_policy:\n", watchlist("0.30", "6.0", "0.30", "6.0", False)
        )
        text = vertical(
            text,
            **{
                "    min_credit: 0.50\n": "    min_credit: 0.30\n",
                "    min_credit_to_width_pct: 10.0\n": "    min_credit_to_width_pct: 6.0\n",
            },
        )
        text = risk_caps(text, 1, 1, 1)
    elif arm == "o1":
        text = replace_block(
            text, "watchlist:\n", "entry_policy:\n", watchlist("0.30", "6.0", "0.30", "6.0", True)
        )
        text = vertical(
            text,
            **{
                "    min_credit: 0.50\n": "    min_credit: 0.30\n",
                "    min_credit_to_width_pct: 10.0\n": "    min_credit_to_width_pct: 6.0\n",
                "    max_open_positions: 1\n": "    max_open_positions: 3\n",
            },
        )
        text = risk_caps(
            text,
            2,
            2,
            2,
            "# QQQ and SPY are one group, so 2 is the real slot cap (vertical max_open is 3).",
        )
    elif arm == "o2":
        text = replace_block(
            text, "watchlist:\n", "entry_policy:\n", watchlist("0.80", "8.0", "0.80", "8.0", True)
        )
        text = vertical(
            text,
            **{
                "    min_credit: 0.50\n": "    min_credit: 0.80\n",
                "    max_width: 5\n": "    max_width: 10\n    widths:\n      - 10\n",
                "    min_credit_to_width_pct: 10.0\n": "    min_credit_to_width_pct: 8.0\n",
                "    max_open_positions: 1\n": "    max_open_positions: 2\n",
            },
        )
        text = risk_caps(text, 2, 2, 2)
    elif arm == "o3":
        text = replace_block(
            text, "watchlist:\n", "entry_policy:\n", watchlist("0.30", "6.0", "0.30", "6.0", False)
        )
        text = vertical(
            text,
            **{
                "    dte_min: 32\n": "    dte_min: 21\n",
                "    dte_max: 45\n": "    dte_max: 30\n",
                "    min_credit: 0.50\n": "    min_credit: 0.30\n",
                "    min_credit_to_width_pct: 10.0\n": "    min_credit_to_width_pct: 6.0\n",
                "    max_open_positions: 1\n": "    max_open_positions: 2\n",
            },
        )
        text = replace_between(
            text, "exit_rules:\n", "  roll:\n", "  profit_target_pct: 60\n", "  profit_target_pct: 50\n"
        )
        text = replace_between(text, "exit_rules:\n", "  roll:\n", "  dte_close: 21\n", "  dte_close: 10\n")
        text = risk_caps(text, 2, 2, 2)
    elif arm == "o4":
        text = replace_block(
            text, "watchlist:\n", "entry_policy:\n", watchlist("0.50", "10.0", "0.50", "10.0", False)
        )
        text = vertical(
            text,
            **{
                "    short_delta_min: 0.12\n": "    short_delta_min: 0.20\n",
                "    short_delta_max: 0.20\n": "    short_delta_max: 0.25\n",
            },
        )
        text = risk_caps(text, 1, 1, 1)
    else:
        raise SystemExit(arm)
    if "options-pilot-8709" in text:
        raise SystemExit(f"{arm} still mentions the 8709 agent id")
    # The live prompts name the account and its size. LLM is off; keep them off the public tree.
    text = replace_block(text, "  prompts:\n", "notify:\n", "")
    if "$637k" in text or "IRA 8709" in text:
        raise SystemExit(f"{arm} still has personal prompt text")
    return text


def main() -> None:
    src = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "rules" / "options-pilot-8709.yaml"
    raw = base_from(src)
    OUT.mkdir(parents=True, exist_ok=True)
    for arm in ("o0", "o1", "o2", "o3", "o4"):
        rendered = build(raw, arm)
        if "FB796575" in rendered or rendered.count(PLACEHOLDER) != 1:
            raise SystemExit(f"{arm} hash handling failed")
        dest = OUT / f"{arm}.yaml"
        dest.write_text(rendered)
        print(dest.relative_to(ROOT))


if __name__ == "__main__":
    main()
