# The lever: risk budget, not signals (p3/p4 sweeps)

**Date:** 2026-09-17 · Branch `feat/require-below-sma` · 58-symbol watchlist, $4,000 sleeve,
`backtest run --fresh --no-learn --simulate --yes`, frozen baseline (LLM/veto/adaptation off).
Control = current live geometry. Only the listed fields differ between arms; every arm shares the
2-year daily cache, and every run: exit 0.

## Levers tested and rejected

### 1. Entry gating — no effect (p2, commit f8ff975)
Pullback gate (`require_below_sma: [9]`) binds correctly (32/32 fills at/below SMA9 vs 29/59 above in
the control) but leaves per-trade expectancy unchanged (+$6.86 → +$6.33) and halves the trade count, so
total P&L halves. Closed.

### 2. Exit geometry — every change is worse (p3)

| arm | geometry | trades | ROI% | expectancy | P&L $ | maxDD% | ret/deployed |
|---|---|---|---|---|---|---|---|
| **p3-ctl** | target 2.5xATR / stop 2.0xATR (**R:R 1.25**) | 58 | **+10.27** | **+6.86** | **+398** | **2.49** | **+99.7%** |
| p3-tp35 | target 3.5xATR (R:R 1.75) | 54 | +7.02 | +4.98 | +269 | 4.12 | +67.6% |
| p3-tp45 | target 4.5xATR (R:R 2.25) | 54 | +6.56 | +4.64 | +250 | 4.14 | +58.2% |
| p3-tp35s25 | target 3.5x / stop 2.5xATR | 33 | +5.27 | +6.10 | +201 | 3.55 | +88.4% |

Raising the target does **not** add trades (58→54) — the R:R gate is not the binding admission
constraint — and it converts profit-target exits into stop-outs (28→23→20 targets, 22→23→25 stops) as
targets drift out of reach. The live window collapses for the wider-stop variant (+0.43%, 3 stop-outs,
0 targets). The current geometry is the best of the four; the exit side is already at its optimum in
this space.

### 3. Concurrency — provably inert (p4)
`max_positions` 4→8 and `max_portfolio_heat_pct` 8→32 produced **byte-identical results**
(p4-h8-r2 ≡ p4-h16-r2, p4-h16-r4 ≡ p4-h32-r4). The bot rarely holds more than 1–2 positions, so slots
and the heat ceiling are never the constraint. Whatever limits this strategy, it is not concurrency.

## The lever: `position_size.risk_per_trade_pct`

Diagnosis: one entry carried risk 0.75% of the $4,000 sleeve; the implied stop distance on high-ATR
watchlist names is ~7%, so the notional was ~$426 (10.6% of the sleeve). With 4 slots that is ≤3%
portfolio heat against a **declared 8% ceiling** — the bot declares a risk budget it never spends, and
deploys ~10% of capital (median $286).

| risk/trade | trades | ROI% | P&L $ | maxDD% | headroom to 10% halt | P&L/DD |
|---|---|---|---|---|---|---|
| 0.75% (live) | 58 | +10.27 | +398 | 2.49 | +7.51% | 159.8 |
| 2.00% | 58 | +24.60 | +950 | 6.07 | +3.93% | 156.7 |
| **3.00%** | 58 | **+32.58** | **+1,253** | **7.94** | **+2.06%** | **157.8** |
| 4.00% | 58 | +31.72 | +1,202 | 10.22 | **−0.22%** | 117.6 |
| 6.00% | 17 | **−10.50** | −420 | 12.60 | −2.60% | −33.3 |

Reference: SPY over the same 2-year window is +33.68% with **19.0%** max drawdown.

Live window (11 weeks): 0.75% → −1.08pp vs SPY; 2% → +1.72pp; **3% → +3.34pp**; 4% → +5.78pp.

### The ceiling is a kill-switch, not a slope
At 6% risk/trade the run dies: equity fell 3,779 → 3,580 on **2025-01-14** (−12.6% from the 4,096
peak), tripping `risk.max_drawdown_halt_pct: 10.0`, after which equity is **frozen at 3,580.12 for the
remaining 18 months** and no further entry is ever taken (17 trades total, −10.5%). At 4% the measured
drawdown (10.22%) already crosses the halt threshold. Over-sizing does not just risk more — it turns
the bot off permanently.

### Recommended change (one number)
```
playbook.entry.position_size.risk_per_trade_pct: 0.75 -> 3.0
playbook.entry.position_size.max_position_pct:   18.0 -> 50.0   # else the per-name cap saturates the scale-up
risk.max_portfolio_heat_pct:                     8.0  -> 12.0   # spend the budget the config declares
```
Effect: 2-year P&L **3.1x** (+$398 → +$1,253), ROI +10.27% → +32.58% (index-matching), drawdown 2.49%
→ 7.94% (under half of SPY's 19%), while keeping 2.06% of headroom beneath the 10% kill-switch. The
best risk-adjusted point in the sweep; 4% and above trades return for ruin risk.

## Caveats before real cash
1. **This scales an edge; it does not create one.** The per-trade edge is t≈2.62 on 58 trades. Losses
   scale with the win.
2. **Friction is still unmodeled** — the harness fills at the close with no slippage and no commission.
   At 3x size, friction costs 3x more. This is the first thing to fix before live cash.
3. **Frozen config ≠ live config.** All arms ran with LLM veto, learning and adaptation disabled. The
   live adaptive layer's contribution is still unmeasured — and the live-loop diagnosis suggested its
   extra entries were the losing ones. Decide deliberately whether it runs in production.
4. **The live window is 11 weeks / 5 trades.** Far too short to confirm anything.
5. The drawdown halt is **permanent** in these runs — in production that means manual intervention.

## Reproduce
```
~/.hermes/hermes-agent/venv/bin/python scripts/gen_p3_arms.py   # then scripts/gen_p4_arms.py
bash scripts/run_grid.sh <arm> [<arm> ...]                      # rules/trader-swing-<arm>-9947.yaml
~/.hermes/hermes-agent/venv/bin/python scripts/sum_grid.py p4-
```
Arm rules files and `.backtest-cache-*` are gitignored (`*-9947.yaml`) and are copies of the live
rules with only the arm's fields changed.
