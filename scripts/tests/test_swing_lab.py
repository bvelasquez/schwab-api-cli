import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import swing_lab as lab  # noqa: E402


def make_series(symbol, closes, opens=None, start="2020-01-01"):
    import datetime as dt

    d0 = dt.date.fromisoformat(start)
    bars = []
    for i, c in enumerate(closes):
        o = opens[i] if opens else c
        d = d0 + dt.timedelta(days=i)
        bars.append([d.isoformat(), o, max(o, c) * 1.01, min(o, c) * 0.99, c, 1000])
    return lab.Series(symbol, bars)


class EngineTests(unittest.TestCase):
    def setUp(self):
        self._rate, self._yield = lab.CASH_RATE_PCT, dict(lab.DIV_YIELD_PCT)
        lab.CASH_RATE_PCT = 0.0
        lab.DIV_YIELD_PCT["SPY"] = 0.0

    def tearDown(self):
        lab.CASH_RATE_PCT = self._rate
        lab.DIV_YIELD_PCT.clear()
        lab.DIV_YIELD_PCT.update(self._yield)

    def test_buy_hold_tracks_price_with_one_day_delay(self):
        closes = [100.0 + i for i in range(50)]
        data = {"SPY": make_series("SPY", closes)}
        res = lab.Engine(data, slippage_bps=0.0).run(lab.BuyHold(data), start=0)
        # decision at close of day 0, fill at open of day 1 (=101.0)
        self.assertAlmostEqual(res.equity[0], lab.START_CAPITAL)
        self.assertAlmostEqual(res.equity[-1], lab.START_CAPITAL * closes[-1] / closes[1], places=4)

    def test_slippage_costs_money_round_trip(self):
        closes = [100.0] * 60
        data = {"SPY": make_series("SPY", closes)}

        class InOut(lab.Strategy):
            def decide(self, i):
                return {"SPY": 1.0} if 5 <= i < 20 else {}

        free = lab.Engine(data, 0.0).run(InOut(data), start=0).equity[-1]
        paid = lab.Engine(data, 10.0).run(InOut(data), start=0).equity[-1]
        self.assertAlmostEqual(free, lab.START_CAPITAL, places=6)
        self.assertLess(paid, free)
        self.assertAlmostEqual(paid / free, (1 - 0.001) ** 2, places=4)

    def test_signal_cannot_see_the_future(self):
        # price jumps only on the last bar; a strategy that buys on the signal bar must not capture earlier moves
        closes = [100.0] * 30 + [150.0]
        data = {"SPY": make_series("SPY", closes)}

        class BuyOnLast(lab.Strategy):
            def decide(self, i):
                return {"SPY": 1.0} if i == 30 else {}

        res = lab.Engine(data, 0.0).run(BuyOnLast(data), start=0)
        self.assertAlmostEqual(res.equity[-1], lab.START_CAPITAL)

    def test_trade_log_records_exit(self):
        closes = [100.0] * 10 + [110.0] * 10
        opens = closes[:]
        data = {"SPY": make_series("SPY", closes, opens)}

        class Hold(lab.Strategy):
            def decide(self, i):
                return {"SPY": 1.0} if 2 <= i < 12 else {}

        res = lab.Engine(data, 0.0).run(Hold(data), start=0)
        self.assertEqual(len(res.trades), 1)
        self.assertGreater(res.trades[0]["pnl_pct"], 0)


class MetricTests(unittest.TestCase):
    def test_max_drawdown(self):
        m = lab.seg_metrics([100.0] * 10 + [50.0] * 10 + [100.0] * 10)
        self.assertAlmostEqual(m["max_drawdown_pct"], 50.0)

    def test_indicators(self):
        s = make_series("X", [float(i) for i in range(1, 40)])
        self.assertAlmostEqual(s.sma(5)[4], 3.0)
        self.assertEqual(s.rsi(2)[10], 100.0)


if __name__ == "__main__":
    unittest.main()
