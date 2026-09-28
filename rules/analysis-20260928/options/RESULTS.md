# Options pilot backtest sweep — 2026-09-28
Mechanical replay on **jarvis**, rules copies under this directory only. Live `rules/options-pilot-8709.yaml` and `*state*.json` were **not** touched; no services restarted.
## Setup notes
- **Period:** 2025-01-01 → 2026-09-27 (OHLCV cache prefetched to 2026-09-27).
- **LLM:** `llm.enabled: false` on every variant. The backtest runner has **no LLM code path**; `entry_policy.require_llm_proceed` is **ignored** during replay.
- **Variants:** `bt-v42.yaml`, `bt-v41.yaml`, `bt-credit10.yaml`, `bt-d30.yaml`, `bt-d30-pt50-nostop.yaml` with unique `agent_id`.
- **Artifacts:** journals, state, `run-*.json`, `report-*.json` in this folder.
## Pricing model
| Assumption | Implementation |
|------------|----------------|
| Marks | European Black-Scholes, r=0, same IV=VIX for both legs |
| Strikes | $1 grid; short by delta band; long = short ± max_width |
| IV | Daily VIX close; missing VIX → 18.0 on entry |
| RV | 20d realized vol for IV/RV gates |
| Gaps | No skew/smile, no bid/ask, **fill_slippage_pct not applied**, no watchlist credit merge in replay |

Synthetic v4.x credits ~$0.61–0.79 (12–16% of $5 width) vs paper $0.35–0.43 — credit floors often **non-binding** in sim (`bt-credit10` identical to `bt-v42`).

## Results summary

| Variant | Trades | Wins | Win% | Total PnL | Avg win | Avg loss | BE win% | E[trade] | ~95% CI | Max DD% | Avg credit | Cr/width% |
|---------|-------:|-----:|-----:|----------:|--------:|---------:|--------:|--------:|---------|--------:|-----------:|----------:|
| `bt-v42` | 4 | 4 | 100.0 | $228 | $57.1 | $0.0 | 0.0 | $57.1 | [$47.5, $66.6] | 0.00 | $0.79 | 15.7 |
| `bt-v41` | 17 | 16 | 94.1 | $510 | $38.9 | $-111.3 | 74.1 | $30.0 | [$11.6, $48.4] | 2.52 | $0.63 | 12.6 |
| `bt-credit10` | 4 | 4 | 100.0 | $228 | $57.1 | $0.0 | 0.0 | $57.1 | [$47.5, $66.6] | 0.00 | $0.79 | 15.7 |
| `bt-d30` | 65 | 30 | 46.2 | $754 | $64.0 | $-33.3 | 34.2 | $11.6 | [$-1.5, $24.7] | 4.51 | $1.23 | 24.6 |
| `bt-d30-pt50-nostop` | 65 | 30 | 46.2 | $754 | $64.0 | $-33.3 | 34.2 | $11.6 | [$-1.5, $24.7] | 4.51 | $1.23 | 24.6 |

BE win% = |avg_loss|/(avg_win+|avg_loss|). CI: normal approx on closed-trade PnL.

### Exit reason mix

- **bt-v42:** profit_target=4
- **bt-v41:** dte_close=1, profit_target=14, stop_loss=1, thesis_profit_giveback=1
- **bt-credit10:** profit_target=4
- **bt-d30:** profit_target=27, thesis_delta_breach=2, thesis_near_strike=29, thesis_profit_giveback=7
- **bt-d30-pt50-nostop:** profit_target=27, thesis_delta_breach=2, thesis_near_strike=29, thesis_profit_giveback=7

### Top skip buckets

- **bt-v42:** entry_analytics=585, vix_or_regime_pause=69, put_credit_guard=56, rsi2_gate=44, correlation_group=11
- **bt-v41:** entry_analytics=446, correlation_group=86, max_open_per_underlying=86, vix_or_regime_pause=69, put_credit_guard=42
- **bt-credit10:** entry_analytics=585, vix_or_regime_pause=69, put_credit_guard=56, rsi2_gate=44, correlation_group=11
- **bt-d30:** correlation_group=195, max_open_per_underlying=195, entry_analytics=129, vix_or_regime_pause=69, put_credit_guard=52
- **bt-d30-pt50-nostop:** correlation_group=195, max_open_per_underlying=195, entry_analytics=129, vix_or_regime_pause=69, put_credit_guard=52

## Interpretation

1. **Small n:** bt-v42 / bt-credit10 have only 4 closes — CIs are not meaningful. bt-v41 (17 trades) aligns with prior v4.1 backtest (~18 trades, +$346); this run +$510.

2. **Paper hypothesis:** Lower v4.2 credit floors (7–8% width) imply breakeven WR ≈ POP with little edge. **This sim cannot test that** because BS credits are much richer; tightening floors to 10%/$0.50 changed nothing.

3. **v4.1 vs v4.2 in sim:** Tighter bt-v41 traded *more* (17 vs 4) and PnL higher (+$510 vs +$228). Looser v4.2 delta band yields more post-pick `entry_analytics` rejects (585 vs 446). Not a live recommendation — sim omits slippage and watchlist floors.

4. **bt-d30:** 65 closes, 46% WR, +$754, E ~$11.6 (95% CI roughly -$1 to +$25). Thesis exits dominate. stop_loss_pct 300% identical to 200% (no stops triggered).

5. **Open:** bt-d30* leave 1 open position at sample end (PnL is closed-only).
