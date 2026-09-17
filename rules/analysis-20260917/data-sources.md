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
| **Polygon** | **no price-data entitlement** | `/v1/marketstatus/now` → 200, but `/v2/aggs/ticker/AAPL/range/1/{day,hour,minute}/...` → `403 NOT_AUTHORIZED` for **all three** timespans |
| **FMP** | **key invalid** | `401 Invalid API KEY`; the legacy `historical-price-full` endpoint also returns `403` |
| **OPENROUTER** | working | used by the LLM layer |

Note: Polygon's error body referenced `massive.com/pricing` (the service is rebranding); a burst of 160
unthrottled requests also returned `429 exceeded the maximum requests per minute`. `/tmp/fetch_polygon.py`
paces at `time.sleep(0.4)` ≈ 150 req/min, tuned for a tier this key does not have.

## Consequence for intraday work
An intraday strategy **cannot** be backtested over the 2-year window with the current sources: Polygon
is unentitled and FMP is invalid. The available path is the Schwab client itself — its price-history
endpoint supports `frequencyType=minute` (`periodType=day`), which is free with the existing auth but
has a **~30-day lookback ceiling**. That is enough to paper-forward an intraday strategy over recent
weeks; it is **not** enough for statistically meaningful intraday backtesting.

Before building anything intraday, decide the data question:
1. stay on Schwab minute bars → validate forward only, accept the ~30-day ceiling; or
2. buy a minute-bar plan (Polygon/Massive paid tier, or equivalent) → enables real intraday backtests.

Also note the minute-bar path is the prerequisite for **friction modeling** (spread/slippage), which is
the top item on the go-live checklist.
