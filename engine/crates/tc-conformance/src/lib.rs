//! tc-conformance: the contract every `Venue` implementation must satisfy before it may touch the engine.
//!
//! Each rule comes from an incident in the live-ops doctrine (`prediction-market-live-ops`): swallowed cancel
//! failures stacking positions, post-only silently becoming a taker, double-counted fills.
//!
//! Usage from a venue crate (against its sandbox/paper endpoint or a faithful mock):
//! ```ignore
//! tc_conformance::run_all(|| MyVenue::sandbox(), &tc_conformance::Fixture::default());
//! ```
//! The venue must list instrument `Fixture::instrument` and accept `feed()` of a `Book` for it (a live adapter
//! implements `feed` as "inject into the mock/sandbox"; the sim does so natively).

use tc_core::*;

pub struct Fixture {
    pub instrument: &'static str,
    pub bid: Px,
    pub ask: Px,
}
impl Default for Fixture {
    fn default() -> Self {
        Fixture {
            instrument: "C:YES",
            bid: Px::cents(48),
            ask: Px::cents(50),
        }
    }
}

fn seeded<V: Venue>(mk: &impl Fn() -> V, fx: &Fixture) -> V {
    let mut v = mk();
    v.feed(&Event::Book(Book {
        instrument: InstrumentId::new(fx.instrument),
        ts: Ts(1),
        bids: vec![Level {
            px: fx.bid,
            qty: Qty::units(100),
        }],
        asks: vec![Level {
            px: fx.ask,
            qty: Qty::units(100),
        }],
    }));
    v
}

fn order(
    fx: &Fixture,
    id: &str,
    side: Side,
    px: Px,
    qty: i64,
    tif: Tif,
    post_only: bool,
) -> OrderReq {
    OrderReq {
        client_id: id.into(),
        instrument: InstrumentId::new(fx.instrument),
        side,
        px,
        qty: Qty::units(qty),
        tif,
        post_only,
        tag: "conf".into(),
    }
}

pub fn run_all<V: Venue>(mk: impl Fn() -> V, fx: &Fixture) {
    duplicate_client_id_is_rejected(&mk, fx);
    post_only_never_crosses(&mk, fx);
    cancel_is_truthful(&mk, fx);
    ioc_never_rests(&mk, fx);
    fills_are_delivered_exactly_once_and_well_formed(&mk, fx);
    resting_order_visible_then_gone_after_cancel(&mk, fx);
}

pub fn duplicate_client_id_is_rejected<V: Venue>(mk: &impl Fn() -> V, fx: &Fixture) {
    let mut v = seeded(mk, fx);
    let o = order(
        fx,
        "dup-1",
        Side::Buy,
        fx.bid - Px::cents(5),
        1,
        Tif::Gtc,
        true,
    );
    v.place(Ts(1), o.clone()).expect("first placement accepted");
    let e = v
        .place(Ts(1), o)
        .expect_err("same client_id twice must be refused (idempotency)");
    assert!(
        matches!(e, PlaceError::Duplicate(_)),
        "expected Duplicate, got {e:?}"
    );
}

pub fn post_only_never_crosses<V: Venue>(mk: &impl Fn() -> V, fx: &Fixture) {
    let mut v = seeded(mk, fx);
    let e = v.place(
        Ts(1),
        order(fx, "po-1", Side::Buy, fx.ask, 1, Tif::Gtc, true),
    );
    assert_eq!(
        e,
        Err(PlaceError::PostOnlyWouldCross),
        "post-only that crosses must be rejected, not taken"
    );
    assert!(
        v.open_orders().is_empty(),
        "a rejected post-only must not rest"
    );
    assert!(
        v.drain_fills().is_empty(),
        "a rejected post-only must not fill"
    );
}

pub fn cancel_is_truthful<V: Venue>(mk: &impl Fn() -> V, fx: &Fixture) {
    let mut v = seeded(mk, fx);
    // unknown id: NOT "Cancelled"
    assert_ne!(
        v.cancel("nope"),
        CancelResult::Cancelled,
        "cancelling an unknown order is not a cancel"
    );
    // fully-filled order: AlreadyGone, never Cancelled
    v.place(
        Ts(1),
        order(fx, "ioc-1", Side::Buy, fx.ask, 1, Tif::Ioc, false),
    )
    .unwrap();
    let _ = v.drain_fills();
    assert_ne!(
        v.cancel("ioc-1"),
        CancelResult::Cancelled,
        "a filled order cannot be 'cancelled'"
    );
}

pub fn ioc_never_rests<V: Venue>(mk: &impl Fn() -> V, fx: &Fixture) {
    let mut v = seeded(mk, fx);
    // IOC priced away from the market: no fill, and nothing may rest
    let _ = v.place(
        Ts(1),
        order(
            fx,
            "ioc-away",
            Side::Buy,
            fx.bid - Px::cents(10),
            1,
            Tif::Ioc,
            false,
        ),
    );
    assert!(
        v.open_orders().iter().all(|o| o.client_id != "ioc-away"),
        "IOC remainder must be cancelled"
    );
}

pub fn fills_are_delivered_exactly_once_and_well_formed<V: Venue>(
    mk: &impl Fn() -> V,
    fx: &Fixture,
) {
    let mut v = seeded(mk, fx);
    v.place(
        Ts(1),
        order(fx, "t-1", Side::Buy, fx.ask, 3, Tif::Ioc, false),
    )
    .unwrap();
    let f = v.drain_fills();
    assert!(
        !f.is_empty(),
        "marketable IOC against displayed depth must fill"
    );
    for x in &f {
        assert!(
            x.qty.0 > 0,
            "fill qty must be positive (direction lives in `side`)"
        );
        assert_eq!(x.tag, "conf", "cohort tag must survive to the fill");
        assert_eq!(x.liquidity, Liquidity::Taker);
    }
    assert!(
        v.drain_fills().is_empty(),
        "fills must be delivered exactly once"
    );
    let total: i64 = f.iter().map(|x| x.qty.0).sum();
    assert!(total <= Qty::units(3).0, "cannot fill more than ordered");
}

pub fn resting_order_visible_then_gone_after_cancel<V: Venue>(mk: &impl Fn() -> V, fx: &Fixture) {
    let mut v = seeded(mk, fx);
    v.place(
        Ts(1),
        order(
            fx,
            "r-1",
            Side::Buy,
            fx.bid - Px::cents(5),
            2,
            Tif::Gtc,
            true,
        ),
    )
    .unwrap();
    assert!(
        v.open_orders().iter().any(|o| o.client_id == "r-1"),
        "resting order must be listed"
    );
    assert_eq!(v.cancel("r-1"), CancelResult::Cancelled);
    assert!(
        v.open_orders().iter().all(|o| o.client_id != "r-1"),
        "cancelled order must be gone"
    );
    assert_eq!(
        v.cancel("r-1"),
        CancelResult::AlreadyGone,
        "second cancel must say AlreadyGone"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tc_sim::{SimConfig, SimVenue};

    #[test]
    fn sim_venue_conforms() {
        run_all(
            || {
                SimVenue::new(
                    "sim",
                    SimConfig::default(),
                    vec![Instrument::binary("C:YES", "C")],
                )
            },
            &Fixture::default(),
        );
    }

    /// A venue that swallows cancel failures by claiming success must be caught by the suite.
    #[test]
    #[should_panic(expected = "unknown order")]
    fn liar_venue_is_caught() {
        struct Liar(SimVenue);
        impl Venue for Liar {
            fn name(&self) -> &str {
                "liar"
            }
            fn feed(&mut self, e: &Event) {
                self.0.feed(e)
            }
            fn place(&mut self, n: Ts, r: OrderReq) -> Result<(), PlaceError> {
                self.0.place(n, r)
            }
            fn cancel(&mut self, _id: &str) -> CancelResult {
                CancelResult::Cancelled
            } // always lies
            fn open_orders(&self) -> Vec<OrderReq> {
                self.0.open_orders()
            }
            fn drain_fills(&mut self) -> Vec<Fill> {
                self.0.drain_fills()
            }
        }
        cancel_is_truthful(
            &|| {
                Liar(SimVenue::new(
                    "sim",
                    SimConfig::default(),
                    vec![Instrument::binary("C:YES", "C")],
                ))
            },
            &Fixture::default(),
        );
    }
}
