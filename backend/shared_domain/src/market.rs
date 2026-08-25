//! Full market-data tick model.
//!
//! The Kotak HSM feed carries far more than the last traded price — open
//! interest, best bid/ask, average traded price, session OHLC and cumulative
//! volume all arrive on the same stream (see `kotak-api-docs/neo-websocket.md`
//! § "WebSocket Response Field Mapping"). The original engine only ever needed
//! `ltp`, so everything else was dropped at the parse site.
//!
//! The strategy engine needs the rest: order-flow pressure, liquidity gates and
//! PCR are all derived from these fields. This module models a **full** tick and
//! the store that holds one per subscribed scrip.
//!
//! # Why fields are `Option`
//!
//! HSM is an *incremental* feed. After the first snapshot for a scrip, later
//! frames carry only what actually changed — a quote update may contain `ltp`
//! and `ltt` and nothing else. Overwriting a stored tick wholesale with such a
//! frame would erase open interest and the bid/ask, and a strategy reading that
//! zeroed state would see a fake liquidity collapse. So every field is optional
//! and [`MarketTick::merge_from`] only overwrites what the incoming frame
//! actually carried.

use std::sync::Arc;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

/// One instrument's latest known market state, accumulated across incremental
/// feed frames.
///
/// A field is `None` only when the feed has never sent it for this scrip.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketTick {
    /// Feed key, `"<exchange_segment>|<token>"` (e.g. `"nse_fo|51386"`).
    pub key: String,
    /// Trading symbol (`ts`), when the feed supplies it.
    pub trading_symbol: Option<String>,
    /// Last traded price (`ltp`).
    pub ltp: Option<f64>,
    /// Last traded quantity (`ltq`).
    pub last_traded_qty: Option<f64>,
    /// Best bid price (`bp`).
    pub bid: Option<f64>,
    /// Best ask price (`sp`).
    pub ask: Option<f64>,
    /// Best bid quantity (`bq`).
    pub bid_qty: Option<f64>,
    /// Best ask quantity (`bs`).
    pub ask_qty: Option<f64>,
    /// Total buy quantity across the book (`tbq`).
    pub total_buy_qty: Option<f64>,
    /// Total sell quantity across the book (`tsq`).
    pub total_sell_qty: Option<f64>,
    /// Session open (`op`).
    pub open: Option<f64>,
    /// Session high (`h`).
    pub high: Option<f64>,
    /// Session low (`lo`).
    pub low: Option<f64>,
    /// Previous close (`c`).
    pub prev_close: Option<f64>,
    /// Average traded price (`ap`) — the exchange's own session VWAP.
    pub avg_traded_price: Option<f64>,
    /// Cumulative traded volume (`v`), when present.
    pub volume: Option<f64>,
    /// Turnover (`to`).
    pub turnover: Option<f64>,
    /// Open interest (`oi`).
    pub open_interest: Option<f64>,
    /// Exchange feed timestamp (`ltt` / `fdtm`), as supplied.
    pub feed_time: Option<String>,
    /// Local monotonic-ish wall clock (epoch millis) of the last merge, used to
    /// decide staleness without trusting exchange clocks.
    pub updated_at_ms: i64,
}

/// Parse a feed value that may arrive as a JSON number *or* a numeric string.
///
/// HSM is inconsistent about this between frame types, and a silent `as_f64()`
/// on a string field would read as "absent" and quietly degrade the strategy.
fn num(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => {
            let t = s.trim();
            if t.is_empty() { None } else { t.parse::<f64>().ok() }
        }
        _ => None,
    }
}

fn text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

impl MarketTick {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into(), ..Default::default() }
    }

    /// Overlay one incremental feed frame onto this tick.
    ///
    /// Only keys actually present in `frame` are written; absent keys leave the
    /// existing value intact. Values that fail to parse are treated as absent
    /// rather than as zero.
    pub fn merge_from(&mut self, frame: &serde_json::Value, now_ms: i64) {
        macro_rules! set_num {
            ($field:ident, $($k:literal),+) => {
                $( if let Some(v) = frame.get($k).and_then(num) { self.$field = Some(v); } )+
            };
        }

        set_num!(ltp, "ltp");
        set_num!(last_traded_qty, "ltq");
        set_num!(bid, "bp");
        set_num!(ask, "sp");
        set_num!(bid_qty, "bq");
        set_num!(ask_qty, "bs");
        set_num!(total_buy_qty, "tbq");
        set_num!(total_sell_qty, "tsq");
        set_num!(open, "op");
        set_num!(high, "h");
        set_num!(low, "lo");
        set_num!(prev_close, "c");
        set_num!(avg_traded_price, "ap");
        // Cumulative volume appears as `v` on scrip feeds; some frames use `vol`.
        set_num!(volume, "v", "vol");
        set_num!(turnover, "to");
        set_num!(open_interest, "oi");

        if let Some(ts) = frame.get("ts").and_then(text) {
            self.trading_symbol = Some(ts);
        }
        if let Some(t) = frame.get("ltt").and_then(text).or_else(|| frame.get("fdtm").and_then(text)) {
            self.feed_time = Some(t);
        }

        self.updated_at_ms = now_ms;
    }

    /// Best bid/ask spread as a fraction of the mid price.
    ///
    /// `None` when either side is missing or non-positive — an absent quote is
    /// never treated as a tight spread.
    pub fn spread_pct(&self) -> Option<f64> {
        let (b, a) = (self.bid?, self.ask?);
        if b <= 0.0 || a <= 0.0 || a < b {
            return None;
        }
        let mid = (a + b) / 2.0;
        if mid <= 0.0 {
            return None;
        }
        Some((a - b) / mid * 100.0)
    }

    /// Mid price, falling back to LTP when only one side is quoted.
    pub fn mid(&self) -> Option<f64> {
        match (self.bid, self.ask) {
            (Some(b), Some(a)) if b > 0.0 && a > 0.0 && a >= b => Some((a + b) / 2.0),
            _ => self.ltp.filter(|v| *v > 0.0),
        }
    }

    /// Book imbalance in `[-1.0, 1.0]`: positive means resting buy interest
    /// outweighs sell interest.
    pub fn book_imbalance(&self) -> Option<f64> {
        let (b, s) = (self.total_buy_qty?, self.total_sell_qty?);
        let total = b + s;
        if total <= 0.0 {
            return None;
        }
        Some((b - s) / total)
    }

    /// True when the last merge is older than `max_age_ms`.
    ///
    /// Callers treat a stale tick as no data at all: the engine must not act on
    /// a price the exchange stopped confirming.
    pub fn is_stale(&self, now_ms: i64, max_age_ms: i64) -> bool {
        now_ms.saturating_sub(self.updated_at_ms) > max_age_ms
    }
}

/// Concurrent map of feed key → latest accumulated [`MarketTick`].
///
/// Shared with the same `Arc<DashMap>` pattern the LTP map already uses, so the
/// websocket task can write while the strategy loop reads without a lock.
pub type TickStore = Arc<DashMap<String, MarketTick>>;

pub fn new_tick_store() -> TickStore {
    Arc::new(DashMap::new())
}

/// Apply one feed frame to the store, creating the entry if this is the first
/// frame seen for that key.
pub fn apply_frame(store: &TickStore, key: &str, frame: &serde_json::Value, now_ms: i64) {
    store
        .entry(key.to_string())
        .or_insert_with(|| MarketTick::new(key))
        .merge_from(frame, now_ms);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn incremental_frames_do_not_erase_known_fields() {
        // First frame: a full snapshot.
        let mut tick = MarketTick::new("nse_fo|51386");
        tick.merge_from(
            &json!({"ltp": "120.5", "oi": "45000", "bp": "120.4", "sp": "120.6", "v": "88000"}),
            1_000,
        );
        assert_eq!(tick.open_interest, Some(45_000.0));
        assert_eq!(tick.ltp, Some(120.5));

        // Second frame: a price-only delta, as HSM actually sends.
        tick.merge_from(&json!({"ltp": "121.0"}), 2_000);

        assert_eq!(tick.ltp, Some(121.0), "price must update");
        assert_eq!(
            tick.open_interest,
            Some(45_000.0),
            "OI must survive a delta frame that omits it"
        );
        assert_eq!(tick.bid, Some(120.4), "bid must survive a delta frame");
        assert_eq!(tick.updated_at_ms, 2_000);
    }

    #[test]
    fn numeric_strings_and_numbers_both_parse() {
        let mut a = MarketTick::new("k");
        a.merge_from(&json!({"ltp": "99.25"}), 0);
        let mut b = MarketTick::new("k");
        b.merge_from(&json!({"ltp": 99.25}), 0);
        assert_eq!(a.ltp, b.ltp);
    }

    #[test]
    fn unparseable_values_are_absent_not_zero() {
        let mut t = MarketTick::new("k");
        t.merge_from(&json!({"ltp": "", "oi": "n/a"}), 0);
        assert_eq!(t.ltp, None, "empty string must not become 0.0");
        assert_eq!(t.open_interest, None, "garbage must not become 0.0");
    }

    #[test]
    fn spread_rejects_crossed_or_missing_quotes() {
        let mut t = MarketTick::new("k");
        t.merge_from(&json!({"ltp": 100.0}), 0);
        assert_eq!(t.spread_pct(), None, "no quote is not a tight spread");

        t.merge_from(&json!({"bp": 101.0, "sp": 99.0}), 0);
        assert_eq!(t.spread_pct(), None, "crossed book must not yield a spread");

        t.merge_from(&json!({"bp": 99.5, "sp": 100.5}), 0);
        let s = t.spread_pct().expect("valid quote");
        assert!((s - 1.0).abs() < 1e-9, "expected 1% spread, got {s}");
    }

    #[test]
    fn staleness_is_measured_against_local_clock() {
        let mut t = MarketTick::new("k");
        t.merge_from(&json!({"ltp": 1.0}), 10_000);
        assert!(!t.is_stale(12_000, 5_000));
        assert!(t.is_stale(16_000, 5_000));
    }
}
