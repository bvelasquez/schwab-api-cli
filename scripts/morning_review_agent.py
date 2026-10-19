#!/usr/bin/env python3
"""Multi-turn LLM morning review of Jarvis paper trading.

Read-only: scorecard metrics, agent liveness, recent journal events. Optional
OpenRouter analysis (3 turns: analyze → critique → Telegram format). Sends
one message via Telegram when --telegram is set.

Does not place orders or reload rules.
"""
from __future__ import annotations

import argparse
import datetime as dt
import importlib.util
import json
import os
import sys
import urllib.request
from pathlib import Path
from typing import Any

DEFAULT_MODEL = "google/gemini-2.5-flash"
TELEGRAM_MAX = 3900

# Ground truth for the LLM — what already exists on Jarvis (do not recommend rebuilding).
OPERATOR_CONTEXT = {
    "existing_monitoring": [
        "scripts/agent-health.py + schwab-bot-watchdog: stale last_tick on paper units",
        "scripts/scorecard.py drift_alerts: credit/width <10%, stale mark exits, degraded exits, swing stop% last 10, drawdown, missing state files",
        "schwab-auto-login.timer + jarvis-token-keeper: OAuth refresh / invalid_grant",
        "paper-host-watchdog.sh: restart wedged --simulate units + Telegram",
    ],
    "options_arms_are_experiments": (
        "O0–O8 are A/B arms with fixed hypotheses (see arm_roles). Low credit/width on O3/O short-DTE "
        "arms is often by design (6% floors). Do not recommend pausing an arm for metrics that "
        "define the experiment unless data falsifies its hypothesis with sufficient N."
    ),
    "promotion": (
        "Arm promotion: scripts/options_promotion_gate.py (≥40 weekdays, ≥20 distinct closes vs O0, "
        "bootstrap CIs). Pilot 8709 is the production rules file, not an arm in that gate."
    ),
    "execution_quality": (
        "Degraded-quote deferral and DTE-close guards live in rules YAML (exit_rules). "
        "Scorecard already counts exits_on_stale_marks and exits_on_degraded_quotes. "
        "Recommend threshold/Telegram on those counters, not a new generic watchdog service."
    ),
    "swing_stops": (
        "Lifetime stop_share in exit_reason_mix is not the same as scorecard's stop_share_last10_pct. "
        "Prefer last-10 and last_20_expectancy_pct before suggesting stop or entry changes."
    ),
}

ARM_ROLES: dict[str, str] = {
    "arm-o0": "Control — same gates as pilot after mark/IV fixes",
    "arm-o1": "Second slot / ladder hypothesis",
    "arm-o2": "Wide $10 spreads — more credit per cost",
    "arm-o3": "Short DTE negative control — low credit/width expected",
    "arm-o4": "Higher delta 0.20–0.25",
    "arm-o5": "Long 55–70 DTE cycle",
    "arm-o6": "No-stop control vs O0",
    "arm-o7": "HAR-RV IV gate vs trailing vol",
    "arm-o8": "VIX term-structure entry pause",
    "pilot-8709": "Production options pilot (not an O0–O8 arm)",
}


def load_scorecard(repo: Path):
    script = repo / "scripts" / "scorecard.py"
    spec = importlib.util.spec_from_file_location("scorecard", script)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {script}")
    mod = importlib.util.module_from_spec(spec)
    sys.modules["scorecard"] = mod
    spec.loader.exec_module(mod)
    return mod


def iso_age_minutes(ts: str | None) -> float | None:
    if not ts:
        return None
    try:
        t = dt.datetime.fromisoformat(ts.replace("Z", "+00:00"))
        return (dt.datetime.now(dt.timezone.utc) - t).total_seconds() / 60.0
    except Exception:
        return None


def discover_agents(rules: Path) -> list[tuple[str, Path]]:
    agents: list[tuple[str, Path]] = []
    pilot = rules / "agent-sim-state-options-pilot-8709.json"
    swing = rules / "trader-state-trader-swing-9947.json"
    if pilot.is_file():
        agents.append(("options-pilot", pilot))
    if swing.is_file():
        agents.append(("swing", swing))
    for p in sorted(rules.glob("agent-sim-state-options-arm-o*.json")):
        if ".bak" in p.name:
            continue
        agents.append((p.stem.replace("agent-sim-state-options-", "arm-"), p))
    return agents


def agent_liveness(rules: Path) -> list[dict]:
    rows: list[dict] = []
    for name, path in discover_agents(rules):
        if not path.is_file():
            rows.append({"agent": name, "status": "missing_state"})
            continue
        d = json.loads(path.read_text())
        lt = d.get("last_tick")
        age = iso_age_minutes(lt)
        regime = (d.get("last_regime") or {}).get("class")
        open_n = len(d.get("open_positions") or {})
        sim = d.get("sim") if isinstance(d.get("sim"), dict) else {}
        rows.append(
            {
                "agent": name,
                "last_tick": lt,
                "age_minutes": round(age, 1) if age is not None else None,
                "stale": age is None or age > 15,
                "regime": regime,
                "open_positions": open_n,
                "realized_pnl_usd": sim.get("realized_pnl_usd"),
            }
        )
    return rows


def journal_events_today(rules: Path, day: str) -> list[dict]:
    events: list[dict] = []
    for jpath in sorted(rules.glob("agent-sim-journal-*.jsonl")):
        arm = jpath.stem.replace("agent-sim-journal-options-", "")
        with jpath.open(encoding="utf-8", errors="replace") as fh:
            for line in fh:
                try:
                    o = json.loads(line)
                except json.JSONDecodeError:
                    continue
                ts = (o.get("ts") or "")[:10]
                if ts != day:
                    continue
                typ = o.get("type")
                if typ not in ("sim_exit_filled", "sim_entry_filled", "entry_funnel", "defensive_roll"):
                    continue
                pl = o.get("payload") or {}
                events.append(
                    {
                        "arm": arm,
                        "ts": o.get("ts"),
                        "type": typ,
                        "underlying": pl.get("underlying"),
                        "exit_reason": pl.get("exit_reason"),
                        "pnl_usd": pl.get("pnl_usd"),
                        "entry_credit": pl.get("entry_credit"),
                        "funnel_day": pl.get("day"),
                        "admitted": pl.get("admitted"),
                    }
                )
    swing_j = rules / "trader-journal-trader-swing-9947.jsonl"
    if swing_j.is_file():
        with swing_j.open(encoding="utf-8", errors="replace") as fh:
            for line in fh:
                try:
                    o = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if (o.get("ts") or "")[:10] != day:
                    continue
                typ = o.get("type")
                if typ not in ("sim_entry_filled", "sim_exit_filled", "entry_rejected"):
                    continue
                pl = o.get("payload") or o
                events.append(
                    {
                        "arm": "swing",
                        "ts": o.get("ts"),
                        "type": typ,
                        "symbol": pl.get("symbol"),
                        "reason": pl.get("reason") or pl.get("exit_reason"),
                        "pnl_usd": pl.get("pnl_usd"),
                    }
                )
    return events


def load_promotion_gate_summary(rules: Path) -> dict | None:
    path = rules / "analysis" / "options-promotion-gate.json"
    if not path.is_file():
        return None
    try:
        doc = json.loads(path.read_text())
    except json.JSONDecodeError:
        return None
    arms = doc.get("arms") or doc.get("agents") or {}
    brief: dict[str, str] = {}
    if isinstance(arms, dict):
        for arm_id, row in arms.items():
            if isinstance(row, dict):
                brief[str(arm_id)] = str(row.get("verdict") or row.get("status") or "unknown")
    return {"arms": brief, "generated_at": doc.get("generated_at")} if brief else None


def build_derived_signals(
    swing: dict,
    options: dict,
    options_arms: dict,
    swing_alert_inputs: dict,
    options_alert_inputs: dict,
) -> dict[str, Any]:
    exit_mix = swing.get("exit_reason_mix") or {}
    total_exits = sum(exit_mix.values()) if exit_mix else 0
    stop_lifetime = exit_mix.get("stop_loss", 0)
    stop_lifetime_pct = round(100.0 * stop_lifetime / total_exits, 1) if total_exits else None

    pilot_closed = int(options.get("closed_trades") or 0)
    arms_meta = options_arms.get("agents") or {}
    arm_closed_max = max(
        (int((arms_meta.get(k) or {}).get("closed_trades") or 0) for k in arms_meta),
        default=0,
    )

    return {
        "swing_stop_share_lifetime_pct": stop_lifetime_pct,
        "swing_stop_share_last10_pct": swing_alert_inputs.get("stop_share_last10_pct"),
        "swing_last_20_expectancy_pct": swing.get("last_20_expectancy_pct"),
        "swing_sessions_since_last_entry": swing_alert_inputs.get("sessions_since_entry"),
        "options_pilot_closed_trades": pilot_closed,
        "options_pilot_small_sample": pilot_closed < 20,
        "options_arm_max_closed_trades": arm_closed_max,
        "options_nominal_vs_distinct_entries": {
            "nominal": options_arms.get("nominal_entries"),
            "distinct": options_arms.get("effective_entries"),
        },
        "options_stale_exits": options.get("exits_on_stale_marks"),
        "options_degraded_exits": options.get("exits_on_degraded_quotes"),
        "arm_roles": ARM_ROLES,
    }


def public_books(rules: Path) -> dict:
    out: dict[str, Any] = {}
    for fname in ("agent-trader-public-paper.json", "agent-trader-public-swing-paper.json"):
        p = rules / fname
        if not p.is_file():
            continue
        d = json.loads(p.read_text())
        pos = d.get("open_positions") or d.get("positions") or []
        syms: list[str] = []
        if isinstance(pos, dict):
            iterable = pos.values()
        else:
            iterable = pos
        for item in iterable:
            if isinstance(item, dict):
                syms.append(str(item.get("underlying") or item.get("symbol") or "?"))
        out[fname] = {
            "last_tick": d.get("last_tick"),
            "open_count": len(pos) if isinstance(pos, list) else len(pos),
            "symbols": syms,
        }
    return out


def build_fact_pack(repo: Path, day: str) -> dict:
    sc = load_scorecard(repo)
    rules = repo / "rules"
    journal_paths = sc.swing_journal_paths(rules)
    swing = sc.swing_section(rules / sc.SWING_STATE, journal_paths)
    options = sc.options_section(rules / sc.OPTIONS_STATE, rules / sc.OPTIONS_JOURNAL)
    options_arms = sc.options_arms_section(rules)
    drift = sc.build_drift_alerts(swing, options)
    drift.extend(sc.build_arm_alerts(options_arms))
    drift.extend(sc.build_missing_file_alerts(rules))
    swing_alert_inputs = dict(swing.get("_alerts_inputs") or {})
    options_alert_inputs = dict(options.get("_alerts_inputs") or {})
    derived = build_derived_signals(
        swing, options, options_arms, swing_alert_inputs, options_alert_inputs
    )
    swing.pop("_alerts_inputs", None)
    options.pop("_alerts_inputs", None)

    liveness = agent_liveness(rules)
    stale = [a["agent"] for a in liveness if a.get("stale")]

    return {
        "date": day,
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "operator_context": OPERATOR_CONTEXT,
        "derived_signals": derived,
        "promotion_gate": load_promotion_gate_summary(rules),
        "scorecard_summary": sc.compact_summary(
            {
                "date": day,
                "swing": swing,
                "options": options,
                "options_arms": options_arms,
                "drift_alerts": drift,
            }
        ),
        "drift_alerts": drift,
        "swing": swing,
        "options": options,
        "options_arms": options_arms,
        "agent_liveness": liveness,
        "stale_agents": stale,
        "journal_today": journal_events_today(rules, day),
        "public_books": public_books(rules),
    }


def openrouter_chat(messages: list[dict], model: str, max_tokens: int = 1200) -> str:
    key = os.environ.get("OPENROUTER_API_KEY", "").strip()
    if not key:
        raise RuntimeError("OPENROUTER_API_KEY unset")
    body = json.dumps(
        {
            "model": model,
            "temperature": 0.2,
            "max_tokens": max_tokens,
            "messages": messages,
        }
    ).encode()
    req = urllib.request.Request(
        "https://openrouter.ai/api/v1/chat/completions",
        data=body,
        headers={
            "Authorization": f"Bearer {key}",
            "Content-Type": "application/json",
            "HTTP-Referer": "https://github.com/bvelasquez/schwab-api-cli",
        },
    )
    with urllib.request.urlopen(req, timeout=120) as resp:
        doc = json.loads(resp.read().decode())
    return doc["choices"][0]["message"]["content"].strip()


SYSTEM_PROMPT = """You are a paper-trading operations analyst for the Jarvis lab host.
Books: Schwab simulate (options pilot 8709, arms O0–O8, swing 9947) and agent-trader Public paper JSON books.

The FACT PACK includes operator_context (what is already automated) and derived_signals (precomputed).
Use those before inventing new infrastructure.

Hard rules:
- Read-only: no live orders, no "pause entries" unless citing an existing drift_alert or halt flag in data.
- Never recommend building a generic "watchdog" — point to scorecard drift_alerts, agent-health, or paper-host-watchdog.
- Never recommend pausing/demoting arm O3 (or other arms) solely for low avg credit/width; check arm_roles (O3 is a short-DTE negative control).
- Pilot 8709 is not promoted via options_promotion_gate.py; do not conflate pilot PnL with arm pass/fail.
- When options_pilot_small_sample or low arm closed_trades: say "insufficient N" and defer rule changes.
- Swing: compare stop_share_last10_pct vs lifetime stop mix; mention realized_usd_by_profile when present.
- Recommendations must be specific (script name, YAML knob, or "monitor drift alert X") — max 3 items, prioritized P0–P2.
- Skip recommendations that duplicate what operator_context says is already covered unless metrics worsened."""

CRITIQUE_PROMPT = """Review your draft against operator_context and derived_signals.
Delete any recommendation that:
- Proposes new watchdog/monitoring already listed in operator_context
- Pauses arms for designed-low credit floors (especially O3)
- Changes swing stops based only on lifetime stop % without citing stop_share_last10_pct
- Treats pilot promotion like an arm gate pass/fail

Then write a REVISED brief. Recommendations: at most 3, numbered, each one line with (P0|P1|P2) prefix.
If nothing actionable beyond drift_alerts, say "No new ops actions" and list alerts only."""

TELEGRAM_FORMAT_PROMPT = """Format for Telegram: plain text only, no markdown tables, no **bold**.
Max {max_chars} characters. Sections (use these emojis only):
📊 Health & session
📈 Performance (pilot vs arms vs swing vs SPY — note small N when applicable)
⚠️ Drift alerts (verbatim from fact pack if any)
✅ Ops actions (0–3 items, P0/P1/P2 prefix; or "None — monitoring only")
Do NOT include a generic "Recommendations" essay. End with: — Jarvis morning review"""


def multi_turn_review(fact_pack: dict, model: str) -> dict[str, str]:
    fact_json = json.dumps(fact_pack, default=str)
    if len(fact_json) > 14000:
        fact_json = fact_json[:14000] + "\n…(truncated)"

    messages: list[dict] = [
        {"role": "system", "content": SYSTEM_PROMPT},
        {
            "role": "user",
            "content": (
                "Analyze the FACT PACK. Start with operator_context and derived_signals. "
                "Cover: agent liveness, journal_today, effectiveness vs SPY, pilot vs arms "
                "(correlated entries), swing funnel/rejections, drift_alerts, public_books, "
                "promotion_gate if present. Draft at most 3 ops recommendations.\n\n"
                f"FACT PACK:\n{fact_json}"
            ),
        },
    ]
    draft = openrouter_chat(messages, model, max_tokens=1200)
    messages.append({"role": "assistant", "content": draft})
    messages.append({"role": "user", "content": CRITIQUE_PROMPT})
    revised = openrouter_chat(messages, model, max_tokens=1200)
    messages.append({"role": "assistant", "content": revised})
    messages.append(
        {
            "role": "user",
            "content": TELEGRAM_FORMAT_PROMPT.format(max_chars=TELEGRAM_MAX),
        }
    )
    telegram = openrouter_chat(messages, model, max_tokens=900)
    if len(telegram) > TELEGRAM_MAX:
        telegram = telegram[: TELEGRAM_MAX - 20] + "\n…(trimmed)"
    return {"draft": draft, "revised": revised, "telegram": telegram}


def fallback_telegram(fact_pack: dict) -> str:
    lines = [
        "📊 Jarvis paper (no LLM)",
        fact_pack["scorecard_summary"],
        "",
    ]
    if fact_pack.get("stale_agents"):
        lines.append("⚙️ STALE: " + ", ".join(fact_pack["stale_agents"]))
    else:
        lines.append("⚙️ Agents: all fresh")
    ev = fact_pack.get("journal_today") or []
    if ev:
        lines.append(f"📈 Today: {len(ev)} journal events")
        for e in ev[-6:]:
            if e.get("type") == "sim_exit_filled":
                lines.append(
                    f"  exit {e.get('arm')} {e.get('underlying')} "
                    f"{e.get('exit_reason')} ${e.get('pnl_usd')}"
                )
            elif e.get("type") == "sim_entry_filled":
                lines.append(f"  entry {e.get('arm')} credit={e.get('entry_credit')}")
    alerts = fact_pack.get("drift_alerts") or []
    if alerts:
        lines.append("⚠️ " + "; ".join(alerts[:3]))
    lines.append("— Jarvis morning review")
    return "\n".join(lines)[:TELEGRAM_MAX]


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Multi-turn morning paper review + optional Telegram")
    p.add_argument("--repo", type=Path, default=Path.home() / "projects/schwabinvestbot")
    p.add_argument("--date", default=None, help="UTC calendar day for journals (default: today UTC)")
    p.add_argument("--model", default=os.environ.get("MORNING_REVIEW_MODEL", DEFAULT_MODEL))
    p.add_argument("--no-llm", action="store_true", help="Skip OpenRouter; use scorecard-only Telegram body")
    p.add_argument("--telegram", action="store_true", help="Send final message to Telegram")
    p.add_argument("--dry-run", action="store_true", help="Print Telegram body only; do not send")
    return p.parse_args()


def main() -> int:
    args = parse_args()
    repo = args.repo.expanduser().resolve()
    day = args.date or dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d")

    fact_pack = build_fact_pack(repo, day)
    sc = load_scorecard(repo)

    review: dict[str, str]
    if args.no_llm:
        review = {"telegram": fallback_telegram(fact_pack), "draft": "", "revised": ""}
    else:
        try:
            review = multi_turn_review(fact_pack, args.model)
        except Exception as exc:
            print(f"LLM review failed ({exc}); using fallback", file=sys.stderr)
            review = {"telegram": fallback_telegram(fact_pack), "draft": "", "revised": f"(llm error: {exc})"}

    out_dir = repo / "rules" / "scorecards"
    out_dir.mkdir(parents=True, exist_ok=True)
    artifact = {
        "date": day,
        "fact_pack": fact_pack,
        "review": review,
        "model": args.model if not args.no_llm else None,
    }
    md_path = out_dir / f"morning-review-{day}.md"
    md_body = (
        f"# Morning review {day}\n\n"
        f"## Telegram\n\n{review['telegram']}\n\n"
        f"## Revised analysis\n\n{review.get('revised') or '(none)'}\n"
    )
    sc.write_atomic(md_path, md_body)
    sc.write_atomic(out_dir / f"morning-review-{day}.json", json.dumps(artifact, indent=2) + "\n")

    print(review["telegram"])
    if args.telegram and not args.dry_run:
        env_file = Path.home() / ".config/environment.d/schwab-paper.conf"
        sc.send_telegram(review["telegram"], env_file)
    return 0


if __name__ == "__main__":
    sys.exit(main())
