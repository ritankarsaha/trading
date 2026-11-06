import io, json, pytest
from tc_research import events as E


def test_units_match_engine_fixed_point():
    assert E.px(0.65) == 6500 and E.qty(10) == 100_000


def test_serde_shape():
    e = E.signal("M:YES", 5, "kronos", 0.3)
    assert list(e) == ["Signal"] and e["Signal"]["prob"] == 0.3
    assert list(E.book("M:YES", 1, [(0.48, 10)], [(0.5, 5)])) == ["Book"]


def test_signal_rejects_bad_probability():
    for bad in (0.0, 1.0, -0.1, 1.5):
        with pytest.raises(ValueError):
            E.signal("M:YES", 1, "x", bad)


def test_write_jsonl_roundtrip():
    buf = io.StringIO()
    n = E.write_jsonl([E.settlement("M:YES", 9, True), E.trade("M:YES", 8, 0.5, 3, "Buy")], buf)
    lines = buf.getvalue().strip().split("\n")
    assert n == 2 and json.loads(lines[0])["Settlement"]["price"] == 10_000
