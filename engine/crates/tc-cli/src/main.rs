//! `tc` — command-line front end for the engine.
//!
//!   tc demo-pm    [--seed N] [--markets N]      longshot fade on SYNTHETIC biased vs calibrated markets
//!   tc demo-bars  [--seed N]                    ported quant-trading bar strategies on SYNTHETIC GBM bars
//!   tc demo-regime [--seed N]                   regime gate + ATR exits + vol sizing vs plain, on SYNTHETIC regimes
//!   tc determinism                              run twice, compare ledger digests
//!   tc replay <events.jsonl> --strategy fade|edge [--equity D] [--halt-file PATH]
//!             replay a recorded event stream (one JSON `Event` per line, the research-plane interchange format)
//!
//! Everything here is paper/sim. There is no live path in this binary yet.

use std::collections::{BTreeSet, HashSet};
use std::io::{BufRead, BufReader};
use tc_backtest::{synth, Engine, Report};
use tc_core::*;
use tc_ledger::Ledger;
use tc_risk::{RiskGate, RiskLimits};
use tc_sim::{SimConfig, SimVenue};
use tc_strategy::bar::*;
use tc_strategy::pm::*;
use tc_strategy::regime::{Home, RegimeGated};
use tc_strategy::Strategy;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn print_report(label: &str, r: &Report) {
    println!("── {label}");
    println!(
        "   equity {} → {}   pnl {}  ({:+.2}%)   maxDD {:.2}%   fees {}",
        r.start_equity, r.end_equity, r.pnl, r.return_pct, r.max_drawdown_pct, r.fees_paid
    );
    let c = &r.counters;
    println!(
        "   orders {}  fills {} (maker {} / taker {})  risk-rejects {}  venue-rejects {}  halts {}  replaces-skipped {}",
        c.orders_sent, c.fills, c.maker_fills, c.taker_fills, c.risk_rejects, c.venue_rejects, c.halts, c.replaces_skipped
    );
    if !c.reject_reasons.is_empty() {
        println!("   risk rejects by reason: {:?}", c.reject_reasons);
    }
    for (tag, m) in &r.realized_by_tag {
        println!("   cohort {tag:<28} realised {m}");
    }
    println!(
        "   open positions {}   ledger digest {:016x}",
        r.open_positions, r.digest
    );
}

fn fade_limits() -> RiskLimits {
    let mut l = RiskLimits::default();
    l.post_only_tags = HashSet::from(["longshot_fade".to_string()]);
    l.max_total_frac = 0.60; // many small bets across many events
    l
}

fn run_fade(s: &synth::PmSynth, equity: i64) -> Report {
    let strat: Vec<Box<dyn Strategy>> =
        vec![Box::new(LongshotFade::new(FadeConfig::default(), None))];
    Engine::new(
        SimVenue::new("sim", SimConfig::default(), s.instruments.clone()),
        s.instruments.clone(),
        Money::dollars(equity),
        RiskGate::new(fade_limits(), Money::dollars(equity)),
        Ledger::in_memory(),
        strat,
    )
    .run(s.events.clone())
    .expect("events in order")
}

fn demo_pm(args: &[String]) {
    let seed: u64 = arg(args, "--seed")
        .and_then(|s| s.parse().ok())
        .unwrap_or(42);
    let n: usize = arg(args, "--markets")
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    println!("SYNTHETIC data — proves the machinery, says nothing about real-market profit.\n");
    let biased = synth::prediction_markets(n, 100, seed, 0.55);
    let control = synth::prediction_markets(n, 100, seed, 1.00);
    let adverse = synth::prediction_markets(n, 100, seed, 2.00);
    print_report(
        "longshot fade | longshots OVERPRICED (true = 0.55 × price)",
        &run_fade(&biased, 10_000),
    );
    print_report(
        "longshot fade | perfectly CALIBRATED control",
        &run_fade(&control, 10_000),
    );
    print_report(
        "longshot fade | longshots UNDERPRICED (true = 2 × price)  ← wrong-regime risk",
        &run_fade(&adverse, 10_000),
    );
}

fn bar_engine(id: &str, strat: Box<dyn Strategy>) -> Engine<SimVenue> {
    let inst = Instrument::linear(id, id, Px::from_f64(0.01)).with_fee(FeeModel::Bps {
        taker: 3.0,
        maker: 1.0,
    });
    let mut limits = RiskLimits::default();
    limits.max_instrument_frac = 0.5;
    limits.max_group_frac = 0.5;
    limits.max_total_frac = 0.5;
    limits.price_band_frac = 0.05;
    limits.max_drawdown_frac = 0.5;
    Engine::new(
        SimVenue::new("sim", SimConfig::default(), vec![inst.clone()]),
        vec![inst],
        Money::dollars(100_000),
        RiskGate::new(limits, Money::dollars(100_000)),
        Ledger::in_memory(),
        vec![strat],
    )
}

fn demo_bars(args: &[String]) {
    let seed: u64 = arg(args, "--seed")
        .and_then(|s| s.parse().ok())
        .unwrap_or(7);
    println!("SYNTHETIC GBM bars (drift +0.02%/bar, vol 0.3%/bar), 3 bps taker fee, 4 bps spread, 2 bps slippage.\n");
    let id = InstrumentId::new("SYN");
    let bars = synth::gbm_bars("SYN", 3000, seed, 0.0002, 0.003);
    let mk: Vec<(&str, Box<dyn Strategy>)> = vec![
        (
            "macd(12,26,9)",
            Box::new(BarStrategy::new(
                id.clone(),
                MacdCross::new(12, 26, 9),
                100,
                false,
                2.0,
            )),
        ),
        (
            "rsi(14) 30/70",
            Box::new(BarStrategy::new(
                id.clone(),
                RsiReversion::new(14, 30.0, 70.0),
                100,
                false,
                2.0,
            )),
        ),
        (
            "bollinger(20,2)",
            Box::new(BarStrategy::new(
                id.clone(),
                BollingerReversion::new(20, 2.0),
                100,
                false,
                2.0,
            )),
        ),
        (
            "dual-thrust(20)",
            Box::new(BarStrategy::new(
                id.clone(),
                DualThrust::new(20, 0.5, 0.5),
                100,
                false,
                2.0,
            )),
        ),
        (
            "heikin-ashi",
            Box::new(BarStrategy::new(
                id.clone(),
                HeikinAshiTrend::new(),
                100,
                false,
                2.0,
            )),
        ),
        (
            "parabolic-sar",
            Box::new(BarStrategy::new(
                id.clone(),
                ParabolicSar::new(0.02, 0.2),
                100,
                false,
                2.0,
            )),
        ),
    ];
    for (name, st) in mk {
        let r = bar_engine("SYN", st).run(bars.clone()).expect("ordered");
        print_report(name, &r);
    }
}

fn demo_regime(args: &[String]) {
    let seed: u64 = arg(args, "--seed")
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    println!("SYNTHETIC alternating regimes (250-bar trends ↔ 250-bar mean-reverting ranges) — the regimes are built in.");
    println!("Same signals, with vs without the regime gate + 2.5×ATR stop + 1%-risk volatility sizing. 3 bps fee, 2 bps slip.\n");
    let id = InstrumentId::new("SYN");
    let exits = Exits {
        atr_stop_mult: Some(2.5),
        max_bars_unprofitable: Some(60),
        cooldown_bars: 5,
    };
    let risk = Sizing::RiskPct {
        pct: 0.01,
        atr_mult: 2.5,
        max_notional_frac: 0.25,
    };
    let mut tot = [0i64; 2];
    for sd in seed..seed + 6 {
        let bars = synth::regime_switching_bars("SYN", 8, 250, sd);
        let plain: Vec<(&str, Box<dyn Strategy>)> = vec![
            (
                "macd",
                Box::new(BarStrategy::new(
                    id.clone(),
                    MacdCross::new(12, 26, 9),
                    100,
                    false,
                    2.0,
                )),
            ),
            (
                "bollinger",
                Box::new(BarStrategy::new(
                    id.clone(),
                    BollingerReversion::new(20, 2.0),
                    100,
                    false,
                    2.0,
                )),
            ),
            (
                "rsi",
                Box::new(BarStrategy::new(
                    id.clone(),
                    RsiReversion::new(14, 30.0, 70.0),
                    100,
                    false,
                    2.0,
                )),
            ),
        ];
        let gated: Vec<(&str, Box<dyn Strategy>)> = vec![
            (
                "macd",
                Box::new(
                    BarStrategy::new(
                        id.clone(),
                        RegimeGated::new(MacdCross::new(12, 26, 9), Home::Trend),
                        0,
                        false,
                        2.0,
                    )
                    .with_exits(exits)
                    .with_sizing(risk),
                ),
            ),
            (
                "bollinger",
                Box::new(
                    BarStrategy::new(
                        id.clone(),
                        RegimeGated::new(BollingerReversion::new(20, 2.0), Home::Reversion),
                        0,
                        false,
                        2.0,
                    )
                    .with_exits(exits)
                    .with_sizing(risk),
                ),
            ),
            (
                "rsi",
                Box::new(
                    BarStrategy::new(
                        id.clone(),
                        RegimeGated::new(RsiReversion::new(14, 30.0, 70.0), Home::Reversion),
                        0,
                        false,
                        2.0,
                    )
                    .with_exits(exits)
                    .with_sizing(risk),
                ),
            ),
        ];
        for ((n, a), (_, b)) in plain.into_iter().zip(gated) {
            let (ra, rb) = (
                bar_engine("SYN", a).run(bars.clone()).unwrap(),
                bar_engine("SYN", b).run(bars.clone()).unwrap(),
            );
            tot[0] += ra.pnl.0;
            tot[1] += rb.pnl.0;
            if sd == seed {
                println!(
                    "   (gated rejects: {:?}, orders {})",
                    rb.counters.reject_reasons, rb.counters.orders_sent
                );
                println!("seed {sd} {n:<10} plain pnl {:>12}  fees {:>10} | gated pnl {:>12}  fees {:>10}", ra.pnl, ra.fees_paid, rb.pnl, rb.fees_paid);
            }
        }
    }
    println!(
        "\nsum over 6 seeds × 3 strategies:  plain {}   gated {}",
        Money(tot[0]),
        Money(tot[1])
    );
}

fn determinism() {
    let s = synth::prediction_markets(80, 80, 99, 0.55);
    let (a, b) = (run_fade(&s, 10_000), run_fade(&s, 10_000));
    println!(
        "run A digest {:016x}\nrun B digest {:016x}\n{}",
        a.digest,
        b.digest,
        if a.digest == b.digest {
            "IDENTICAL ✔"
        } else {
            "DIVERGED ✘"
        }
    );
    if a.digest != b.digest {
        std::process::exit(1);
    }
}

fn replay(args: &[String]) {
    let Some(path) = args.get(0) else {
        eprintln!("usage: tc replay <events.jsonl> --strategy fade|edge [--equity D]");
        std::process::exit(2);
    };
    let equity: i64 = arg(args, "--equity")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000);
    let which = arg(args, "--strategy").unwrap_or_else(|| "fade".into());
    let f = std::fs::File::open(path).unwrap_or_else(|e| {
        eprintln!("cannot open {path}: {e}");
        std::process::exit(2)
    });
    let mut events: Vec<Event> = vec![];
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line.expect("read line");
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Event>(&line) {
            Ok(e) => events.push(e),
            Err(e) => {
                eprintln!("line {}: bad event: {e}", i + 1);
                std::process::exit(2);
            }
        }
    }
    // stable sort keeps same-timestamp events in file order
    events.sort_by_key(|e| e.ts());
    let ids: BTreeSet<InstrumentId> = events
        .iter()
        .filter_map(|e| match e {
            Event::Book(b) => Some(b.instrument.clone()),
            Event::Trade(t) => Some(t.instrument.clone()),
            Event::Signal(s) => Some(s.instrument.clone()),
            Event::Settlement(s) => Some(s.instrument.clone()),
            _ => None,
        })
        .collect();
    let instruments: Vec<Instrument> = ids
        .iter()
        .map(|id| {
            let group = id.0.split(':').next().unwrap_or(&id.0).to_string();
            Instrument::binary(&id.0, &group).with_fee(FeeModel::kalshi())
        })
        .collect();
    let mut limits = fade_limits();
    limits.post_only_tags.insert("signal_edge".into());
    let strat: Vec<Box<dyn Strategy>> = match which.as_str() {
        "edge" => vec![Box::new(SignalEdge::new(EdgeConfig::default()))],
        _ => vec![Box::new(LongshotFade::new(FadeConfig::default(), None))],
    };
    let mut risk = RiskGate::new(limits, Money::dollars(equity));
    if let Some(h) = arg(args, "--halt-file") {
        risk = risk.with_halt_file(h);
    }
    println!(
        "replaying {} events, {} instruments, strategy={which}, equity=${equity}",
        events.len(),
        instruments.len()
    );
    let r = Engine::new(
        SimVenue::new("sim", SimConfig::default(), instruments.clone()),
        instruments,
        Money::dollars(equity),
        risk,
        Ledger::in_memory(),
        strat,
    )
    .run(events)
    .unwrap_or_else(|e| {
        eprintln!("engine error: {e}");
        std::process::exit(1)
    });
    print_report(&format!("replay {path}"), &r);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("demo-pm") => demo_pm(&args[1..]),
        Some("demo-bars") => demo_bars(&args[1..]),
        Some("demo-regime") => demo_regime(&args[1..]),
        Some("determinism") => determinism(),
        Some("replay") => replay(&args[1..]),
        _ => {
            eprintln!("tc <demo-pm|demo-bars|determinism|replay> …   (paper/sim only)");
            std::process::exit(2);
        }
    }
}
