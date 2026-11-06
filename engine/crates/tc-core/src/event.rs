//! Market events + the `Venue` trait. Everything a strategy can ever see is an `Event` with a timestamp;
//! the runner delivers them in non-decreasing time order, so future data is unreachable by construction.

use crate::types::*;
use crate::units::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Level {
    pub px: Px,
    pub qty: Qty,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Book {
    pub instrument: InstrumentId,
    pub ts: Ts,
    /// Best (highest) first.
    pub bids: Vec<Level>,
    /// Best (lowest) first.
    pub asks: Vec<Level>,
}

impl Book {
    pub fn best_bid(&self) -> Option<Level> {
        self.bids.first().copied()
    }
    pub fn best_ask(&self) -> Option<Level> {
        self.asks.first().copied()
    }
    pub fn mid(&self) -> Option<Px> {
        Some(Px((self.best_bid()?.px.0 + self.best_ask()?.px.0) / 2))
    }
    pub fn spread(&self) -> Option<Px> {
        Some(self.best_ask()?.px - self.best_bid()?.px)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TradeTick {
    pub instrument: InstrumentId,
    pub ts: Ts,
    pub px: Px,
    pub qty: Qty,
    /// The aggressor. `Buy` means a taker bought (hit the ask).
    pub taker_side: Side,
}

/// OHLCV bar, stamped with its **close** time. A strategy sees a bar only once it has closed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    pub instrument: InstrumentId,
    pub ts: Ts,
    pub open: Px,
    pub high: Px,
    pub low: Px,
    pub close: Px,
    pub volume: Qty,
}

/// A probability / score published by the research plane (Python: Kronos, TradingAgents, calibrated models…).
/// `ts` is when the signal became *available*, never the time of the data it describes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    pub ts: Ts,
    pub source: String,
    pub instrument: InstrumentId,
    /// Model probability that the instrument (binary) resolves YES / that price rises, in (0,1).
    pub prob: Prob,
    /// Optional strength/confidence in [0,1].
    pub confidence: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settlement {
    pub instrument: InstrumentId,
    pub ts: Ts,
    /// Venue-reported result: `Px::ONE` (YES) or `Px::ZERO` (NO) for binaries. Never self-derived.
    pub price: Px,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    Book(Book),
    Trade(TradeTick),
    Bar(Bar),
    Signal(Signal),
    Settlement(Settlement),
    /// Periodic wake-up so strategies can requote without market data.
    Timer(Ts),
}

impl Event {
    pub fn ts(&self) -> Ts {
        match self {
            Event::Book(b) => b.ts,
            Event::Trade(t) => t.ts,
            Event::Bar(b) => b.ts,
            Event::Signal(s) => s.ts,
            Event::Settlement(s) => s.ts,
            Event::Timer(t) => *t,
        }
    }
}

/// The only way orders reach a market. `SimVenue` implements it for backtest/paper; each real platform
/// (Glimpse, Polymarket, Kalshi, Zerodha, Angel One) implements it in its own `venues/<name>` crate.
///
/// The trait is deliberately synchronous: it keeps the runner deterministic. Live adapters own their own
/// async runtime/WebSocket tasks and expose the results through `drain_fills` / `feed`.
pub trait Venue {
    fn name(&self) -> &str;
    /// Feed a market event (sim: updates books and matches resting orders; live: usually a no-op).
    fn feed(&mut self, ev: &Event);
    fn place(&mut self, now: Ts, req: OrderReq) -> Result<(), PlaceError>;
    fn cancel(&mut self, client_id: &str) -> CancelResult;
    /// Orders currently resting at the venue.
    fn open_orders(&self) -> Vec<OrderReq>;
    /// Fills since the last call (in venue order). The runner applies them to the portfolio + ledger.
    fn drain_fills(&mut self) -> Vec<Fill>;
}
