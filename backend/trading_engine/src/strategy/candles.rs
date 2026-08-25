//! Minute-bar aggregation from the live tick stream.
//!
//! Kotak exposes **no historical candle API** (see `kotak-api-docs/` — quotes
//! return a snapshot, never a series). Every indicator this strategy relies on
//! therefore has to be built from bars we construct ourselves, which creates a
//! cold-start problem: at 09:15 an in-memory-only aggregator knows nothing, and
//! an EMA/ATR needs tens of bars before it means anything.
//!
//! The fix is persistence. Bars are appended to SQLite as they close and
//! reloaded at startup, so only the very first session ever runs cold and a
//! mid-day restart resumes with its history intact.
//!
//! # Volume handling
//!
//! The feed reports **cumulative session volume**, not per-tick volume. Bar
//! volume is the delta between consecutive cumulative readings. Two cases are
//! deliberately clamped to zero rather than trusted:
//!
//! - a *negative* delta, which means the session counter reset (new day) or the
//!   feed re-sent a stale frame;
//! - the first reading after a restart, where the previous cumulative baseline
//!   is unknown and a raw delta would look like a huge fake volume spike.
//!
//! An inflated volume bar would read as a breakout to the order-flow agent, so
//! under-reporting is the safe direction to err.

use std::collections::VecDeque;

/// One closed (or in-progress) minute bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Candle {
    /// Bar start, as whole minutes since the Unix epoch. Integer minute keys
    /// avoid every timezone/DST question at comparison time.
    pub minute: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Traded volume within this bar (0 when the feed carries none, as for
    /// index feeds).
    pub volume: f64,
    /// Number of ticks folded into the bar — a thin bar is less trustworthy.
    pub ticks: u32,
}

impl Candle {
    fn new(minute: i64, price: f64) -> Self {
        Self { minute, open: price, high: price, low: price, close: price, volume: 0.0, ticks: 1 }
    }

    /// Typical price `(H + L + C) / 3`, the standard VWAP input.
    pub fn typical(&self) -> f64 {
        (self.high + self.low + self.close) / 3.0
    }

    /// True range against the previous bar's close (Wilder). Falls back to the
    /// bar's own range when there is no previous close.
    pub fn true_range(&self, prev_close: Option<f64>) -> f64 {
        let hl = self.high - self.low;
        match prev_close {
            Some(pc) => hl.max((self.high - pc).abs()).max((self.low - pc).abs()),
            None => hl,
        }
    }
}

/// Rolling minute-bar history for a single instrument.
#[derive(Debug, Clone)]
pub struct CandleSeries {
    /// Feed key this series tracks (`"nse_cm|Nifty 50"`, `"nse_fo|51386"`, …).
    pub key: String,
    closed: VecDeque<Candle>,
    working: Option<Candle>,
    /// Last cumulative volume reading, to difference against.
    last_cum_volume: Option<f64>,
    capacity: usize,
}

impl CandleSeries {
    pub fn new(key: impl Into<String>, capacity: usize) -> Self {
        Self {
            key: key.into(),
            closed: VecDeque::with_capacity(capacity.min(1024)),
            working: None,
            last_cum_volume: None,
            capacity: capacity.max(2),
        }
    }

    /// Seed the series with bars loaded from the database.
    ///
    /// Input is assumed ascending by minute; the newest `capacity` bars are
    /// kept. These land in `closed` only — a reloaded bar is never resumed as
    /// the working bar, since its within-minute tick state is gone.
    pub fn seed(&mut self, bars: impl IntoIterator<Item = Candle>) {
        for b in bars {
            self.closed.push_back(b);
        }
        while self.closed.len() > self.capacity {
            self.closed.pop_front();
        }
    }

    /// Fold one tick into the series.
    ///
    /// `epoch_ms` is the local receive time; `cum_volume` is the feed's
    /// cumulative session volume, if it supplies one. Returns the bar that just
    /// *closed*, if this tick rolled the series into a new minute — callers
    /// persist that bar.
    pub fn push_tick(&mut self, price: f64, cum_volume: Option<f64>, epoch_ms: i64) -> Option<Candle> {
        if !price.is_finite() || price <= 0.0 {
            return None;
        }
        let minute = epoch_ms.div_euclid(60_000);

        // Volume delta, clamped — see the module note on why.
        let vol_delta = match (cum_volume, self.last_cum_volume) {
            (Some(cur), Some(prev)) if cur >= prev => cur - prev,
            _ => 0.0,
        };
        if let Some(cur) = cum_volume {
            self.last_cum_volume = Some(cur);
        }

        match self.working.as_mut() {
            Some(w) if w.minute == minute => {
                w.high = w.high.max(price);
                w.low = w.low.min(price);
                w.close = price;
                w.volume += vol_delta;
                w.ticks += 1;
                None
            }
            // A tick older than the working bar (out-of-order frame) is dropped
            // rather than allowed to rewrite a bar that already closed.
            Some(w) if minute < w.minute => None,
            _ => {
                let mut fresh = Candle::new(minute, price);
                fresh.volume = vol_delta;
                let finished = self.working.replace(fresh);
                if let Some(done) = finished.clone() {
                    self.closed.push_back(done);
                    while self.closed.len() > self.capacity {
                        self.closed.pop_front();
                    }
                }
                finished
            }
        }
    }

    /// Closed bars, oldest first. Excludes the in-progress bar, which is not
    /// yet a valid indicator input.
    pub fn closed(&self) -> &VecDeque<Candle> {
        &self.closed
    }

    /// The in-progress bar, if any.
    pub fn working(&self) -> Option<&Candle> {
        self.working.as_ref()
    }

    /// Number of closed bars available — the warm-up gate reads this.
    pub fn len(&self) -> usize {
        self.closed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.closed.is_empty()
    }

    /// Closing prices of all closed bars, oldest first.
    pub fn closes(&self) -> Vec<f64> {
        self.closed.iter().map(|c| c.close).collect()
    }

    /// Most recent close, preferring the live working bar when one exists.
    pub fn last_price(&self) -> Option<f64> {
        self.working.as_ref().map(|w| w.close).or_else(|| self.closed.back().map(|c| c.close))
    }

    /// Bars belonging to the current session, identified by sharing a UTC day
    /// with the newest bar. Session VWAP and opening-range logic use this so a
    /// reloaded multi-day history does not bleed into today's levels.
    pub fn session_bars(&self) -> Vec<&Candle> {
        let Some(last) = self.closed.back().or(self.working.as_ref()) else {
            return Vec::new();
        };
        // IST is UTC+5:30 and the session runs 09:15–15:40 IST (03:45–10:10
        // UTC), so an IST trading day never straddles a UTC date boundary.
        let day = last.minute.div_euclid(1440);
        self.closed
            .iter()
            .chain(self.working.iter())
            .filter(|c| c.minute.div_euclid(1440) == day)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: i64 = 60_000;

    #[test]
    fn ticks_fold_into_one_bar_then_roll() {
        let mut s = CandleSeries::new("k", 100);
        assert_eq!(s.push_tick(100.0, None, 0), None, "first tick opens a bar, closes nothing");
        s.push_tick(105.0, None, 10_000);
        s.push_tick(95.0, None, 20_000);
        s.push_tick(102.0, None, 30_000);

        assert_eq!(s.len(), 0, "bar is still in progress");
        let w = s.working().expect("working bar");
        assert_eq!((w.open, w.high, w.low, w.close), (100.0, 105.0, 95.0, 102.0));
        assert_eq!(w.ticks, 4);

        // Crossing into the next minute closes the first bar.
        let closed = s.push_tick(103.0, None, M).expect("bar should close");
        assert_eq!(closed.close, 102.0);
        assert_eq!(closed.minute, 0);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn bar_volume_is_the_delta_of_cumulative_volume() {
        let mut s = CandleSeries::new("k", 100);
        // First reading establishes the baseline; it must contribute 0, not 1000.
        s.push_tick(100.0, Some(1000.0), 0);
        s.push_tick(101.0, Some(1250.0), 10_000);
        assert_eq!(s.working().unwrap().volume, 250.0, "baseline must not be counted as volume");

        let closed = s.push_tick(102.0, Some(1400.0), M).unwrap();
        assert_eq!(closed.volume, 250.0);
        assert_eq!(s.working().unwrap().volume, 150.0, "new bar takes the next delta");
    }

    #[test]
    fn cumulative_volume_reset_does_not_produce_negative_or_giant_volume() {
        let mut s = CandleSeries::new("k", 100);
        s.push_tick(100.0, Some(900_000.0), 0);
        // Session rollover: counter restarts far below the previous reading.
        s.push_tick(100.0, Some(500.0), 10_000);
        assert_eq!(s.working().unwrap().volume, 0.0, "a counter reset must clamp to zero");
        // And the new baseline is adopted, so the next delta is sane.
        s.push_tick(100.0, Some(800.0), 20_000);
        assert_eq!(s.working().unwrap().volume, 300.0);
    }

    #[test]
    fn out_of_order_tick_cannot_rewrite_a_closed_bar() {
        let mut s = CandleSeries::new("k", 100);
        s.push_tick(100.0, None, M);           // opens minute 1
        s.push_tick(200.0, None, 0);           // stale frame from minute 0
        let w = s.working().unwrap();
        assert_eq!(w.minute, 1);
        assert_eq!(w.high, 100.0, "stale tick must not extend the current bar");
    }

    #[test]
    fn capacity_evicts_oldest_bars() {
        let mut s = CandleSeries::new("k", 3);
        for i in 0..10 {
            s.push_tick(100.0 + i as f64, None, i * M);
        }
        assert_eq!(s.len(), 3);
        assert_eq!(s.closed().front().unwrap().minute, 6);
    }

    #[test]
    fn invalid_prices_are_ignored() {
        let mut s = CandleSeries::new("k", 10);
        s.push_tick(0.0, None, 0);
        s.push_tick(-5.0, None, 0);
        s.push_tick(f64::NAN, None, 0);
        assert!(s.working().is_none(), "no bar should be opened from junk prices");
    }

    #[test]
    fn true_range_uses_previous_close() {
        let c = Candle { minute: 0, open: 100.0, high: 110.0, low: 105.0, close: 108.0, volume: 0.0, ticks: 1 };
        // Gap up: |low - prev_close| = 15 exceeds the 5-point bar range.
        assert_eq!(c.true_range(Some(90.0)), 20.0);
        assert_eq!(c.true_range(None), 5.0);
    }

    #[test]
    fn seeded_bars_respect_capacity_and_stay_closed() {
        let mut s = CandleSeries::new("k", 2);
        s.seed((0..5).map(|i| Candle {
            minute: i, open: 1.0, high: 1.0, low: 1.0, close: i as f64, volume: 0.0, ticks: 1,
        }));
        assert_eq!(s.len(), 2);
        assert_eq!(s.closed().front().unwrap().minute, 3);
        assert!(s.working().is_none(), "reloaded bars must not resume as a working bar");
    }
}
