//! tc-strategy: the `Strategy` trait and the strategy library.
//!
//! A strategy sees only `&Ctx` (state as of `ctx.now`) and one `Event`; it returns `Action`s. It cannot reach a
//! venue, cannot read the future, and cannot bypass the risk gate — the runner owns all of that.

use std::collections::{BTreeMap, HashMap};
use tc_core::*;
use tc_ledger::Portfolio;

pub mod bar;
pub mod indicators;
pub mod pairs;
pub mod pm;
pub mod regime;

pub enum Action {
    Place(OrderReq),
    Cancel(String),
    /// Cancel `cancel`, and place `place` ONLY if the cancel was truthful (`Cancelled`). If the venue says
    /// `AlreadyGone` the order probably filled — re-quoting on top of that is how positions run away.
    Replace {
        cancel: String,
        place: OrderReq,
    },
}

pub struct Ctx<'a> {
    pub now: Ts,
    pub portfolio: &'a Portfolio,
    pub marks: &'a BTreeMap<InstrumentId, Px>,
    pub books: &'a HashMap<InstrumentId, Book>,
    pub instruments: &'a HashMap<InstrumentId, Instrument>,
    pub open_orders: &'a [OrderReq],
    /// Multiplier (0..=1) from the risk gate's graduated ladders. Strategies MUST scale their sizes by it.
    pub size_scale: f64,
}

impl<'a> Ctx<'a> {
    /// Evidence counters for a strategy cohort (realisations + wins).
    pub fn cohort(&self, tag: &str) -> tc_ledger::CohortStats {
        self.portfolio.cohort.get(tag).copied().unwrap_or_default()
    }
    /// Net realised PnL (after fees) of a cohort.
    pub fn cohort_pnl(&self, tag: &str) -> Money {
        self.portfolio
            .realized_by_tag
            .get(tag)
            .copied()
            .unwrap_or_default()
    }
    pub fn equity(&self) -> Money {
        self.portfolio.equity(self.marks)
    }
    pub fn pos(&self, id: &InstrumentId) -> Qty {
        self.portfolio.position(id)
    }
    pub fn book(&self, id: &InstrumentId) -> Option<&Book> {
        self.books.get(id)
    }
    pub fn inst(&self, id: &InstrumentId) -> Option<&Instrument> {
        self.instruments.get(id)
    }
}

pub trait Strategy {
    fn name(&self) -> &str;
    fn on_event(&mut self, ctx: &Ctx, ev: &Event) -> Vec<Action>;
    /// Called for every fill of this strategy's orders, before the next event.
    fn on_fill(&mut self, _fill: &Fill) {}
    /// Called when the risk gate or the venue refused one of this strategy's orders. Strategies use it to back off
    /// instead of re-submitting the same refused order on every tick.
    fn on_reject(&mut self, _now: Ts, _req: &OrderReq, _reason: &str) {}
}

/// Round a price to the instrument's tick. `up=true` rounds toward +∞.
pub fn snap(px: Px, tick: Px, up: bool) -> Px {
    let t = tick.0.max(1);
    let q = px.0.div_euclid(t);
    let r = px.0.rem_euclid(t);
    Px(if r == 0 {
        px.0
    } else if up {
        (q + 1) * t
    } else {
        q * t
    })
}

/// Unique, venue-safe (`[A-Za-z0-9-]`, ≤36 chars) client order id.
pub(crate) fn id_gen(prefix: &str, n: &mut u64) -> String {
    *n += 1;
    tc_core::markets::sanitize_client_id(&format!("{prefix}-{n}"))
}

/// Evidence gate shared by every cohort-sized strategy (kelly-criterion + prediction-market-live-ops):
/// * Kelly fraction follows the sample-size schedule (0.10× under 30 realisations, 0.25× to 100, 0.5× after),
///   never above the configured cap;
/// * the risk gate's `size_scale` shrinks sizes during drawdowns / loss streaks (0 ⇒ stand down);
/// * PRE-DECLARED KILL LINE: after `kill_min_results` realisations, a cohort with negative net PnL stops trading.
/// Returns `(kelly_fraction, size_scale)` or `None` if the strategy must stand down.
pub(crate) fn evidence_gate(
    ctx: &Ctx,
    tag: &str,
    kelly_cap: f64,
    kill_min_results: u64,
) -> Option<(f64, f64)> {
    if ctx.size_scale <= 0.0 {
        return None;
    }
    let st = ctx.cohort(tag);
    if st.results >= kill_min_results && ctx.cohort_pnl(tag).0 < 0 {
        return None;
    }
    Some((
        kelly_cap.min(tc_core::sizing::kelly_fraction_for_sample(st.results)),
        ctx.size_scale,
    ))
}
