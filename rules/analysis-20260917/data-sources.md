# Data sources and their real limits

**Date:** 2026-09-17 · verified live with the keys in `.env`

## What actually feeds the backtests
The `.backtest-cache-*.json` files are hydrated from **Schwab's own market-data API**
(`backtest/prefetch.rs` → `schwab_market_data::MarketDataApi`, `cache.ingest_schwab_history`,
`periodType=year` / `frequencyType=daily`, chunked by calendar year). Polygon and FMP are **not** part
of the price path.

## Verified state of each key

| source | status | evidence |
|---|---|---|
| **Schwab** | working | 2-year daily bars for 160 symbols already cached and reproducible |
| **Polygon** (key rotated 2026-09-17) | **end-of-day only — free tier** | `/v1/marketstatus/now` → 200; `1/day` → `200 DELAYED` with real OHLCV; `1/hour` and `1/minute` → **`403 NOT_AUTHORIZED`** |
| **FMP** | **key invalid** | `401 Invalid API KEY`; the legacy `historical-price-full` endpoint also returns `403` |
| **OPENROUTER** | working | used by the LLM layer |

Note: Polygon's error body referenced `massive.com/pricing` (the service is rebranding). A burst of 160
unthrottled requests also returned `429 exceeded the maximum requests per minute`; `/tmp/fetch_polygon.py`
paces at `time.sleep(0.4)` ≈ 150 req/min, tuned for a tier this key does not have.

## Massive (ex-Polygon) tiers — verified against the pricing page
| tier | price | minute aggregates |
|---|---|---|
| Stocks Basic | $0 | **no** (end-of-day only) — confirmed empirically above |
| Stocks Starter | $29/mo ($23.20 annual) | **yes** — unlimited calls, 5y history, 15-min delayed |
| Stocks Developer | $79/mo | yes, plus trades (10y) |
| Stocks Advanced | $199/mo | yes, plus real-time quotes (20y+) |

The free tier's daily bars are redundant here — Schwab already supplies daily history.

## Do you need to pay? No, not yet
**Minute bars are already free in this stack.** The live rules carry
`intraday_history: {period_type: day, period: 5, frequency_type: minute}` and the client passes
`frequency_type` straight through (`market_ctx.rs:134`, `rules.rs:645`), so the bot already fetches
Schwab 1-minute data. Extending the lookback is a config value, not new plumbing.

What $29/mo would buy is **historical depth for intraday backtesting** (5 years of minute bars) — real
value, but there is no intraday hypothesis defined yet to test, and it is not needed for the live path.

The economics decide it at the current sleeve size:

| | value |
|---|---|
| subscription | $348/yr ($278/yr annual) |
| sleeve | $4,000 |
| cost as share of capital | **8.7%/yr** (7.0% annual billing) |
| strategy return at the recommended config | ~+32.6% over 2y ≈ **~15%/yr** |
| share of the edge consumed | **~57%** |

Buy it when the sleeve reaches ~$20k (then $348/yr = 1.7%), or when a specific intraday hypothesis is
blocked on multi-year minute history. If buying, Starter is the correct tier — Developer/Advanced only
add trades/real-time, which a backtest workflow does not need.

## Free-path limits (state honestly)
- Schwab minute lookback is short — the rules ask for 5 days; the practical ceiling is on the order of a
  **month**, so there is no multi-year intraday backtest and even the 11-week live window is only
  partly reachable.
- Minute bars are OHLCV: no bid/ask. For **friction modeling**, sample the Schwab quotes endpoint during
  market hours to build an empirical spread table for the watchlist — free, and better than a guess.
