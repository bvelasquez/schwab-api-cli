# Agent schedule: regular hours, overnight, at open

The options agent runs **non-stop** with three session modes. Cost and behavior differ by mode.

## Session modes

| Mode | When | Tick interval | Schwab API | LLM | Trades |
|------|------|---------------|------------|-----|--------|
| **regular** | Option market open | `tick_interval_seconds` (120s) | Chains, positions, hours | Selection + monitor | Yes |
| **overnight** | Closed + `overnight.enabled` | `overnight.tick_interval_seconds` (3600s) | Positions reconcile only | Web digest (optional) | No |
| **idle** | Closed + overnight disabled | `tick_interval_seconds` | Positions + hours | None | No |

## Regular hours (9:30–4:00 ET options)

Every tick:

1. Reconcile positions with Schwab
2. Mechanical exits (50% profit, 2× credit stop, 21 DTE) using live chains
3. Entry scan (unless daily cap / risk limits)
4. Monitor LLM every `llm.review_every_ticks` **regular** ticks (not overnight wakes)

`regular_tick_count` tracks only regular-session ticks so overnight polling does not skew monitor LLM timing.

## Overnight / pre-market

When `schedule.overnight.enabled: true`:

- Agent **keeps running** after the close
- Wakes every `overnight.tick_interval_seconds` (default **1 hour**)
- Each wake: reconcile positions (know what's still open)
- **Web digest** (`overnight.web_digest`, uses `llm.web_model`): macro/news playbook for the open
- **No** option chain calls, **no** entries, **no** mechanical exits (marks are stale)
- Playbook saved to `agent-state-*.json` → `open_playbook`
- Telegram: only when `risk_alerts` non-empty if `alert_on_risk_only: true`

Skip digest when flat: `skip_llm_when_flat: true` (no positions → no LLM cost).

### Example overnight config

```yaml
schedule:
  tick_interval_seconds: 120
  market_hours_only: true
  timezone: America/New_York
  overnight:
    enabled: true
    tick_interval_seconds: 3600   # 1/hour overnight
    web_digest: true
    skip_llm_when_flat: true
    alert_on_risk_only: true
```

### Token budget (rough)

| Phase | Calls | Model | ~Cost |
|-------|-------|-------|-------|
| Regular monitor | every ~10 min | gemini-flash | low |
| Regular selection | when setup exists | claude-sonnet | medium |
| Overnight digest | 1/hour (if positions) | perplexity/sonar | medium |

Overnight: ~6–8 web digests per night with one open position ≈ **$0.05–0.20/night** (varies by model).

## At market open

First **regular** tick after overnight:

1. Flagged `at_open: true` in tick output
2. Telegram (if enabled): summarizes overnight `open_playbook`
3. Full evaluation: live marks, mechanical exits, entry scan
4. Regular LLM monitor receives `open_playbook` in context

## State fields

| Field | Purpose |
|-------|---------|
| `last_session` | `regular` \| `overnight` \| `idle` |
| `regular_tick_count` | Monitor LLM cadence |
| `last_overnight_digest_at` | Throttle web digest |
| `open_playbook` | Latest overnight playbook for at-open handoff |

## Phase 2 (not yet implemented)

- Fixed digest times ET (`18:00`, `06:00`, `08:30`) in addition to interval
- Pre-market-only faster interval 8:00–9:30 ET
- `open_playbook` → automatic “close at open” hints (still no auto-trade unless `allow_llm_exits`)

## Run

```bash
schwab agent validate rules/options-pilot-8709.yaml
schwab agent run rules/options-pilot-8709.yaml --trust --yes
```

Overnight ticks show `overnight` session in console; regular ticks show `regular`.
