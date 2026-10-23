#!/usr/bin/env python3
"""Swing / ETF strategy lab: one daily-bar engine, many published strategies, same costs.

Reads rules/lab-data/daily/*.json (see scripts/swing_lab_fetch.py). Pure Python, no network,
no orders. Every strategy runs on the same engine:

* Signals use data through the close of day t; orders fill at the OPEN of day t+1 (no lookahead).
* Slippage is charged on every fill (default 5 bps per side), applied to the open.
* Starting sleeve is $4,000. Fractional shares. Idle cash earns CASH_RATE_PCT a year. Held positions accrue
  an approximate dividend yield (DIV_YIELD_PCT) so price-only data does not favour cash-heavy strategies.
* Stops and targets are evaluated on closes and executed next open (conservative for stops).
* Parameters are the published defaults, not fitted here. The "out of sample" column is simply the last third
  of the window, a stability check, not a fit/test split.

Commands:
    python3 scripts/swing_lab.py run      [--repo .] [--out-dir rules/analysis-20261023]
    python3 scripts/swing_lab.py catalog  [--repo .]   # writes rules/arms/swing-candidates/*.yaml

Limitations (also written into the results): stock baskets are today's survivors (look-ahead in universe
selection), dividends and cash interest are approximations, no taxes, and a 9-year window has one bear
market (2022) plus the 2020 crash. A result here is a reason to paper-test, not proof.
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import statistics
import sys
from pathlib import Path

START_CAPITAL = 4000.0
CASH_RATE_PCT = 3.0  # manual constant, applied to idle cash for every strategy including baselines
WARMUP = 300  # bars before the first decision (SMA200 / EMA200 / 252-day returns must be settled)
TRADING_DAYS = 252

# Approximate trailing dividend yields (percent per year). Manual constants; only used so that
# holding an ETF is not penalised against holding cash. Not a data feed.
DIV_YIELD_PCT = {
    "SPY": 1.4, "QQQ": 0.6, "IWM": 1.3, "DIA": 1.7, "EFA": 3.0, "EEM": 2.5, "AGG": 3.2, "TLT": 3.6,
    "IEF": 2.8, "GLD": 0.0, "VNQ": 3.8, "BIL": 0.0, "SHY": 3.0, "XLK": 0.7, "XLF": 1.6, "XLE": 3.2,
    "XLV": 1.5, "XLI": 1.4, "XLP": 2.6, "XLU": 2.9, "XLY": 0.8, "XLB": 1.8, "SMH": 0.5,
}
STOCK_YIELD_DEFAULT = 1.0

SECTORS = ["XLK", "XLF", "XLE", "XLV", "XLI", "XLP", "XLU", "XLY", "XLB"]
MEGACAPS = ["AAPL", "MSFT", "NVDA", "AMZN", "GOOGL", "META", "AVGO", "JPM", "XOM", "UNH",
            "COST", "LLY", "WMT", "HD", "PG", "V", "MA", "JNJ", "CVX", "CAT"]
BROAD_ETFS = ["SPY", "QQQ", "IWM", "DIA", "XLK", "XLF", "XLE", "XLV", "XLI", "XLP", "XLU", "XLY",
              "XLB", "SMH", "GLD", "TLT", "EFA"]


# --------------------------------------------------------------------------- data and indicators

class Series:
    """Aligned daily bars plus lazily built indicators for one symbol."""

    def __init__(self, symbol: str, bars: list[list]):
        self.symbol = symbol
        self.dates = [b[0] for b in bars]
        self.o = [b[1] for b in bars]
        self.h = [b[2] for b in bars]
        self.l = [b[3] for b in bars]
        self.c = [b[4] for b in bars]
        self._cache: dict = {}

    def sma(self, n: int) -> list:
        key = ("sma", n)
        if key not in self._cache:
            out = [None] * len(self.c)
            run = 0.0
            for i, v in enumerate(self.c):
                run += v
                if i >= n:
                    run -= self.c[i - n]
                if i >= n - 1:
                    out[i] = run / n
            self._cache[key] = out
        return self._cache[key]

    def ema(self, n: int) -> list:
        key = ("ema", n)
        if key not in self._cache:
            out = [None] * len(self.c)
            if len(self.c) >= n:
                k = 2.0 / (n + 1)
                prev = sum(self.c[:n]) / n
                out[n - 1] = prev
                for i in range(n, len(self.c)):
                    prev = self.c[i] * k + prev * (1 - k)
                    out[i] = prev
            self._cache[key] = out
        return self._cache[key]

    def rsi(self, n: int) -> list:
        key = ("rsi", n)
        if key not in self._cache:
            out = [None] * len(self.c)
            if len(self.c) > n:
                gains = losses = 0.0
                for i in range(1, n + 1):
                    d = self.c[i] - self.c[i - 1]
                    gains += max(d, 0.0)
                    losses += max(-d, 0.0)
                ag, al = gains / n, losses / n
                out[n] = 100.0 if al == 0 else 100.0 - 100.0 / (1 + ag / al)
                for i in range(n + 1, len(self.c)):
                    d = self.c[i] - self.c[i - 1]
                    ag = (ag * (n - 1) + max(d, 0.0)) / n
                    al = (al * (n - 1) + max(-d, 0.0)) / n
                    out[i] = 100.0 if al == 0 else 100.0 - 100.0 / (1 + ag / al)
            self._cache[key] = out
        return self._cache[key]

    def ret(self, n: int) -> list:
        key = ("ret", n)
        if key not in self._cache:
            out = [None] * len(self.c)
            for i in range(n, len(self.c)):
                out[i] = self.c[i] / self.c[i - n] - 1.0
            self._cache[key] = out
        return self._cache[key]

    def high_before(self, n: int) -> list:
        """Highest high of the n bars BEFORE today (excludes today)."""
        key = ("hb", n)
        if key not in self._cache:
            out = [None] * len(self.c)
            for i in range(n, len(self.c)):
                out[i] = max(self.h[i - n:i])
            self._cache[key] = out
        return self._cache[key]

    def low_before(self, n: int) -> list:
        key = ("lb", n)
        if key not in self._cache:
            out = [None] * len(self.c)
            for i in range(n, len(self.c)):
                out[i] = min(self.l[i - n:i])
            self._cache[key] = out
        return self._cache[key]

    def ibs(self, i: int):
        rng = self.h[i] - self.l[i]
        return None if rng <= 0 else (self.c[i] - self.l[i]) / rng


def load_data(repo: Path) -> tuple[dict[str, Series], list[str]]:
    folder = repo / "rules" / "lab-data" / "daily"
    raw = {}
    for p in sorted(folder.glob("*.json")):
        doc = json.loads(p.read_text())
        raw[doc["symbol"]] = doc["bars"]
    if "SPY" not in raw:
        raise SystemExit(f"no SPY bars under {folder}; run scripts/swing_lab_fetch.py first")
    dates = [b[0] for b in raw["SPY"]]
    index = {d: i for i, d in enumerate(dates)}
    data: dict[str, Series] = {}
    for sym, bars in raw.items():
        by_date = {b[0]: b for b in bars}
        aligned, last = [], None
        for d in dates:
            row = by_date.get(d, last)
            if row is None:
                continue
            last = row
            aligned.append([d] + row[1:])
        if len(aligned) == len(dates):
            data[sym] = Series(sym, aligned)
    suspicious = []
    for sym, s in data.items():
        for i in range(1, len(s.c)):
            if abs(s.c[i] / s.c[i - 1] - 1) > 0.35:
                suspicious.append(f"{sym} {s.dates[i]} {s.c[i-1]:.2f}->{s.c[i]:.2f}")
    return data, suspicious


# --------------------------------------------------------------------------- engine

class Result:
    def __init__(self):
        self.equity: list[float] = []
        self.invested: list[float] = []
        self.dates: list[str] = []
        self.trades: list[dict] = []


class Engine:
    def __init__(self, data: dict[str, Series], slippage_bps: float = 5.0):
        self.data = data
        self.dates = data["SPY"].dates
        self.n = len(self.dates)
        self.slip = slippage_bps / 10000.0

    def run(self, strat, start: int | None = None) -> Result:
        start = WARMUP if start is None else start
        data = self.data
        cash = START_CAPITAL
        shares: dict[str, float] = {}
        prev_target: dict[str, float] = {}
        pending = None
        open_trades: dict[str, dict] = {}
        res = Result()
        daily_rate = CASH_RATE_PCT / 100.0 / TRADING_DAYS
        strat.reset()

        for i in range(start, self.n):
            cash += cash * daily_rate
            if pending is not None:
                eq_open = cash + sum(q * data[s].o[i] for s, q in shares.items())
                changed = [s for s in set(pending) | set(prev_target) if abs(pending.get(s, 0.0) - prev_target.get(s, 0.0)) > 1e-9]
                # sells first, so buys can use the proceeds
                for phase in ("sell", "buy"):
                    for s in sorted(changed):
                        px_open = data[s].o[i]
                        cur = shares.get(s, 0.0)
                        want = pending.get(s, 0.0) * eq_open / px_open
                        delta = want - cur
                        if phase == "sell" and delta < 0:
                            px = px_open * (1 - self.slip)
                            cash += -delta * px
                            left = cur + delta
                            if left < 1e-9:
                                shares.pop(s, None)
                                tr = open_trades.pop(s, None)
                                if tr:
                                    tr["exit_date"] = self.dates[i]
                                    tr["exit_px"] = px
                                    tr["pnl_pct"] = (px / tr["entry_px"] - 1.0) * 100.0
                                    tr["pnl_usd"] = (px - tr["entry_px"]) * tr["qty"]
                                    tr["hold_days"] = i - tr["entry_i"]
                                    res.trades.append(tr)
                            else:
                                shares[s] = left
                        elif phase == "buy" and delta > 0:
                            px = px_open * (1 + self.slip)
                            cost = delta * px
                            if cost > cash:
                                delta = cash / px
                                cost = cash
                            if delta <= 0:
                                continue
                            cash -= cost
                            shares[s] = cur + delta
                            if s not in open_trades:
                                open_trades[s] = {"symbol": s, "entry_date": self.dates[i], "entry_px": px,
                                                  "entry_i": i, "qty": delta}
                prev_target = dict(pending)
                pending = None
            for s, q in shares.items():
                y = DIV_YIELD_PCT.get(s, STOCK_YIELD_DEFAULT) / 100.0 / TRADING_DAYS
                cash += q * data[s].c[i] * y
            held_value = sum(q * data[s].c[i] for s, q in shares.items())
            equity = cash + held_value
            res.equity.append(equity)
            res.invested.append(held_value / equity if equity else 0.0)
            res.dates.append(self.dates[i])

            tgt = strat.decide(i)
            tgt = {s: w for s, w in tgt.items() if w > 1e-9}
            total = sum(tgt.values())
            if total > 1.0:
                tgt = {s: w / total for s, w in tgt.items()}
            if tgt != prev_target:
                pending = tgt

        # open trades are marked, not realised, and reported separately
        last = self.n - 1
        res.open_trades = [{"symbol": s, "entry_date": t["entry_date"],
                            "pnl_pct": (data[s].c[last] / t["entry_px"] - 1.0) * 100.0} for s, t in open_trades.items()]
        return res


# --------------------------------------------------------------------------- metrics

def seg_metrics(equity: list[float], invested: list[float] | None = None) -> dict:
    if len(equity) < 20:
        return {}
    rets = [equity[i] / equity[i - 1] - 1.0 for i in range(1, len(equity))]
    years = len(rets) / TRADING_DAYS
    total = equity[-1] / equity[0] - 1.0
    cagr = (equity[-1] / equity[0]) ** (1 / years) - 1.0 if years > 0 and equity[0] > 0 else 0.0
    peak, mdd = equity[0], 0.0
    for v in equity:
        peak = max(peak, v)
        mdd = max(mdd, 1.0 - v / peak)
    rf = CASH_RATE_PCT / 100.0 / TRADING_DAYS
    sd = statistics.pstdev(rets) if len(rets) > 1 else 0.0
    sharpe = (statistics.fmean(rets) - rf) / sd * math.sqrt(TRADING_DAYS) if sd > 0 else 0.0
    out = {
        "total_return_pct": total * 100.0, "cagr_pct": cagr * 100.0, "max_drawdown_pct": mdd * 100.0,
        "sharpe": sharpe, "calmar": (cagr / mdd) if mdd > 0 else None, "vol_pct": sd * math.sqrt(TRADING_DAYS) * 100.0,
    }
    if invested:
        out["avg_exposure_pct"] = statistics.fmean(invested) * 100.0
    return out


def trade_metrics(trades: list[dict]) -> dict:
    if not trades:
        return {"trades": 0}
    wins = [t["pnl_usd"] for t in trades if t["pnl_usd"] > 0]
    losses = [-t["pnl_usd"] for t in trades if t["pnl_usd"] <= 0]
    return {
        "trades": len(trades),
        "win_rate_pct": 100.0 * len(wins) / len(trades),
        "avg_trade_pct": statistics.fmean(t["pnl_pct"] for t in trades),
        "profit_factor": (sum(wins) / sum(losses)) if losses and sum(losses) > 0 else None,
        "avg_hold_days": statistics.fmean(t["hold_days"] for t in trades),
    }


def yearly(res: Result) -> dict:
    out = {}
    by_year: dict[str, list[float]] = {}
    for d, v in zip(res.dates, res.equity):
        by_year.setdefault(d[:4], []).append(v)
    prev_end = res.equity[0]
    for y in sorted(by_year):
        vals = by_year[y]
        out[y] = (vals[-1] / prev_end - 1.0) * 100.0
        prev_end = vals[-1]
    return out


# --------------------------------------------------------------------------- strategies

class Strategy:
    id = "base"

    def __init__(self, data: dict[str, Series]):
        self.d = data
        self.reset()

    def reset(self):
        pass

    def decide(self, i: int) -> dict:
        raise NotImplementedError


def first_trading_day_of_month(d: Series, i: int) -> bool:
    return i > 0 and d.dates[i][5:7] != d.dates[i - 1][5:7]


class BuyHold(Strategy):
    def __init__(self, data, symbol="SPY"):
        self.symbol = symbol
        super().__init__(data)

    def decide(self, i):
        return {self.symbol: 1.0}


class Trend200(Strategy):
    """SPY when close is above its 200-day SMA, otherwise cash."""

    def decide(self, i):
        s = self.d["SPY"]
        return {"SPY": 1.0} if s.c[i] > s.sma(200)[i] else {}


class SectorMomentum(Strategy):
    """Monthly: top 3 sector ETFs by mean of 3/6/12-month return, only those with positive score."""

    def reset(self):
        self.target: dict = {}

    def decide(self, i):
        spy = self.d["SPY"]
        if first_trading_day_of_month(spy, i):
            scores = []
            for sym in SECTORS:
                s = self.d[sym]
                parts = [s.ret(63)[i], s.ret(126)[i], s.ret(252)[i]]
                if any(p is None for p in parts):
                    continue
                scores.append((statistics.fmean(parts), sym))
            scores.sort(reverse=True)
            top = [sym for sc, sym in scores[:3] if sc > 0]
            self.target = {sym: 1.0 / 3.0 for sym in top}
        return dict(self.target)


class GEM(Strategy):
    """Antonacci Global Equities Momentum: SPY vs EFA on 12-month return, else AGG."""

    def reset(self):
        self.target: dict = {}

    def decide(self, i):
        spy = self.d["SPY"]
        if first_trading_day_of_month(spy, i):
            r_spy, r_efa, r_bil = spy.ret(252)[i], self.d["EFA"].ret(252)[i], self.d["BIL"].ret(252)[i]
            if None in (r_spy, r_efa, r_bil):
                self.target = {}
            elif r_spy > r_bil:
                self.target = {"SPY": 1.0} if r_spy >= r_efa else {"EFA": 1.0}
            else:
                self.target = {"AGG": 1.0}
        return dict(self.target)


class Faber(Strategy):
    """Faber tactical allocation: five assets, 20% each while above the 10-month (210-day) SMA."""

    ASSETS = ["SPY", "EFA", "AGG", "GLD", "VNQ"]

    def reset(self):
        self.target: dict = {}

    def decide(self, i):
        spy = self.d["SPY"]
        if first_trading_day_of_month(spy, i):
            t = {}
            for sym in self.ASSETS:
                s = self.d[sym]
                m = s.sma(210)[i]
                if m is not None and s.c[i] > m:
                    t[sym] = 0.2
            self.target = t
        return dict(self.target)


class RSI2(Strategy):
    """Connors RSI(2): buy a pullback in an uptrend, sell the first bounce above the 5-day SMA."""

    SYMS = ["SPY", "QQQ"]

    def __init__(self, data, symbols=None, entry=10.0, max_hold=10):
        self.symbols = symbols or self.SYMS
        self.entry, self.max_hold = entry, max_hold
        super().__init__(data)

    def reset(self):
        self.held: dict[str, int] = {}

    def decide(self, i):
        for sym in self.symbols:
            s = self.d[sym]
            if sym in self.held:
                age = i - self.held[sym]
                if s.c[i] > s.sma(5)[i] or age >= self.max_hold:
                    del self.held[sym]
            else:
                r, m200 = s.rsi(2)[i], s.sma(200)[i]
                if r is not None and m200 is not None and s.c[i] > m200 and r < self.entry:
                    self.held[sym] = i
        w = 1.0 / len(self.symbols)
        return {sym: w for sym in self.held}


class IBSRevert(Strategy):
    """Internal bar strength: buy index ETFs that close in the bottom of their range above SMA200."""

    SYMS = ["SPY", "QQQ", "IWM"]

    def reset(self):
        self.held: dict[str, int] = {}

    def decide(self, i):
        for sym in self.SYMS:
            s = self.d[sym]
            ibs = s.ibs(i)
            if sym in self.held:
                if (ibs is not None and ibs > 0.7) or i - self.held[sym] >= 10:
                    del self.held[sym]
            else:
                m200 = s.sma(200)[i]
                if ibs is not None and m200 is not None and s.c[i] > m200 and ibs < 0.2:
                    self.held[sym] = i
        return {sym: 1.0 / len(self.SYMS) for sym in self.held}


class Donchian(Strategy):
    """Turtle-style system 1: buy a 20-day high, exit on a 10-day low, SPY above SMA200 for new entries."""

    SLOTS = 5

    def reset(self):
        self.held: dict[str, int] = {}

    def decide(self, i):
        spy = self.d["SPY"]
        for sym in list(self.held):
            s = self.d[sym]
            if s.c[i] < s.low_before(10)[i]:
                del self.held[sym]
        if spy.c[i] > spy.sma(200)[i] and len(self.held) < self.SLOTS:
            cands = []
            for sym in BROAD_ETFS:
                if sym in self.held:
                    continue
                s = self.d[sym]
                hb, r63 = s.high_before(20)[i], s.ret(63)[i]
                if hb is not None and r63 is not None and s.c[i] > hb:
                    cands.append((r63, sym))
            cands.sort(reverse=True)
            for _, sym in cands[: self.SLOTS - len(self.held)]:
                self.held[sym] = i
        return {sym: 1.0 / self.SLOTS for sym in self.held}


class DipBuy(Strategy):
    """The workflow idea at scale: quality stock above its EMA200 that dips to its EMA20."""

    SLOTS = 5

    def __init__(self, data, universe=None, target_pct=8.0):
        self.universe = universe or MEGACAPS
        self.target_pct = target_pct
        super().__init__(data)

    def reset(self):
        self.held: dict[str, tuple[int, float]] = {}

    def decide(self, i):
        for sym in list(self.held):
            s = self.d[sym]
            ei, ep = self.held[sym]
            if (s.c[i] / ep - 1.0) * 100.0 >= self.target_pct or s.c[i] < s.ema(200)[i] or i - ei >= 30:
                del self.held[sym]
        if len(self.held) < self.SLOTS:
            cands = []
            for sym in self.universe:
                if sym in self.held:
                    continue
                s = self.d[sym]
                e20, e200, r252 = s.ema(20)[i], s.ema(200)[i], s.ret(252)[i]
                if None in (e20, e200, r252):
                    continue
                if s.c[i] > e200 and s.c[i] <= e20:
                    cands.append((r252, sym))
            cands.sort(reverse=True)
            for _, sym in cands[: self.SLOTS - len(self.held)]:
                self.held[sym] = (i, self.d[sym].c[i])
        return {sym: 1.0 / self.SLOTS for sym in self.held}


class PullbackControl(Strategy):
    """Approximation of the live 9947 entry rule: above SMA20 and SMA50, RSI14 48-65, 5.5% stop,
    7.5% target, 30-day time stop, 4 slots. Optional SPY regime gate (the review's main finding)."""

    SLOTS = 4

    def __init__(self, data, regime_gate=False, universe=None):
        self.regime_gate = regime_gate
        self.universe = universe or (MEGACAPS + BROAD_ETFS)
        super().__init__(data)

    def reset(self):
        self.held: dict[str, tuple[int, float]] = {}

    def decide(self, i):
        for sym in list(self.held):
            s = self.d[sym]
            ei, ep = self.held[sym]
            chg = s.c[i] / ep - 1.0
            if chg <= -0.055 or chg >= 0.075 or i - ei >= 30:
                del self.held[sym]
        spy = self.d["SPY"]
        ok = (not self.regime_gate) or (spy.c[i] > spy.sma(200)[i] and spy.c[i] > spy.sma(50)[i])
        if ok and len(self.held) < self.SLOTS:
            cands = []
            for sym in self.universe:
                if sym in self.held:
                    continue
                s = self.d[sym]
                m20, m50, r14, r63 = s.sma(20)[i], s.sma(50)[i], s.rsi(14)[i], s.ret(63)[i]
                if None in (m20, m50, r14, r63):
                    continue
                if s.c[i] > m20 and s.c[i] > m50 and 48.0 <= r14 <= 65.0:
                    cands.append((r63, sym))
            cands.sort(reverse=True)
            for _, sym in cands[: self.SLOTS - len(self.held)]:
                self.held[sym] = (i, self.d[sym].c[i])
        return {sym: 1.0 / self.SLOTS for sym in self.held}


class StockMomentum(Strategy):
    """Jegadeesh-Titman style 12-1 momentum, top 5 of the megacap basket, monthly, SPY SMA200 gate."""

    def reset(self):
        self.target: dict = {}

    def decide(self, i):
        spy = self.d["SPY"]
        if first_trading_day_of_month(spy, i):
            if spy.c[i] <= spy.sma(200)[i]:
                self.target = {}
            else:
                scores = []
                for sym in MEGACAPS:
                    s = self.d[sym]
                    r12, r1 = s.ret(252)[i], s.ret(21)[i]
                    if r12 is None or r1 is None:
                        continue
                    scores.append(((1 + r12) / (1 + r1) - 1, sym))
                scores.sort(reverse=True)
                self.target = {sym: 0.2 for _, sym in scores[:5]}
        return dict(self.target)


REGISTRY = [
    dict(id="spy_buy_hold", cls=BuyHold, family="baseline", label="SPY buy and hold", paper="baseline",
         symbols=["SPY"], biased=False, rotation=True,
         source="Baseline A. Passive index exposure; the bar every active idea has to beat after costs.",
         thesis="Equities earn a risk premium over time; most active rules fail to beat it net of costs and drawdown."),
    dict(id="spy_trend200", cls=Trend200, family="baseline", label="SPY above SMA200, else cash", paper="baseline",
         symbols=["SPY"], biased=False, rotation=True,
         source="Baseline B. 200-day moving average timing, Faber 2007 and long-standing practitioner use.",
         thesis="Staying out below the 200-day average avoids most of the deep drawdowns at a modest cost in whipsaws."),
    dict(id="sector_momentum", cls=SectorMomentum, family="rotation", label="Sector ETF momentum, top 3 monthly", paper="workflow",
         symbols=SECTORS, biased=False, rotation=True,
         source="Moskowitz and Grinblatt 1999 (industry momentum); Antonacci, Dual Momentum Investing 2014 (absolute filter).",
         thesis="Sector winners tend to keep winning for 3-12 months; requiring a positive score sends capital to cash in downtrends."),
    dict(id="gem_dual_momentum", cls=GEM, family="rotation", label="Global equities momentum (SPY/EFA/AGG)", paper="workflow",
         symbols=["SPY", "EFA", "AGG", "BIL"], biased=False, rotation=True,
         source="Antonacci, Dual Momentum Investing 2014.",
         thesis="Relative momentum picks the stronger equity region; absolute momentum versus T-bills rotates to bonds in bear markets."),
    dict(id="faber_taa", cls=Faber, family="rotation", label="Faber 5-asset tactical allocation", paper="workflow",
         symbols=Faber.ASSETS, biased=False, rotation=True,
         source="Faber 2007, A Quantitative Approach to Tactical Asset Allocation.",
         thesis="Holding each asset only above its 10-month average cuts drawdowns across asset classes with few trades."),
    dict(id="rsi2_spy_qqq", cls=RSI2, family="mean_reversion", label="Connors RSI(2) on SPY/QQQ", paper="shadow_arm",
         symbols=["SPY", "QQQ"], biased=False, rotation=False,
         source="Connors and Alvarez, Short Term Trading Strategies That Work 2008.",
         thesis="Short, sharp pullbacks in an uptrend tend to revert within days; RSI(2) below 10 above SMA200 marks them."),
    dict(id="ibs_indices", cls=IBSRevert, family="mean_reversion", label="IBS mean reversion on SPY/QQQ/IWM", paper="shadow_arm",
         symbols=["SPY", "QQQ", "IWM"], biased=False, rotation=False,
         source="Internal bar strength studies (Connors/Alvarez lineage; QuantifiedStrategies write-ups).",
         thesis="Closing in the bottom fifth of the day range in an uptrend predicts a next-day bounce in index ETFs."),
    dict(id="donchian_etf", cls=Donchian, family="trend", label="Donchian 20/10 breakout on liquid ETFs", paper="shadow_arm",
         symbols=BROAD_ETFS, biased=False, rotation=False,
         source="Donchian channel breakout; Turtle Trading system 1 (Dennis and Eckhardt, 1983).",
         thesis="Breakouts to new 20-day highs start trends that run longer than the 10-day-low exit gives back."),
    dict(id="dip_buy_ema", cls=DipBuy, family="pullback", label="Quality dip buy: above EMA200, at/below EMA20", paper="workflow",
         symbols=MEGACAPS, biased=True, rotation=False,
         source="Generalises rules/workflows/example-msft-dip.yaml; trend-pullback entries are standard in swing literature.",
         thesis="Large, trending companies that dip to a short average while above the long average usually resume the trend."),
    dict(id="pullback_control", cls=PullbackControl, family="pullback", label="Control: current 9947 entry rule (approx.)", paper="control",
         symbols=MEGACAPS + BROAD_ETFS, biased=True, rotation=False,
         source="rules/trader-swing-9947.yaml, approximated on daily bars (no ATR sizing, no intraday logic).",
         thesis="Existing live-paper approach: SMA20/50 trend, RSI 48-65, 5.5% stop, 7.5% target."),
    dict(id="pullback_regime_gate", cls=PullbackControl, kwargs=dict(regime_gate=True), family="pullback",
         label="Control plus SPY>SMA50/SMA200 regime gate", paper="shadow_arm",
         symbols=MEGACAPS + BROAD_ETFS, biased=True, rotation=False,
         source="The 2026-10-23 swing review: the book earns in low_vol_trend and loses in elevated_vol.",
         thesis="Only entering when the index is in an uptrend should remove most of the elevated-vol stop-outs."),
    dict(id="stock_momentum_12_1", cls=StockMomentum, family="momentum", label="12-1 momentum, top 5 megacaps, monthly", paper="workflow",
         symbols=MEGACAPS, biased=True, rotation=True,
         source="Jegadeesh and Titman 1993; skip-month variant.",
         thesis="Stocks with the best 12-month returns (ex last month) continue to outperform for months."),
]

NOT_BACKTESTED = [
    dict(id="pead_large_caps", family="event", label="Post-earnings-announcement drift", paper="shadow_arm",
         source="Bernard and Thomas 1989; Ball and Brown 1968.",
         thesis="Prices keep drifting in the direction of an earnings surprise for weeks.",
         reason="Needs point-in-time earnings dates and surprises. Only rules/earnings-calendar.json (forward dates) exists. "
                "Paper-test it from the calendar going forward instead of backtesting."),
]


def build_strategy(spec: dict, data: dict[str, Series]) -> Strategy:
    return spec["cls"](data, **spec.get("kwargs", {}))


# --------------------------------------------------------------------------- reporting

def evaluate(spec: dict, data: dict[str, Series]) -> dict:
    out = {"id": spec["id"], "label": spec["label"], "family": spec["family"], "biased_universe": spec["biased"],
           "paper_target": spec["paper"]}
    runs = {}
    for bps in (5.0, 15.0):
        eng = Engine(data, slippage_bps=bps)
        runs[bps] = eng.run(build_strategy(spec, data))
    base = runs[5.0]
    full = seg_metrics(base.equity, base.invested)
    split = len(base.equity) * 2 // 3
    oos = seg_metrics(base.equity[split:], base.invested[split:])
    out.update(
        start=base.dates[0], end=base.dates[-1], oos_start=base.dates[split],
        full=full, oos=oos, trade=trade_metrics(base.trades), open_positions=getattr(base, "open_trades", []),
        yearly=yearly(base), cagr_pct_at_15bps=seg_metrics(runs[15.0].equity)["cagr_pct"],
        final_equity=base.equity[-1],
    )
    return out


def rank_and_flag(rows: list[dict]) -> None:
    spy = next(r for r in rows if r["id"] == "spy_buy_hold")
    for r in rows:
        r["excess_cagr_vs_spy_pct"] = r["full"]["cagr_pct"] - spy["full"]["cagr_pct"]
        r["oos_excess_cagr_vs_spy_pct"] = r["oos"]["cagr_pct"] - spy["oos"]["cagr_pct"]
        n = r["trade"].get("trades", 0)
        rot = next(s for s in REGISTRY if s["id"] == r["id"])["rotation"]
        checks = {
            "beats_spy_sharpe": r["full"]["sharpe"] > spy["full"]["sharpe"],
            "lower_drawdown_than_spy": r["full"]["max_drawdown_pct"] < spy["full"]["max_drawdown_pct"],
            "oos_positive": r["oos"]["cagr_pct"] > 0,
            "survives_15bps": r["cagr_pct_at_15bps"] > 0,
            "enough_trades": n >= 30 or rot,
        }
        r["checks"] = checks
        r["checks_passed"] = sum(checks.values())
    rows.sort(key=lambda r: (r["checks_passed"], r["full"]["sharpe"]), reverse=True)


def render_md(rows: list[dict], meta: dict) -> str:
    L = []
    L.append(f"# Swing lab results ({meta['generated']})\n")
    L.append(f"Window {rows[0]['start']} to {rows[0]['end']} ({meta['bars']} bars loaded per symbol). Sleeve $4,000, 5 bps slippage per side, "
             f"idle cash {CASH_RATE_PCT:.0f}%/yr, approximate dividend accrual. Out-of-sample = last third from {rows[0]['oos_start']}.\n")
    L.append("Ranked by number of checks passed, then Sharpe. Checks: Sharpe above SPY, drawdown below SPY, OOS CAGR positive, "
             "still positive at 15 bps, at least 30 trades (or a monthly rotation).\n")
    L.append("| Strategy | CAGR % | Max DD % | Sharpe | Excess CAGR vs SPY | OOS CAGR % | CAGR @15bps | Trades | Win % | PF | Exposure % | Checks |")
    L.append("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for r in rows:
        f, o, t = r["full"], r["oos"], r["trade"]
        pf = t.get("profit_factor")
        L.append(
            f"| {r['label']}{' *' if r['biased_universe'] else ''} | {f['cagr_pct']:.1f} | {f['max_drawdown_pct']:.1f} | {f['sharpe']:.2f} | "
            f"{r['excess_cagr_vs_spy_pct']:+.1f} | {o['cagr_pct']:.1f} | {r['cagr_pct_at_15bps']:.1f} | {t.get('trades', 0)} | "
            f"{t.get('win_rate_pct', 0):.0f} | {('%.2f' % pf) if pf else 'n/a'} | {f.get('avg_exposure_pct', 0):.0f} | {r['checks_passed']}/5 |"
        )
    L.append("\n`*` stock basket is today's survivors, so results are optimistic (selection look-ahead).\n")
    years = sorted({y for r in rows for y in r["yearly"]})
    L.append("## Calendar-year returns (%)\n")
    L.append("| Strategy | " + " | ".join(years) + " |")
    L.append("|---|" + "---|" * len(years))
    for r in rows:
        L.append(f"| {r['label']} | " + " | ".join(f"{r['yearly'].get(y, float('nan')):.1f}" for y in years) + " |")
    L.append("\n## Not backtested\n")
    for n in NOT_BACKTESTED:
        L.append(f"- **{n['label']}**: {n['reason']}")
    if meta["suspicious"]:
        L.append("\n## Data flags\n")
        L.append("Daily moves over 35% (check split adjustment): " + "; ".join(meta["suspicious"][:10]))
    L.append("\n## Caveats\n")
    L.append("- Fixed published parameters, nothing fitted; OOS is a stability check on the last third, not a fit/test split.")
    L.append("- Close-based stops and next-open fills; no intraday gaps through stops beyond the open slippage.")
    L.append("- One bear market (2022) and the 2020 crash in the window; price history is split-adjusted but not dividend-adjusted, so a yield table is used.")
    L.append("- A strategy passing here earns a paper test, nothing more.")
    return "\n".join(L) + "\n"


def cmd_run(args) -> int:
    repo = args.repo.resolve()
    data, suspicious = load_data(repo)
    missing = {s for spec in REGISTRY for s in spec["symbols"]} - set(data)
    if missing:
        raise SystemExit(f"missing data for {sorted(missing)}")
    rows = [evaluate(spec, data) for spec in REGISTRY]
    rank_and_flag(rows)
    meta = {"generated": dt.date.today().isoformat(), "bars": len(data["SPY"].c), "suspicious": suspicious,
            "slippage_bps": 5.0, "cash_rate_pct": CASH_RATE_PCT, "start_capital": START_CAPITAL}
    out_dir = (args.out_dir if args.out_dir.is_absolute() else repo / args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    payload = {"generated_at": dt.datetime.now(dt.timezone.utc).isoformat(), "meta": meta, "results": rows,
               "not_backtested": NOT_BACKTESTED}
    (out_dir / "swing-lab-results.json").write_text(json.dumps(payload, indent=1))
    (out_dir / "swing-lab-results.md").write_text(render_md(rows, meta))
    art = repo / "rules" / "analysis"
    art.mkdir(parents=True, exist_ok=True)
    (art / "swing-lab.json").write_text(json.dumps(payload))
    print(render_md(rows, meta))
    return 0


# --------------------------------------------------------------------------- catalog

def yaml_str(v: str) -> str:
    return json.dumps(v)


def cmd_catalog(args) -> int:
    repo = args.repo.resolve()
    out = repo / "rules" / "arms" / "swing-candidates"
    out.mkdir(parents=True, exist_ok=True)
    index = []
    for spec in REGISTRY:
        lines = [
            f"# Swing candidate: {spec['label']}",
            f"id: {spec['id']}",
            f"family: {spec['family']}",
            f"label: {yaml_str(spec['label'])}",
            f"source: {yaml_str(spec['source'])}",
            f"thesis: {yaml_str(spec['thesis'])}",
            f"backtest_class: {spec['cls'].__name__}",
            f"paper_target: {spec['paper']}   # baseline | control | shadow_arm | workflow",
            f"biased_universe: {str(spec['biased']).lower()}",
            f"universe: [{', '.join(spec['symbols'])}]",
            "doc: " + yaml_str((spec['cls'].__doc__ or '').strip().replace("\n", " ")),
        ]
        if spec.get("kwargs"):
            lines.append("params: " + json.dumps(spec["kwargs"]))
        (out / f"{spec['id']}.yaml").write_text("\n".join(lines) + "\n")
        index.append(spec["id"])
    for n in NOT_BACKTESTED:
        lines = [f"# Swing candidate: {n['label']} (not backtestable with current data)", f"id: {n['id']}", f"family: {n['family']}",
                 f"label: {yaml_str(n['label'])}", f"source: {yaml_str(n['source'])}", f"thesis: {yaml_str(n['thesis'])}",
                 f"paper_target: {n['paper']}", "backtested: false", f"reason: {yaml_str(n['reason'])}"]
        (out / f"{n['id']}.yaml").write_text("\n".join(lines) + "\n")
        index.append(n["id"])
    (out / "README.md").write_text(
        "# Swing candidates\n\nOne file per strategy tested by `scripts/swing_lab.py`. The YAML records provenance and the\n"
        "paper target; the executable definition is the backtest class named in `backtest_class`. Results:\n"
        "`rules/analysis-20261023/swing-lab-results.md`.\n\n" + "\n".join(f"- `{i}`" for i in index) + "\n")
    print(f"wrote {len(index)} candidate files to {out}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name, fn in (("run", cmd_run), ("catalog", cmd_catalog)):
        p = sub.add_parser(name)
        p.add_argument("--repo", type=Path, default=Path("."))
        if name == "run":
            p.add_argument("--out-dir", type=Path, default=Path("rules/analysis-20261023"))
        p.set_defaults(fn=fn)
    args = ap.parse_args()
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
