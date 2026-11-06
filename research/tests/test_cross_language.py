"""Python writes events → Rust `tc replay` consumes them. Proves the interchange schema end to end."""
import pathlib, subprocess, pytest
from tc_research import events as E

BIN = pathlib.Path(__file__).resolve().parents[2] / "engine/target/release/tc"


@pytest.mark.skipif(not BIN.exists(), reason="build the engine first: cd engine && cargo build --release")
def test_rust_replays_python_events(tmp_path):
    evs, t = [], 1_000_000_000
    inst = "EV1:M1"
    for i in range(40):
        t += 1000
        evs.append(E.book(inst, t, [(0.08, 50)], [(0.10, 50)]))
        t += 1
        evs.append(E.trade(inst, t, 0.10, 80, "Buy"))     # taker lifts the ask → our resting sell fills
    t += 1000
    evs.append(E.settlement(inst, t, False))              # longshot misses
    p = tmp_path / "events.jsonl"
    with open(p, "w") as f:
        E.write_jsonl(evs, f)
    out = subprocess.run([str(BIN), "replay", str(p), "--strategy", "fade"], capture_output=True, text=True)
    assert out.returncode == 0, out.stderr
    assert "replaying 81 events" in out.stdout
    assert "maker" in out.stdout and "taker 0" in out.stdout.replace("/ ", "/ ")


@pytest.mark.skipif(not BIN.exists(), reason="build the engine first")
def test_rust_rejects_malformed_probability(tmp_path):
    p = tmp_path / "bad.jsonl"
    p.write_text('{"Signal":{"ts":1,"source":"x","instrument":"A:B","prob":1.5,"confidence":null}}\n')
    out = subprocess.run([str(BIN), "replay", str(p)], capture_output=True, text=True)
    assert out.returncode == 2 and "bad event" in out.stderr
