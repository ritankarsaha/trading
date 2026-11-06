use std::collections::HashSet;
use tc_backtest::{synth, Engine, EngineError};
use tc_core::*;
use tc_ledger::Ledger;
use tc_risk::{RiskGate, RiskLimits};
use tc_sim::{SimConfig, SimVenue};
use tc_strategy::pm::{FadeConfig, LongshotFade};
use tc_strategy::Strategy;

fn fade_engine(s: &synth::PmSynth, equity: i64) -> Engine<SimVenue> {
    let mut limits = RiskLimits::default();
    limits.post_only_tags = HashSet::from(["longshot_fade".to_string()]);
    limits.max_total_frac = 0.60;
    let strat: Vec<Box<dyn Strategy>> =
        vec![Box::new(LongshotFade::new(FadeConfig::default(), None))];
    Engine::new(
        SimVenue::new("sim", SimConfig::default(), s.instruments.clone()),
        s.instruments.clone(),
        Money::dollars(equity),
        RiskGate::new(limits, Money::dollars(equity)),
        Ledger::in_memory(),
        strat,
    )
}

#[test]
fn replay_is_deterministic_bit_for_bit() {
    let s = synth::prediction_markets(40, 60, 7, 0.55);
    let r1 = fade_engine(&s, 10_000).run(s.events.clone()).unwrap();
    let r2 = fade_engine(&s, 10_000).run(s.events.clone()).unwrap();
    assert_eq!(r1.digest, r2.digest);
    assert_eq!(r1.end_equity, r2.end_equity);
    assert!(r1.counters.fills > 0, "scenario must actually trade");
}

#[test]
fn different_data_changes_digest() {
    let a = synth::prediction_markets(40, 60, 7, 0.55);
    let b = synth::prediction_markets(40, 60, 8, 0.55);
    let ra = fade_engine(&a, 10_000).run(a.events.clone()).unwrap();
    let rb = fade_engine(&b, 10_000).run(b.events.clone()).unwrap();
    assert_ne!(ra.digest, rb.digest);
}

#[test]
fn out_of_order_events_are_refused() {
    let s = synth::prediction_markets(2, 3, 1, 1.0);
    let mut e = fade_engine(&s, 10_000);
    let mut evs = s.events.clone();
    evs.swap(1, 4);
    let err = e.run(evs).unwrap_err();
    assert!(matches!(err, EngineError::OutOfOrder { .. }));
}

#[test]
fn accounting_closes_exactly_after_all_markets_settle() {
    // after every market settles there are no open positions, and equity == cash == start + realised - fees
    let s = synth::prediction_markets(60, 80, 11, 0.55);
    let mut e = fade_engine(&s, 10_000);
    let r = e.run(s.events.clone()).unwrap();
    assert_eq!(r.open_positions, 0);
    assert_eq!(e.portfolio.cash, e.portfolio.equity(&e.marks));
    let realised: i64 = e.portfolio.positions.values().map(|p| p.realized.0).sum();
    assert_eq!(
        e.portfolio.cash.0,
        Money::dollars(10_000).0 + realised - e.portfolio.fees_paid.0
    );
}

#[test]
fn maker_strategy_never_takes() {
    let s = synth::prediction_markets(60, 80, 5, 0.55);
    let r = fade_engine(&s, 10_000).run(s.events).unwrap();
    assert_eq!(
        r.counters.taker_fills, 0,
        "post_only maker strategy must never take liquidity"
    );
    assert!(r.counters.maker_fills > 0);
}

#[test]
fn longshot_fade_beats_calibrated_control_on_biased_data() {
    // Biased world (longshots overpriced) vs calibrated control, same seeds, averaged over seeds.
    // PROVES THE PLUMBING ONLY — the bias is built into the generator.
    let (mut biased, mut control) = (0i64, 0i64);
    for seed in 100..110 {
        let b = synth::prediction_markets(80, 80, seed, 0.55);
        biased += fade_engine(&b, 10_000).run(b.events.clone()).unwrap().pnl.0;
        let c = synth::prediction_markets(80, 80, seed, 1.0);
        control += fade_engine(&c, 10_000).run(c.events.clone()).unwrap().pnl.0;
    }
    assert!(biased > control, "biased={biased} control={control}");
    assert!(
        biased > 0,
        "biased world should be profitable for the fade: {biased}"
    );
}

#[test]
fn drawdown_breaker_halts_and_cancels_everything() {
    // calibrated=false world where longshots hit way more than priced → fade bleeds → breaker must trip
    let s = synth::prediction_markets(80, 80, 3, 3.0); // true_p = 3× price: fading is a disaster
    let mut limits = RiskLimits::default();
    limits.post_only_tags = HashSet::from(["longshot_fade".to_string()]);
    limits.max_total_frac = 0.60;
    limits.max_drawdown_frac = 0.03;
    let strat: Vec<Box<dyn Strategy>> =
        vec![Box::new(LongshotFade::new(FadeConfig::default(), None))];
    let mut e = Engine::new(
        SimVenue::new("sim", SimConfig::default(), s.instruments.clone()),
        s.instruments.clone(),
        Money::dollars(10_000),
        RiskGate::new(limits, Money::dollars(10_000)),
        Ledger::in_memory(),
        strat,
    );
    let r = e.run(s.events).unwrap();
    assert!(r.counters.halts >= 1, "breaker should have tripped");
    assert!(e.risk.halted().is_some());
    use tc_core::Venue;
    assert!(
        e.venue.open_orders().is_empty(),
        "halt must cancel all resting orders"
    );
}
