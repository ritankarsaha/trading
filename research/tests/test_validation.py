import numpy as np, pytest
from tc_research import validation as V, timemachine as T


def test_splits_have_no_leakage_and_respect_purge_and_embargo():
    for expanding in (False, True):
        folds = list(V.walk_forward_splits(1000, train=300, test=100, step=100, embargo=10, purge=24, expanding=expanding))
        assert len(folds) >= 5
        for tr, te in folds:
            assert tr.max() < te.min()                       # strictly ordered
            assert te.min() - tr.max() - 1 >= 10 + 24        # purge + embargo gap
            assert len(set(tr) & set(te)) == 0
        assert (len(folds[-1][0]) > len(folds[0][0])) == expanding


def test_rolling_window_has_fixed_train_size():
    sizes = {len(tr) for tr, _ in V.walk_forward_splits(1000, 200, 50)}
    assert len(sizes) == 1


def test_dsr_penalises_many_trials_and_short_samples():
    # SR0 (expected best-of-200 under the null) ≈ 0.25 per period here
    sr_above = 0.35
    one_trial = V.deflated_sharpe(sr_above, 500, 1, 0.0)
    many = V.deflated_sharpe(sr_above, 500, 200, 0.01)
    short = V.deflated_sharpe(sr_above, 40, 200, 0.01)
    assert one_trial > 0.95 and many < one_trial          # trying 200 variants discounts the winner
    assert short < many                                   # above SR0, a short sample is less convincing
    # fat tails + negative skew lower confidence further
    assert V.deflated_sharpe(sr_above, 500, 200, 0.01, skew=-1.0, kurt=8.0) < many
    # BELOW the luck threshold, more data only confirms it is luck (probability falls with T)
    assert V.deflated_sharpe(0.15, 500, 200, 0.01) < V.deflated_sharpe(0.15, 40, 200, 0.01)


def test_pbo_flags_pure_noise_and_clears_a_real_edge():
    rng = np.random.default_rng(0)
    noise = rng.normal(0, 0.01, size=(1600, 40))                 # 40 candidates, no edge → picking the best is luck
    assert V.pbo_cscv(noise, 8)["pbo"] > 0.35
    real = rng.normal(0, 0.01, size=(1600, 40)); real[:, 7] += 0.004   # one genuinely good strategy
    assert V.pbo_cscv(real, 8)["pbo"] < 0.1


def test_timemachine_probabilities_sum_and_respond_to_drift():
    rng = np.random.default_rng(1)
    flat = rng.normal(0.0, 0.002, 600)
    r = T.simulate_plan(flat, 60, 100.0, 99.0, 101.0, sims=4000, seed=3)
    assert abs(r["p_target_first"] + r["p_stop_first"] + r["p_neither"] - 1) < 1e-9
    up = T.simulate_plan(flat + 0.0006, 60, 100.0, 99.0, 101.0, sims=4000, seed=3)
    assert up["p_target_first"] > r["p_target_first"] and up["mean_R"] > r["mean_R"]
    with pytest.raises(ValueError):
        T.simulate_plan(flat[:5], 60, 100, 99, 101)
