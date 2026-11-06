//! tc-ledger: portfolio accounting + append-only, hash-chained event log.
//!
//! PnL is derived **only** from venue-confirmed fills and settlements (never from in-memory marks of
//! shadow orders). Every fill carries its cohort tag from the moment it is written.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use tc_core::*;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Position {
    /// Signed: positive long, negative short.
    pub qty: Qty,
    /// Derived: volume-weighted average entry price of the open quantity (`cost / qty`). Display/risk only —
    /// accounting uses `cost`, so integer truncation here can never leak money.
    pub avg_px: Px,
    /// Signed notional paid to open the current quantity: Σ(signed_qty × entry_px). Positive for longs,
    /// negative for shorts (cash received).
    pub cost: Money,
    pub realized: Money,
    /// Cohort tag of the fill that opened the current quantity (settlement PnL is attributed to it).
    pub tag: String,
}

impl Position {
    /// Position with a given quantity opened at a single price.
    pub fn new(qty: Qty, px: Px) -> Position {
        Position {
            qty,
            avg_px: px,
            cost: px * qty,
            realized: Money::ZERO,
            tag: String::new(),
        }
    }
    fn refresh_avg(&mut self) {
        self.avg_px = if self.qty.0 == 0 {
            Px::ZERO
        } else {
            Px(((self.cost.0 as i128 * SCALE as i128) / self.qty.0 as i128) as i64)
        };
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Portfolio {
    pub cash: Money,
    pub fees_paid: Money,
    // BTreeMap: deterministic iteration order → deterministic replays.
    pub positions: BTreeMap<InstrumentId, Position>,
    /// Realised PnL by cohort tag (strategy/band), net of fees.
    pub realized_by_tag: BTreeMap<String, Money>,
    /// Evidence counters per cohort: how many realisations (closing fills + settlements), how many were wins.
    /// Sizing and kill lines are gated on these — never on a backtest.
    pub cohort: BTreeMap<String, CohortStats>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortStats {
    pub results: u64,
    pub wins: u64,
}

impl Portfolio {
    pub fn new(cash: Money) -> Self {
        Portfolio {
            cash,
            fees_paid: Money::ZERO,
            positions: BTreeMap::new(),
            realized_by_tag: BTreeMap::new(),
            cohort: BTreeMap::new(),
        }
    }

    pub fn position(&self, id: &InstrumentId) -> Qty {
        self.positions.get(id).map(|p| p.qty).unwrap_or(Qty::ZERO)
    }

    /// Apply a venue-confirmed fill.
    /// Returns the realised PnL (before this fill's fee) if the fill closed any quantity, else zero.
    pub fn apply_fill(&mut self, f: &Fill) -> Money {
        let signed = Qty(f.qty.0 * f.side.sign());
        let notional = f.px * f.qty;
        match f.side {
            Side::Buy => self.cash -= notional,
            Side::Sell => self.cash += notional,
        }
        self.cash -= f.fee;
        self.fees_paid += f.fee;

        let pos = self.positions.entry(f.instrument.clone()).or_default();
        let mut realized = Money::ZERO;
        if pos.qty.0 == 0 || pos.qty.signum() == signed.signum() {
            // opening / adding: cost basis grows by the signed notional
            if pos.qty.0 == 0 {
                pos.tag = f.tag.clone();
            }
            pos.cost += f.px * signed;
            pos.qty += signed;
        } else {
            // reducing / flipping. Realised = value of the closed piece at the exit price − its share of cost.
            let closing = Qty(pos.qty.abs().0.min(f.qty.0));
            let sign = pos.qty.signum();
            let cost_portion =
                Money(((pos.cost.0 as i128 * closing.0 as i128) / pos.qty.abs().0 as i128) as i64);
            let value = f.px * Qty(closing.0 * sign);
            realized = value - cost_portion;
            pos.cost -= cost_portion;
            pos.qty += signed;
            let leftover = f.qty.0 - closing.0;
            if leftover > 0 {
                // flipped: the leftover opens a fresh position. Its cost is derived from the fill's TOTAL signed
                // notional (exactly what moved through cash) minus the closing leg, so the two halves always
                // sum to the cash flow — truncating them separately leaks raw units.
                pos.cost = (f.px * signed) + value;
                pos.tag = f.tag.clone();
            }
            pos.realized += realized;
        }
        pos.refresh_avg();
        *self.realized_by_tag.entry(f.tag.clone()).or_default() += realized - f.fee;
        if realized.0 != 0 {
            let c = self.cohort.entry(f.tag.clone()).or_default();
            c.results += 1;
            if realized.0 > 0 {
                c.wins += 1;
            }
        }
        realized
    }

    /// Settle an instrument at the venue-reported price (binary: 1.00 or 0.00).
    /// Returns the realised PnL of the settlement.
    pub fn apply_settlement(&mut self, id: &InstrumentId, price: Px) -> Money {
        let Some(pos) = self.positions.get_mut(id) else {
            return Money::ZERO;
        };
        if pos.qty.0 == 0 {
            return Money::ZERO;
        }
        // long gets price×qty, short pays price×|qty| (qty is negative → same formula)
        self.cash += price * pos.qty;
        let pnl = price * pos.qty - pos.cost;
        pos.realized += pnl;
        *self.realized_by_tag.entry(pos.tag.clone()).or_default() += pnl;
        let c = self.cohort.entry(pos.tag.clone()).or_default();
        c.results += 1;
        if pnl.0 > 0 {
            c.wins += 1;
        }
        pos.qty = Qty::ZERO;
        pos.cost = Money::ZERO;
        pos.avg_px = Px::ZERO;
        pnl
    }

    /// Mark-to-market equity. `marks` maps instrument → last mark; instruments with no mark are valued
    /// at their average entry price (conservative: no unrealised gain assumed).
    pub fn equity(&self, marks: &BTreeMap<InstrumentId, Px>) -> Money {
        let mut e = self.cash;
        for (id, pos) in &self.positions {
            if pos.qty.0 != 0 {
                let m = marks.get(id).copied().unwrap_or(pos.avg_px);
                e += m * pos.qty;
            }
        }
        e
    }

    pub fn realized_total(&self) -> Money {
        self.positions.values().map(|p| p.realized).sum::<Money>() - self.fees_paid
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Record {
    Fill(Fill),
    Settlement {
        ts: Ts,
        instrument: InstrumentId,
        price: Px,
        pnl: Money,
    },
    Reject {
        ts: Ts,
        client_id: String,
        reason: String,
    },
    Halt {
        ts: Ts,
        reason: String,
    },
    Note {
        ts: Ts,
        text: String,
    },
}

/// Append-only log with a chained FNV-1a digest: any divergence between two runs changes the digest.
pub struct Ledger {
    records: Vec<Record>,
    digest: u64,
    sink: Option<File>,
}

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

impl Ledger {
    pub fn in_memory() -> Self {
        Ledger {
            records: Vec::new(),
            digest: FNV_OFFSET,
            sink: None,
        }
    }

    /// Also append every record as one JSON line to `path` (created if absent, never truncated).
    pub fn with_file(path: &Path) -> std::io::Result<Self> {
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Ledger {
            records: Vec::new(),
            digest: FNV_OFFSET,
            sink: Some(f),
        })
    }

    pub fn append(&mut self, r: Record) {
        let line = serde_json::to_string(&r).expect("ledger record serialises");
        for b in line.as_bytes() {
            self.digest ^= *b as u64;
            self.digest = self.digest.wrapping_mul(FNV_PRIME);
        }
        if let Some(f) = self.sink.as_mut() {
            // A ledger that cannot persist must stop the engine rather than trade unrecorded.
            writeln!(f, "{line}").expect("ledger write failed — refusing to continue unrecorded");
        }
        self.records.push(r);
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }
    pub fn digest(&self) -> u64 {
        self.digest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(side: Side, px: Px, qty: i64, fee: Money) -> Fill {
        Fill {
            client_id: "c".into(),
            instrument: InstrumentId::new("M:YES"),
            side,
            px,
            qty: Qty::units(qty),
            fee,
            liquidity: Liquidity::Taker,
            ts: Ts(0),
            tag: "t".into(),
        }
    }

    #[test]
    fn long_then_win_settlement() {
        let mut p = Portfolio::new(Money::dollars(100));
        p.apply_fill(&fill(Side::Buy, Px::cents(20), 10, Money::ZERO)); // pay $2
        assert_eq!(p.cash, Money::dollars(98));
        let pnl = p.apply_settlement(&InstrumentId::new("M:YES"), Px::ONE);
        assert_eq!(pnl, Money::dollars(8)); // (1.00-0.20)*10
        assert_eq!(p.cash, Money::dollars(108));
    }

    #[test]
    fn short_longshot_loses_when_it_hits() {
        let mut p = Portfolio::new(Money::dollars(100));
        p.apply_fill(&fill(Side::Sell, Px::cents(10), 10, Money::ZERO)); // receive $1
        let pnl = p.apply_settlement(&InstrumentId::new("M:YES"), Px::ONE);
        assert_eq!(pnl, Money::dollars(-9));
        assert_eq!(p.cash, Money::dollars(91)); // 100 + 1 - 10
    }

    #[test]
    fn short_longshot_wins_when_it_misses() {
        let mut p = Portfolio::new(Money::dollars(100));
        p.apply_fill(&fill(Side::Sell, Px::cents(10), 10, Money::ZERO));
        let pnl = p.apply_settlement(&InstrumentId::new("M:YES"), Px::ZERO);
        assert_eq!(pnl, Money::dollars(1));
        assert_eq!(p.cash, Money::dollars(101));
    }

    #[test]
    fn partial_close_and_flip() {
        let mut p = Portfolio::new(Money::dollars(1000));
        p.apply_fill(&fill(Side::Buy, Px::from_f64(10.0), 10, Money::ZERO));
        p.apply_fill(&fill(Side::Sell, Px::from_f64(12.0), 4, Money::ZERO));
        let pos = &p.positions[&InstrumentId::new("M:YES")];
        assert_eq!(pos.qty, Qty::units(6));
        assert_eq!(pos.realized, Money::dollars(8)); // 4 × $2
                                                     // sell 10 more: closes 6 at 13 (+18), flips to short 4 at 13
        p.apply_fill(&fill(Side::Sell, Px::from_f64(13.0), 10, Money::ZERO));
        let pos = &p.positions[&InstrumentId::new("M:YES")];
        assert_eq!(pos.qty, Qty::units(-4));
        assert_eq!(pos.avg_px, Px::from_f64(13.0));
        assert_eq!(pos.realized, Money::dollars(26));
    }

    #[test]
    fn settlement_pnl_is_attributed_to_opening_cohort() {
        let mut p = Portfolio::new(Money::dollars(100));
        p.apply_fill(&fill(Side::Buy, Px::cents(20), 10, Money::ZERO)); // tag "t"
        p.apply_settlement(&InstrumentId::new("M:YES"), Px::ONE);
        assert_eq!(p.realized_by_tag["t"], Money::dollars(8));
    }

    #[test]
    fn books_close_exactly_through_many_odd_lot_trades() {
        // cash must equal start + Σrealized − fees exactly, however ugly the prices (no truncation leaks)
        let mut p = Portfolio::new(Money::dollars(1_000));
        let id = InstrumentId::new("M:YES");
        let mut seed = 12345u64;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as i64
        };
        for _ in 0..500 {
            let side = if next() % 2 == 0 {
                Side::Buy
            } else {
                Side::Sell
            };
            let px = Px(100 + next() % 9_800); // arbitrary raw prices, not cent-aligned
            let qty = Qty(10_000 + (next() % 70_000) * 7); // odd, non-whole quantities
            let mut f = fill(side, px, 1, Money(next() % 40));
            f.qty = qty;
            p.apply_fill(&f);
        }
        p.apply_settlement(&id, Px::cents(37));
        let realised: i64 = p.positions.values().map(|x| x.realized.0).sum();
        assert_eq!(p.cash.0, Money::dollars(1_000).0 + realised - p.fees_paid.0);
    }

    #[test]
    fn cohort_counts_results_and_wins() {
        let mut p = Portfolio::new(Money::dollars(100));
        p.apply_fill(&fill(Side::Buy, Px::cents(20), 10, Money::ZERO));
        assert_eq!(p.cohort.get("t").copied().unwrap_or_default().results, 0); // opening is not a result
        let r = p.apply_fill(&fill(Side::Sell, Px::cents(30), 10, Money::ZERO));
        assert!(r.0 > 0);
        p.apply_fill(&fill(Side::Buy, Px::cents(40), 5, Money::ZERO));
        p.apply_settlement(&InstrumentId::new("M:YES"), Px::ZERO); // loss
        let c = p.cohort["t"];
        assert_eq!((c.results, c.wins), (2, 1));
    }

    #[test]
    fn fees_reduce_cohort_pnl() {
        let mut p = Portfolio::new(Money::dollars(100));
        p.apply_fill(&fill(Side::Buy, Px::cents(50), 2, Money::from_f64(0.04)));
        assert_eq!(p.realized_by_tag["t"], Money::from_f64(-0.04));
    }

    #[test]
    fn digest_changes_with_content_and_is_reproducible() {
        let mk = |n: i64| {
            let mut l = Ledger::in_memory();
            l.append(Record::Note {
                ts: Ts(1),
                text: "a".into(),
            });
            l.append(Record::Fill(fill(Side::Buy, Px::cents(50), n, Money::ZERO)));
            l.digest()
        };
        assert_eq!(mk(1), mk(1));
        assert_ne!(mk(1), mk(2));
    }
}
