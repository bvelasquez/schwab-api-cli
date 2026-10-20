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
        "# the 4% OTM and 6% break-even gates, so this arm is QQQ in practice.\n"
        "# HYPOTHESIS: 12-20 delta credit spreads collect enough variance premium to beat costs.\n"
        "# FALSIFIER: expectancy per trade at or below zero after 20 distinct closed trades."
    ),
    "o1": (
        "O1 ladder. Floors 0.30 / 6%. SPY distance gates relaxed to 3% OTM and\n"
        "# 3.5% to break-even. Vertical max_open is 3, but QQQ and SPY share one\n"
        "# correlation group capped at 2, so two slots is the real limit.\n"
        "# No duplicate expiry (engine already rejects a second short on an open expiry).\n"
        "# HYPOTHESIS: a second slot adds trades without lowering per-trade edge.\n"
        "# FALSIFIER: per-trade expectancy below O0's, or entries mostly duplicate O0's same-day spread."
    ),
    "o2": (
        "O2 wide. $10-wide, delta 0.12-0.20, credit at least 0.80 and 8% of width.\n"
        "# Two slots. SPY uses 3% OTM, 3.5% to break-even, and the 8% credit floor.\n"
        "# HYPOTHESIS: wider spreads keep more credit dollars per dollar of fixed trading cost.\n"
        "# FALSIFIER: return on max risk below O0's after 20 distinct closed trades."
    ),
    "o3": (
        "O3 short cycle. 21-30 DTE, 50% profit target, calendar close at 10 DTE.\n"
        "# Floors 0.30 / 6%, delta 0.12-0.20, $5 wide, two slots. SPY is not relaxed.\n"
        "# HYPOTHESIS: shorter cycles are a negative control; they cost more turnover for the same edge.\n"
        "# FALSIFIER: per-trade expectancy above O0's after 20 distinct closed trades."
    ),
    "o4": (
        "O4 higher-delta sensitivity arm. Delta 0.20-0.25, $5 wide, credit 0.50\n"
        "# and 10% of width. One slot, same exits as O0. Distance gates are relaxed to\n"
        "# 2.5% OTM / 2.5% to break-even / POP 60 because a 20-25 delta short cannot clear the\n"
        "# 6% break-even cushion the other arms use (the first version admitted 0 of 2047\n"
        "# candidates on 2026-10-09).\n"
        "# HYPOTHESIS: more premium per trade offsets the lower win rate.\n"
        "# FALSIFIER: expectancy per trade below O0 after 20 distinct closed trades."
    ),
    "o5": (
        "O5 sixty-day cycle. 55-70 DTE, 50% profit target, calendar close at 30 DTE\n"
        "# (half the entry DTE). Everything else matches O0: floors 0.30 / 6%, delta 0.12-0.20,\n"
        "# $5 wide, one slot.\n"
        "# HYPOTHESIS: fewer, longer trades keep more edge after bid/ask costs (public 13-year\n"
        "# SPY study found 60 DTE the only profitable arm under realistic fills).\n"
        "# FALSIFIER: expectancy per trade not above O0's after 20 distinct closed trades."
    ),
    "o6": (
        "O6 no-stop control. Entries identical to O0. Profit target 50%, calendar close at\n"
        "# 21 DTE. The mechanical stop is effectively off (1000x credit), defensive roll off,\n"
        "# thesis exits off. Max loss is still capped by the spread width.\n"
        "# HYPOTHESIS: stops lower drawdown but do not raise expectancy for 12-20 delta spreads.\n"
        "# FALSIFIER: max drawdown or worst loss materially worse than O0's with no expectancy gain."
    ),
    "o7": (
        "O7 HAR-RV gate. Entries identical to O0 except the IV/RV gate compares chain IV with a\n"
        "# fixed-weight HAR-RV forecast of future vol (daily 0.4, weekly 0.3, monthly 0.3 variance)\n"
        "# instead of trailing 20-day vol. Same 1.05 threshold as O0.\n"
        "# HYPOTHESIS: selling premium when IV exceeds forecast vol, not trailing vol, lifts expectancy.\n"
        "# FALSIFIER: expectancy per trade not above O0's after 20 distinct closed trades, or the gate\n"
        "# admits the same entries as O0 on most days (no information added)."
    ),
    "o8": (
        "O8 term-structure gate. Entries identical to O0, plus new entries pause when VIX / VIX3M\n"
        "# is at or above 0.90 (a flat or inverted curve flags stress). A missing quote pauses (fail closed).\n"
        "# HYPOTHESIS: the VIX curve flags stress earlier than the VIX level, so pausing on it avoids losses.\n"
        "# FALSIFIER: max drawdown or worst loss not better than O0's, or the gate pauses so often that\n"
        "# fewer than 20 distinct closed trades accrue in 60 trading days."
    ),
    "o9": (
        "O9 edge floor. Same book as O0 except credit must be at least 0.50 and 12.5% of\n"
        "# width. The 6% floor made breakeven win rate sit on top of POP.\n"
        "# HYPOTHESIS: a higher credit/width floor is the entry edge the other arms lack.\n"
        "# FALSIFIER: expectancy per trade at or below O0's after 20 distinct closed trades."
    ),
    "o10": (
        "O10 trend gate. Same book as O0, but new put credits pause whenever SPY is below\n"
        "# its 50-day average. vix_above is 0 so the SMA check is not waiting on a VIX band.\n"
        "# HYPOTHESIS: put credits sold under the 50-day average are the losing tape.\n"
        "# FALSIFIER: expectancy per trade not above O0's after 20 distinct closed trades."
    ),
    "o11": (
        "O11 breadth. Same book as O0, plus IWM and XLF. QQQ and SPY stay one correlation\n"
        "# group; IWM and XLF are a second group capped at one open spread.\n"
        "# HYPOTHESIS: uncorrelated underlyings raise distinct bets faster than another index clone.\n"
        "# FALSIFIER: IWM and XLF entries mostly duplicate QQQ/SPY on the same day, or expectancy\n"
        "# per trade is below O0's after 20 distinct closed trades."
    ),
}

ARMS = ("o0", "o1", "o2", "o3", "o4", "o5", "o6", "o7", "o8")
CONTROL_ARMS = ("o9", "o10", "o11")


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
                "    min_distance_to_be_pct: 6.0\n": "    min_distance_to_be_pct: 2.5\n",
                "    min_short_otm_pct: 4.0\n": "    min_short_otm_pct: 2.5\n",
                "    min_pop_pct: 68\n": "    min_pop_pct: 60\n",
            },
        )
        text = risk_caps(text, 1, 1, 1)
    elif arm == "o5":
        text = replace_block(
            text, "watchlist:\n", "entry_policy:\n", watchlist("0.30", "6.0", "0.30", "6.0", False)
        )
        text = vertical(
            text,
            **{
                "    dte_min: 32\n": "    dte_min: 55\n",
                "    dte_max: 45\n": "    dte_max: 70\n",
                "    min_credit: 0.50\n": "    min_credit: 0.30\n",
                "    min_credit_to_width_pct: 10.0\n": "    min_credit_to_width_pct: 6.0\n",
            },
        )
        text = replace_between(
            text, "exit_rules:\n", "  roll:\n", "  profit_target_pct: 60\n", "  profit_target_pct: 50\n"
        )
        text = replace_between(text, "exit_rules:\n", "  roll:\n", "  dte_close: 21\n", "  dte_close: 30\n")
        text = risk_caps(text, 1, 1, 1)
    elif arm == "o6":
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
        text = replace_between(
            text, "exit_rules:\n", "  roll:\n", "  profit_target_pct: 60\n", "  profit_target_pct: 50\n"
        )
        text = replace_between(
            text, "exit_rules:\n", "  roll:\n", "  stop_loss_pct: 200\n", "  stop_loss_pct: 100000\n"
        )
        text = replace_between(
            text,
            "exit_rules:\n",
            "  thesis:\n",
            "  roll:\n    enabled: true\n",
            "  roll:\n    enabled: false\n",
        )
        text = replace_between(
            text,
            "  thesis:\n",
            "risk:\n",
            "  thesis:\n    enabled: true\n",
            "  thesis:\n    enabled: false\n",
        )
        text = risk_caps(text, 1, 1, 1)
    elif arm in ("o7", "o8"):
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
        if arm == "o7":
            text = vertical(
                text,
                **{"    min_iv_rv_ratio: 1.05\n": "    min_iv_rv_ratio: 1.05\n    rv_estimator: har\n"},
            )
        else:
            text = replace_between(
                text,
                "regime:\n",
                "  realized_vol_lookback:",
                "  pause_on_missing_vix: true\n",
                "  pause_on_missing_vix: true\n"
                "  term_structure:\n"
                "    far_symbol: $VIX3M\n"
                "    pause_ratio_at_or_above: 0.90\n",
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


def header_block(arm: str) -> str:
    return (
        f"# Paper arm {arm} ($30k sleeve, LLM off, 300s tick). Does not change 8709.\n"
        f"# {HEADER[arm]}\n"
    )


def build_from_control(text: str, arm: str) -> str:
    start = text.index("# Paper arm o0")
    end = text.index("agent_id: options-arm-o0\n")
    text = text[:start] + header_block(arm) + f"agent_id: options-arm-{arm}\n" + text[end + len("agent_id: options-arm-o0\n") :]
    if arm == "o9":
        vertical = text.index("entry_rules:\n  vertical:\n")
        watch = text.index("watchlist:\n")
        head, watchlist, tail = text[:watch], text[watch:vertical], text[vertical:]
        watchlist = watchlist.replace("    min_credit: 0.30\n", "    min_credit: 0.50\n")
        watchlist = watchlist.replace(
            "    min_credit_to_width_pct: 6.0\n", "    min_credit_to_width_pct: 12.5\n"
        )
        tail = tail.replace("    min_credit: 0.30\n", "    min_credit: 0.50\n", 1)
        tail = tail.replace("    min_credit_to_width_pct: 6.0\n", "    min_credit_to_width_pct: 12.5\n", 1)
        text = head + watchlist + tail
    elif arm == "o10":
        text = text.replace(
            "  put_credit_guard:\n    vix_above: 18.0\n    require_benchmark_above_sma: 20\n",
            "  put_credit_guard:\n    vix_above: 0\n    require_benchmark_above_sma: 50\n",
            1,
        )
    elif arm == "o11":
        text = text.replace(
            "# QQQ/SPY only — no fallback sleeve burn on IWM/GLD/sectors.\n",
            "# QQQ/SPY plus IWM and XLF. IWM and XLF share one correlation slot.\n",
            1,
        )
        text = text.replace(
            "  - symbol: SPY\n    role: primary\n    min_credit: 0.30\n    min_credit_to_width_pct: 6.0\n",
            "  - symbol: SPY\n    role: primary\n    min_credit: 0.30\n    min_credit_to_width_pct: 6.0\n"
            "  - symbol: IWM\n    role: primary\n    min_credit: 0.30\n    min_credit_to_width_pct: 6.0\n"
            "  - symbol: XLF\n    role: primary\n    min_credit: 0.30\n    min_credit_to_width_pct: 6.0\n",
            1,
        )
        text = text.replace(
            "  allowed_underlyings:\n    - QQQ\n    - SPY\n",
            "  allowed_underlyings:\n    - QQQ\n    - SPY\n    - IWM\n    - XLF\n",
            1,
        )
        text = text.replace(
            "  max_open_per_underlying:\n    QQQ: 1\n    SPY: 1\n",
            "  max_open_per_underlying:\n    QQQ: 1\n    SPY: 1\n    IWM: 1\n    XLF: 1\n",
            1,
        )
        text = text.replace(
            "  correlation_groups:\n    - name: broad_index\n      symbols: [QQQ, SPY]\n      max_open: 1\n",
            "  correlation_groups:\n    - name: broad_index\n      symbols: [QQQ, SPY]\n      max_open: 1\n"
            "    - name: financials_smallcap\n      symbols: [IWM, XLF]\n      max_open: 1\n",
            1,
        )
    else:
        raise SystemExit(arm)
    if text.count(f"agent_id: options-arm-{arm}\n") != 1:
        raise SystemExit(f"{arm} agent id rewrite failed")
    if "options-pilot-8709" in text:
        raise SystemExit(f"{arm} still mentions the 8709 agent id")
    return text


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    control = OUT / "o0.yaml"
    if not control.is_file():
        raise SystemExit(f"missing {control}")
    control_text = control.read_text()
    for arm in CONTROL_ARMS:
        rendered = build_from_control(control_text, arm)
        if "FB796575" in rendered or rendered.count(PLACEHOLDER) != 1:
            raise SystemExit(f"{arm} hash handling failed")
        dest = OUT / f"{arm}.yaml"
        dest.write_text(rendered)
        print(dest.relative_to(ROOT))

    src = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "rules" / "options-pilot-8709.yaml"
    if not src.is_file():
        return
    raw = base_from(src)
    for arm in ARMS:
        rendered = build(raw, arm)
        if "FB796575" in rendered or rendered.count(PLACEHOLDER) != 1:
            raise SystemExit(f"{arm} hash handling failed")
        dest = OUT / f"{arm}.yaml"
        dest.write_text(rendered)
        print(dest.relative_to(ROOT))


if __name__ == "__main__":
    main()
