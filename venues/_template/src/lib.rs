//! Venue adapter skeleton. Implement `tc_core::Venue` for the platform, then make the conformance test pass.
//!
//! Adapter responsibilities (and ONLY these — strategy and risk live in the engine):
//!   * authenticate, keep sessions alive (token refresh, reconnects with backoff),
//!   * translate `OrderReq` ↔ native order; prices/quantities cross the boundary as fixed-point `Px`/`Qty`,
//!   * map native fills to `Fill` (maker/taker flag, exact fee as the venue charged it),
//!   * truthful `cancel`: Cancelled / AlreadyGone (probably filled) / Failed — never swallow errors,
//!   * route exchange shards / accounts consistently on EVERY call (place, read, cancel, collateral),
//!   * obey the venue's rate limits (measure them with dust orders; see `prediction-market-live-ops`),
//!   * expose venue-confirmed fills/positions so the engine can reconcile — the venue is the source of truth.
use tc_core::*;

pub struct TemplateVenue;

impl Venue for TemplateVenue {
    fn name(&self) -> &str {
        "template"
    }
    fn feed(&mut self, _ev: &Event) {}
    fn place(&mut self, _now: Ts, _req: OrderReq) -> Result<(), PlaceError> {
        Err(PlaceError::Venue("not implemented".into()))
    }
    fn cancel(&mut self, _client_id: &str) -> CancelResult {
        CancelResult::Failed("not implemented".into())
    }
    fn open_orders(&self) -> Vec<OrderReq> {
        vec![]
    }
    fn drain_fills(&mut self) -> Vec<Fill> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Remove `#[ignore]` once the adapter is implemented — a venue that fails conformance must not trade.
    #[test]
    #[ignore = "template: not implemented"]
    fn conforms() {
        tc_conformance::run_all(|| TemplateVenue, &tc_conformance::Fixture::default());
    }

    #[test]
    fn template_builds_and_names_itself() {
        assert_eq!(TemplateVenue.name(), "template");
    }
}
