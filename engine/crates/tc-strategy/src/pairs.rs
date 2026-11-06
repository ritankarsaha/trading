//! Pairs trading (stat-arb) ported from `quant-trading/Pair trading backtest.py`: trade the z-score of the
//! log-price spread `ln A − β·ln B`. β is fitted in research (cointegration); here it is a parameter.

use crate::indicators::Rolling;
use crate::*;

pub struct PairsZ {
    a: InstrumentId,
    b: InstrumentId,
    beta: f64,
    entry_z: f64,
    exit_z: f64,
    units_a: i64,
    units_b: i64,
    slippage_bps: f64,
    spread: Rolling,
    last: [(Ts, f64); 2],
    dir: i8,
    tag: String,
    n: u64,
}

impl PairsZ {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        a: InstrumentId,
        b: InstrumentId,
        beta: f64,
        window: usize,
        entry_z: f64,
        exit_z: f64,
        units_a: i64,
        units_b: i64,
        slippage_bps: f64,
    ) -> Self {
        let tag = format!("pairs:{a}/{b}");
        PairsZ {
            a,
            b,
            beta,
            entry_z,
            exit_z,
            units_a,
            units_b,
            slippage_bps,
            spread: Rolling::new(window),
            last: [(Ts(i64::MIN), 0.0); 2],
            dir: 0,
            tag,
            n: 0,
        }
    }

    fn leg(
        &mut self,
        ctx: &Ctx,
        id: &InstrumentId,
        target_units: i64,
        px_f: f64,
        out: &mut Vec<Action>,
    ) {
        let delta = Qty::units(target_units) - ctx.pos(id);
        if delta.0 == 0 {
            return;
        }
        let Some(inst) = ctx.inst(id) else { return };
        let side = if delta.0 > 0 { Side::Buy } else { Side::Sell };
        let slip = self.slippage_bps / 10_000.0;
        let px = snap(
            Px::from_f64(px_f * (1.0 + slip * side.sign() as f64)),
            inst.tick,
            side == Side::Buy,
        );
        out.push(Action::Place(OrderReq {
            client_id: id_gen(&self.tag, &mut self.n),
            instrument: id.clone(),
            side,
            px,
            qty: delta.abs(),
            tif: Tif::Ioc,
            post_only: false,
            tag: self.tag.clone(),
        }));
    }
}

impl Strategy for PairsZ {
    fn name(&self) -> &str {
        &self.tag
    }

    fn on_event(&mut self, ctx: &Ctx, ev: &Event) -> Vec<Action> {
        let Event::Bar(bar) = ev else { return vec![] };
        let k = if bar.instrument == self.a {
            0
        } else if bar.instrument == self.b {
            1
        } else {
            return vec![];
        };
        self.last[k] = (bar.ts, bar.close.as_f64());
        // act only when both legs have a bar for the same timestamp (no stale-leg trading)
        if self.last[0].0 != self.last[1].0 {
            return vec![];
        }
        let (pa, pb) = (self.last[0].1, self.last[1].1);
        self.spread.push(pa.ln() - self.beta * pb.ln());
        if !self.spread.full() {
            return vec![];
        }
        let sd = self.spread.std();
        if sd <= 0.0 {
            return vec![];
        }
        let z = (pa.ln() - self.beta * pb.ln() - self.spread.mean()) / sd;
        if self.dir == 0 {
            if z < -self.entry_z {
                self.dir = 1; // spread cheap: long A, short B
            } else if z > self.entry_z {
                self.dir = -1;
            }
        } else if z.abs() < self.exit_z {
            self.dir = 0;
        }
        let d = self.dir as i64;
        let (ia, ib) = (self.a.clone(), self.b.clone());
        let (ua, ub) = (self.units_a, self.units_b);
        let mut out = vec![];
        self.leg(ctx, &ia, d * ua, pa, &mut out);
        self.leg(ctx, &ib, -d * ub, pb, &mut out);
        out
    }
}
