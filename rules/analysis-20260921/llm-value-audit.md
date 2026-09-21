# Is the in-loop LLM worth its tokens? — measured audit

Date: 2026-09-21 (Monday, pre-market). Host: jarvis. Swing journal 200 MB,
2026-06-29 → 2026-09-21. Scripts: `/tmp/llm_audit.py`, `llm_schema.py`, `llm_value.py`,
`llm_join.py`, `llm_calls.py` (jarvis; re-derivable — see "Reproduce" below).

## 1. Cost — measured, not estimated

There is **no token accounting anywhere**: 0 occurrences of `prompt_tokens` /
`completion_tokens` / `usage` / cost fields in the 200 MB swing journal *or* the agents'
journald logs. The only token proxy is payload size (median review response 5,111 chars
≈ 1.3k out-tokens).

The authoritative number is the agents' own OpenRouter key
(`~/.config/environment.d/schwab-paper.conf`, key hash `01fdcf54b0e8`, read via
`GET https://openrouter.ai/api/v1/key` — read-only, key never printed):

| field | value |
|---|---|
| `usage` (lifetime) | **$18.64** |
| `usage_monthly` | **$2.885** |
| `usage_weekly` / `usage_daily` | $0.1066 (week began today) |
| `limit` / `limit_remaining` | $3.00 / $2.893 |

Attribution: this key exists **only** in `schwab-paper.conf` — no other env file, Hermes
profile, or furoshiki config on jarvis uses OpenRouter, and the Mac's Hermes key is a
different key (hash `30385d8abb97`). So **$2.885/month is the two paper agents alone**,
≈ **$0.13 per trading day**.

## 2. Volume — actual calls, not ticks

A cached review repeats its payload verbatim, so distinct-payload counting is the call
count. Swing agent: **27.4 LLM calls/day** = 18.0 reviews + 9.3 learn calls.
Totals over 57 active days: **1,028 reviews + 531 learn = 1,559 calls**.

`rule_patch_proposed` = **531**; `rule_auto_applied` = **1**; every sampled patch payload
carries `"applied": []`. → **531/531 learn calls produced a patch that was discarded.**
With `allow_rule_adaptation: false` that call cannot have any effect by construction.

## 3. Measured decision footprint (pre-freeze, 2026-06-29 → 09-18)

- **Veto:** 245 `llm_veto_or_missing_review` blocks — but only **29 distinct
  (symbol, day)** pairs; **181 of 245 (74%) are AMD** re-vetoed tick after tick. The
  "38% of admissible candidates blocked" figure is mostly one symbol being re-decided.
- **Profiles:** 23 `profile_changed` events.
- **Exits:** 37 closed trades, mean **−1.11%/trade, CI95 [−2.72, +0.50]** (spans zero).
  Exit reasons: stop_loss 22, profit_target 9, thesis_rs 4, thesis_regime 2.
- 4 ticks carry an `error` inside the `llm` block → LLM calls do fail in production.

### Expectancy split by the profile SOURCE active at entry (37/37 joined)

| segment | source | profile | n | mean pnl_pct | CI95 |
|---|---|---|---|---|---|
| pre | llm | elevated_vol | 8 | −0.81% | |
| pre | llm | low_vol_trend | 15 | −0.19% | |
| pre | regime | elevated_vol | 12 | **−2.56%** | [−5.09, −0.03] |
| pre | regime | low_vol_trend | 1 | — | |

Blended: **LLM-sourced −0.41%/trade (n=23) vs regime-sourced −2.56%/trade (n=12)**.
This weakly *contradicts* the premise that LLM profile selection hurt — but n is tiny,
timing is confounded (regime-sourced trades cluster in one market phase), and the CIs
overlap. It is not evidence for either side; it is evidence that the freeze of
`llm_profile_select` was not evidence-based.

## 4. The real finding — a missing review is a hard stop in live mode

`runner.rs:995-1000` + `llm.rs:549-561`:

```rust
let llm_ok = match &llm_review {
    Some(review) => candidate_approved(review, symbol, tick_rules.llm.veto_entries),
    None if rules.llm.enabled && !runtime.dry_run && !runtime.simulate => false,  // live: BLOCK
    None if rules.llm.enabled && runtime.dry_run => true,
    None => true,                                                                 // simulate: allow
};
```
`candidate_approved(..., veto=false)` returns `true` unconditionally, so the frozen veto
is provably inert (measured: 245 blocks pre-freeze → **0 post-freeze**).

But when `llm.enabled` is true in **live** (non-dry-run), *no review* ⇒ *no entries*, with
the **same journal reason string as a veto**. An exhausted key cap (a $3 limit with
~$2.89 still unused on 2026-09-21, so not imminent), an OpenRouter outage, or a malformed
response silently stops the bot trading and reads in the
journal exactly like "the LLM said no". Under `--simulate` (how both paper agents run) a
missing review passes through, so this bites live only.

## 5. Verdict

- **"Burning tokens" is not the problem.** $2.9/month, $0.13/trading day, $18.64 lifetime.
  Turning the whole thing off saves $35/year.
- **But a third of the spend is provably inert**: 531/531 learn calls → 0 applied patches.
- The reviews (18/day, ~2/3 of spend) are now decision-empty for entries (veto frozen);
  their residual value is the market commentary / risk alerts Barry reads — informational,
  not P&L.
- The LLM is **not** the dominant term in the bot's results: −1.11%/trade overall with CIs
  spanning zero, and the one split we can compute points *away* from "the LLM was the
  drag". The drag is the entry/exit rule geometry (61% of exits are stops).

## Proposals (need Barry's OK — jarvis rules/behavior change)

1. **Skip the learn call when `allow_rule_adaptation: false`** (or set
   `llm.learn_every_ticks: 0`). Removes 9.3 calls/day and an API dependency that has had
   zero possible effect across 531 attempts. Evidence: §2.
2. **Live safety:** in live mode, don't map "review unavailable" onto "veto" — either
   proceed (as simulate does) or fail loudly and distinct (`llm_unavailable`), so a key cap
   can't silently halt trading. Evidence: §4.
3. **Keep the reviews**; don't spend effort shaving $2.9/mo — the cost is 2 orders of
   magnitude below the sleeve's risk budget.
4. **Re-open the `llm_profile_select` freeze question** with a proper out-of-sample window
   rather than leaving a decision in place that the only available split contradicts.
   Evidence: §3.

## Reproduce

```
scp /tmp/{llm_audit,llm_schema,llm_value,llm_join,llm_calls}.py jarvis:/tmp/
ssh jarvis 'cd ~/projects/schwabinvestbot && python3 /tmp/llm_calls.py'
ssh jarvis 'set -a; . $HOME/.config/environment.d/schwab-paper.conf; set +a; \
  curl -s -H "Authorization: Bearer $OPENROUTER_API_KEY" https://openrouter.ai/api/v1/key'
```

Freeze boundary used throughout: **ts ≥ 2026-09-19** (the reload/restart landed
2026-09-19T00:28Z = 2026-09-18 17:28 PDT; Friday's session ticks were still pre-freeze).
