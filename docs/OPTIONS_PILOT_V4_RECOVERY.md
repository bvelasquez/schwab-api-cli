# Options pilot v4 recovery (IRA 8709)

**Status:** paper / `--simulate` only until all promotion gates pass.

Start the agent:

```bash
./option-start.sh
```

That script always runs `schwab watch rules/options-pilot-8709.yaml --simulate` and strips `--trust` / `--yes`.
## What went wrong (v3 live)

- **Exit path mismatch:** backtest closed mostly at **50% profit**; live closed at **stops/thesis** with almost no profit targets.
- **Fail-open LLM:** `skip` on gate failures (e.g. GLD inside 1σ) still opened trades.
- **Structure/universe:** iron condors and **fallback** names (IWM, GLD) in a **one-position** sleeve amplified losses.
- **Stop design:** late-armed **2.5×** stop produced **large** tail losses (GLD).

## v4 rule changes (already in `rules/options-pilot-8709.yaml`)

| Area | Change |
|------|--------|
| Universe | **QQQ + SPY only** |
| Structures | **Vertical credits only** (iron condor **off**) |
| Regime | `high_vol_chop` → **put_credit** (no condor) |
| LLM entries | **`fail_open_on_llm_defer: false`** — skip/defer blocks entry |
| Redeploy | **`promote_redeploy_symbol: false`**; **24h** redeploy cooldown |
| Stops | **2× credit**, **always armed** (`stop_loss_require_short_otm_below_pct: null`) |
| Paper | **`simulation.starting_budget_usd: 4000`** |

## v4.1 rule upgrades (2026-08-06, backtested)

Implemented P0-P3 enhancements; all validated by a fresh synthetic backtest
(Jan 2025 – Aug 2026, 392 trading days):

| Variant | Trades | P/L | ROI | Win% | MaxDD | Stops |
|---|---|---|---|---|---|---|
| v4 baseline | 33 | +$220 | 5.5% | 67% | 6.0% | 7 |
| **v4.1 (shipped)** | **11** | **+$298** | **7.4%** | **91%** | **1.8%** | **1** |

### New mechanical gates (code + rules)

- **Put-credit guard** (`regime.put_credit_guard`): when VIX ≥ 18 AND benchmark below its
  20DMA, no new put credits. Backtest showed every historical stop lived at VIX 18–22.
- **RSI(2) timing** (`put_credit_max_rsi2: 45`, `call_credit_min_rsi2: 55`): no puts into a
  rip, no calls into a dump — sell premium into 2-day extremes only. Fail-closed on missing candles.
- **`high_vol_chop → call_credit`** (was put_credit): chop below the 50DMA is bearish tilt;
  selling puts there was the path to every stop.
- **Post-stop caution** (`entry_policy.post_stop_tightening`): 14d after a stop, entries require
  IV/RV ≥ 1.25 and short OTM ≥ 6%. Prevents stacking losses after a stop.
- **Profit target 60%** (was 50%): sweep showed 40/50/60 → 60 captures the same high-quality
  trades longer (+$298 vs +$256).
- **Condor 1σ gate** (`iron_condor.require_shorts_outside_1sigma`): both wings must clear the
  expected move before any condor (for re-enable).
- **Post-event IV-harvest window** (`risk.post_event_entry_window_days: 2`): flags the 2 days
  after FOMC/CPI/NFP for LLM context (premium typically rich; entries resume mechanically).
- **Paper slippage** (`simulation.fill_slippage_pct: 5.0`): entry credit −5%, exit debit +5%.
- **Drawdown halt 10%** (was 15%).

Backtest caveat: synthetic BS marks; the 11-trade sample is small — the sim run now is the
real test. Re-run the sweep before trusting per-trade differences:

```bash
schwab agent backtest run --rules-file rules/options-pilot-8709.yaml --from 2025-01-01 --fresh --json
schwab agent backtest report --rules-file rules/options-pilot-8709.yaml --json
```

## Aggressive correction plan (phases)

### Phase 0 — Now (capital off)

- [x] `option-start.sh` → **`--simulate` only**
- [x] v4 rules above
- [ ] Stop any **live** background daemon: `schwab agent stop rules/options-pilot-8709.yaml`
- [ ] Run paper TUI: `./option-start.sh`

Paper state file: `rules/agent-sim-state-options-pilot-8709.json` (separate from live `agent-state-options-pilot-8709.json`).

### Phase 1 — Prove exit economics (2–4 weeks paper)

**Goal:** ≥60% of closes are **`profit_target`** or small thesis wins; **stop_loss** ≤25% of closes.

- Track each sim close: reason, P/L % of credit, underlying.
- Weekly: `schwab agent scorecard rules/options-pilot-8709.yaml --simulate --json`
- If stops dominate again → tighten **entry** (raise `min_iv_rv_ratio` to 1.25, `min_short_otm_pct` to 6%) before touching stops again.

### Phase 2 — Entry quality gates (paper)

**Goal:** no entries with `short_strike_inside_1sigma: true` in `market_context`.

- Require LLM **`proceed`** (not merely “not vetoed”).
- Optional: raise `min_pop_pct` to 75 and `min_credit` on QQQ to 0.55 after 10+ sim trades.

### Phase 3 — Synthetic backtest sanity

Re-run BS backtest **after** v4 YAML changes; compare exit mix to paper.

```bash
schwab agent backtest run --rules-file rules/options-pilot-8709.yaml --from 2025-01-01 --fresh --json
schwab agent backtest report --rules-file rules/options-pilot-8709.yaml --json
```

If backtest is negative on v4, **do not** promote to live.

### Phase 4 — Promotion gates (all required for live)

1. **≥15** simulated round trips on v4 rules.
2. Sim **realized P/L ≥ 0** and **max drawdown ≤ 8%** of $4k sleeve.
3. **Win rate ≥ 55%** with **avg win ≥ avg loss** (absolute USD).
4. **`profit_target` exits ≥ 40%** of closes.
5. **Zero** entries where mechanical context shows short inside 1σ.
6. Manual sign-off after reviewing `agent-sim-journal-options-pilot-8709.jsonl`.

### Phase 5 — Live re-enable (explicit, not via `option-start.sh`)

Create a separate operator script or one-off command — **do not** remove `--simulate` from `option-start.sh` until v5:

```bash
schwab watch rules/options-pilot-8709.yaml --trust --yes
```

Start with **one** live trade cap unchanged (`max_trades_per_day: 1`).

## Code / ops follow-ups (backlog)

- Iron condor: require **both** wings outside 1σ before re-enabling — gate now implemented
  (`iron_condor.require_shorts_outside_1sigma`), condor still disabled in v4.1.
- Sim slippage model — shipped in v4.1 (`simulation.fill_slippage_pct: 5.0`).
- Alert if `option-start.sh` pattern not used and live 8709 agent is running.

## Two-sleeve rotation (P2.8, optional)

`rules/options-pilot-9947.yaml` is a separate, broader universe (12 liquid ETFs, $2-wide
spreads, margin account) that can run as a second paper sleeve alongside 8709. Run it with
its own rules file and sim state; keep 8709 as the index sleeve. Do not point both at the
same account hash with `max_open_positions` that can overlap the same names.

## References

- [OPTIONS_RULES.md](OPTIONS_RULES.md) — `--simulate` vs live
- Live P/L history: `rules/agent-state-options-pilot-8709.json` → `cumulative_realized_pnl_usd` (~−$212 as of 2026-08-05)
