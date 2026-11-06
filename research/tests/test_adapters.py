import pandas as pd
from tc_research import kronos_signal as K, tradingagents_signal as T, kalshi_convert as C


def test_prob_above_is_laplace_smoothed():
    assert K.prob_above([1, 2, 3, 4], 0) == 5 / 6
    assert K.prob_above([1, 2, 3, 4], 99) == 1 / 6
    assert 0 < K.prob_above([], 1) < 1


def test_kronos_draws_independent_single_paths():
    calls = []

    class Fake:
        def predict(self, **kw):
            assert kw["sample_count"] == 1          # averaging paths would destroy the distribution
            calls.append(1)
            return pd.DataFrame({"close": [100.0, 100.0 + len(calls)]})

    idx = pd.Series(pd.date_range("2026-01-01", periods=2, freq="h"))
    p = K.kronos_prob_above(Fake(), pd.DataFrame({"close": [1.0, 2.0]}), idx, idx, strike=105.0, n_paths=10)
    assert len(calls) == 10 and abs(p - (5 + 1) / 12) < 1e-12   # finals 101..110, five above 105


def test_rating_mapping_and_review_gives_no_signal():
    assert T.rating_to_prob("Buy") > T.rating_to_prob("Hold") > T.rating_to_prob("Sell")
    assert T.rating_to_prob("REVIEW") is None


def test_kalshi_converter_semantics():
    markets = pd.DataFrame({
        "ticker": ["A-1", "B-1"], "event_ticker": ["A", "B"], "result": ["yes", ""],
        "close_time": pd.to_datetime(["2026-02-01", "2026-02-01"], utc=True)})
    trades = pd.DataFrame({
        "ticker": ["A-1", "A-1", "B-1"], "count": [5, 7, 1], "yes_price": [10, 12, 50],
        "taker_side": ["yes", "no", "yes"],
        "created_time": pd.to_datetime(["2026-01-01", "2026-01-02", "2026-01-03"], utc=True)})
    ev = C.convert_frames(markets, trades)
    kinds = [list(e)[0] for e in ev]
    assert kinds == ["Trade", "Trade", "Trade", "Settlement"]           # sorted; only resolved market settles
    assert ev[0]["Trade"]["taker_side"] == "Buy" and ev[1]["Trade"]["taker_side"] == "Sell"
    assert ev[0]["Trade"]["instrument"] == "A:A-1" and ev[-1]["Settlement"]["price"] == 10_000
