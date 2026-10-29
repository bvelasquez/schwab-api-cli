# Swing lab results (2026-10-23)

Window 2018-01-03 to 2026-10-22 (2513 bars loaded per symbol). Sleeve $4,000, 5 bps slippage per side, idle cash 3%/yr, approximate dividend accrual. Out-of-sample = last third from 2023-11-13.

Ranked by number of checks passed, then Sharpe. Checks: Sharpe above SPY, drawdown below SPY, OOS CAGR positive, still positive at 15 bps, at least 30 trades (or a monthly rotation).

| Strategy | CAGR % | Max DD % | Sharpe | Excess CAGR vs SPY | OOS CAGR % | CAGR @15bps | Trades | Win % | PF | Exposure % | Checks |
|---|---|---|---|---|---|---|---|---|---|---|---|
| Control plus SPY>SMA50/SMA200 regime gate * | 14.3 | 18.7 | 0.74 | +0.8 | 10.9 | 12.0 | 361 | 54 | 1.39 | 75 | 5/5 |
| Quality dip buy: above EMA200, at/below EMA20 * | 15.0 | 29.7 | 0.68 | +1.4 | 14.5 | 12.1 | 556 | 52 | 1.40 | 88 | 5/5 |
| Control: current 9947 entry rule (approx.) * | 13.6 | 24.6 | 0.63 | +0.1 | 11.9 | 10.6 | 473 | 52 | 1.29 | 98 | 5/5 |
| 12-1 momentum, top 5 megacaps, monthly * | 14.2 | 31.5 | 0.55 | +0.7 | 27.2 | 13.6 | 120 | 50 | 2.54 | 80 | 4/5 |
| SPY above SMA200, else cash | 9.3 | 20.1 | 0.54 | -4.2 | 19.0 | 8.6 | 29 | 28 | 2.63 | 80 | 4/5 |
| Donchian 20/10 breakout on liquid ETFs | 8.1 | 11.9 | 0.50 | -5.4 | 13.9 | 6.8 | 287 | 43 | 1.58 | 69 | 4/5 |
| Sector ETF momentum, top 3 monthly | 10.1 | 32.0 | 0.45 | -3.4 | 17.0 | 9.5 | 75 | 55 | 2.05 | 94 | 4/5 |
| Connors RSI(2) on SPY/QQQ | 5.7 | 8.3 | 0.40 | -7.8 | 9.3 | 3.9 | 153 | 67 | 1.78 | 12 | 4/5 |
| Faber 5-asset tactical allocation | 5.8 | 11.7 | 0.38 | -7.7 | 12.1 | 5.6 | 49 | 33 | 1.99 | 66 | 4/5 |
| IBS mean reversion on SPY/QQQ/IWM | 4.9 | 10.1 | 0.27 | -8.6 | 2.1 | 0.1 | 608 | 59 | 1.17 | 23 | 4/5 |
| SPY buy and hold | 13.5 | 33.1 | 0.62 | +0.0 | 21.0 | 13.5 | 0 | 0 | n/a | 95 | 3/5 |
| Global equities momentum (SPY/EFA/AGG) | 8.7 | 33.6 | 0.40 | -4.8 | 22.7 | 8.3 | 17 | 53 | 3.70 | 98 | 3/5 |

`*` stock basket is today's survivors, so results are optimistic (selection look-ahead).

## Calendar-year returns (%)

| Strategy | 2018 | 2019 | 2020 | 2021 | 2022 | 2023 | 2024 | 2025 | 2026 |
|---|---|---|---|---|---|---|---|---|---|
| Control plus SPY>SMA50/SMA200 regime gate | -10.8 | 33.7 | 34.4 | 32.0 | -11.2 | 30.9 | 13.9 | 12.4 | 2.4 |
| Quality dip buy: above EMA200, at/below EMA20 | 0.7 | 27.5 | 23.0 | 28.2 | -8.0 | 26.4 | 44.0 | -7.7 | 8.6 |
| Control: current 9947 entry rule (approx.) | 4.7 | 21.5 | 12.7 | 29.4 | -5.5 | 26.2 | 9.8 | 18.5 | 6.0 |
| 12-1 momentum, top 5 megacaps, monthly | -13.2 | 6.2 | 17.4 | 41.8 | -17.7 | 37.3 | 81.3 | -2.1 | 4.4 |
| SPY above SMA200, else cash | -5.7 | 14.1 | 8.8 | 28.5 | -14.6 | 10.9 | 24.9 | 11.7 | 10.2 |
| Donchian 20/10 breakout on liquid ETFs | -6.1 | 15.3 | 12.8 | 14.8 | -6.1 | 10.5 | 6.8 | 2.7 | 24.7 |
| Sector ETF momentum, top 3 monthly | -4.5 | 16.0 | 5.8 | 11.4 | 11.6 | 9.2 | 11.6 | 4.7 | 25.0 |
| Connors RSI(2) on SPY/QQQ | -2.4 | 3.8 | 5.1 | 12.0 | -0.3 | 5.8 | 15.3 | 6.7 | 4.9 |
| Faber 5-asset tactical allocation | -4.8 | 10.7 | 9.3 | 10.8 | -7.4 | 4.8 | 11.7 | 19.4 | -0.4 |
| IBS mean reversion on SPY/QQQ/IWM | -7.1 | 3.7 | 22.8 | 19.6 | -0.5 | 2.0 | -1.3 | 5.2 | 1.8 |
| SPY buy and hold | -6.5 | 30.0 | 17.2 | 27.8 | -17.4 | 24.4 | 23.5 | 16.9 | 12.5 |
| Global equities momentum (SPY/EFA/AGG) | -6.3 | 15.3 | -3.4 | 28.5 | -15.1 | 9.9 | 24.9 | 17.5 | 13.5 |

## Not backtested

- **Post-earnings-announcement drift**: Needs point-in-time earnings dates and surprises. Only rules/earnings-calendar.json (forward dates) exists. Paper-test it from the calendar going forward instead of backtesting.

## Caveats

- Fixed published parameters, nothing fitted; OOS is a stability check on the last third, not a fit/test split.
- Close-based stops and next-open fills; no intraday gaps through stops beyond the open slippage.
- One bear market (2022) and the 2020 crash in the window; price history is split-adjusted but not dividend-adjusted, so a yield table is used.
- A strategy passing here earns a paper test, nothing more.
