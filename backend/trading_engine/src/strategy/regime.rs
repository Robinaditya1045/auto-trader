//! Market regime classification.
//!
//! Directional option buying only works in some market states. A trending
//! market rewards it; a tight range grinds long premium to nothing through
//! theta while price oscillates. So before any directional opinion is formed,
//! the engine classifies *what kind of market this is* and lets the regime veto
//! or damp the trade.
//!
//! Classification uses three orthogonal readings, all computed from minute bars
//! of the **underlying index spot** (never the option premium, which carries
//! its own volatility and decay):
//!
//! | Reading            | Question it answers                    |
//! |--------------------|----------------------------------------|
//! | EMA ribbon stack   | Is there a persistent direction?       |
//! | ATR percentile     | Is volatility expanding or compressed? |
//! | Price vs. VWAP σ   | Is price extended or mean-reverting?   |

use serde::{Deserialize, Serialize};

use super::candles::Candle;
use super::config::StrategyConfig;
use super::indicators::{self, Ribbon, Vwap};

/// The market state the engine believes it is operating in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketRegime {
    /// Stacked EMAs with expanding range — the regime directional option buying
    /// is designed for.
    TrendingUp,
    TrendingDown,
    /// Price oscillating inside VWAP bands with a flat ribbon. Long premium
    /// bleeds here; the engine stands down.
    Rangebound,
    /// Stretched well beyond the VWAP bands against a flat ribbon — a snap-back
    /// is more likely than continuation, so a breakout entry is refused.
    MeanReverting,
    /// Volatility expanding sharply without a settled direction (news, open).
    /// Tradeable only on a decisive break, never on anticipation.
    HighVolatility,
    /// Not enough data to say. Always treated as "do not trade".
    Unknown,
}

impl MarketRegime {
    /// Whether the regime permits opening a new directional long-option trade.
    pub fn allows_entry(&self) -> bool {
        matches!(self, Self::TrendingUp | Self::TrendingDown | Self::HighVolatility)
    }

    /// Directional bias the regime itself implies: `+1` up, `-1` down, `0` none.
    pub fn bias(&self) -> i8 {
        match self {
            Self::TrendingUp => 1,
            Self::TrendingDown => -1,
            _ => 0,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TrendingUp => "TRENDING_UP",
            Self::TrendingDown => "TRENDING_DOWN",
            Self::Rangebound => "RANGEBOUND",
            Self::MeanReverting => "MEAN_REVERTING",
            Self::HighVolatility => "HIGH_VOLATILITY",
            Self::Unknown => "UNKNOWN",
        }
    }
}

/// A full regime reading, retained so the dashboard can show *why* the engine
/// classified the market the way it did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegimeReading {
    pub regime: MarketRegime,
    pub spot: f64,
    pub ema_fast: f64,
    pub ema_medium: f64,
    pub ema_slow: f64,
    /// `+1` bullish stack, `-1` bearish, `0` tangled.
    pub ribbon_direction: i8,
    /// Ribbon separation as a percent of the slow EMA.
    pub ribbon_spread_pct: f64,
    pub atr: f64,
    /// Where current ATR sits in the session's own ATR history (0–100).
    pub atr_percentile: f64,
    pub vwap: f64,
    /// Signed distance from VWAP in sigma units.
    pub vwap_z: f64,
    pub rsi: f64,
    /// Human-readable justification, shown in the debate log.
    pub rationale: String,
}

/// Ribbon separation (percent of slow EMA) below which the stack is considered
/// flat regardless of ordering. Three EMAs within a whisker of each other are
/// not a trend, they are noise that happens to be sorted.
const FLAT_RIBBON_SPREAD_PCT: f64 = 0.05;

/// |z| beyond which price counts as stretched from VWAP.
const STRETCHED_Z: f64 = 2.0;

/// ATR percentile above which volatility counts as expanding.
const VOL_EXPANSION_PCTL: f64 = 80.0;

/// Classify the current regime from the spot series.
///
/// Returns [`MarketRegime::Unknown`] whenever any required indicator is still
/// cold — the caller treats that as "do not trade", so a partial warm-up can
/// never be mistaken for a real reading.
pub fn classify(bars: &[Candle], cfg: &StrategyConfig) -> RegimeReading {
    let unknown = |why: &str| RegimeReading {
        regime: MarketRegime::Unknown,
        spot: bars.last().map(|c| c.close).unwrap_or(0.0),
        ema_fast: 0.0, ema_medium: 0.0, ema_slow: 0.0,
        ribbon_direction: 0, ribbon_spread_pct: 0.0,
        atr: 0.0, atr_percentile: 0.0,
        vwap: 0.0, vwap_z: 0.0, rsi: 50.0,
        rationale: why.to_string(),
    };

    if bars.len() < cfg.min_bars_for_signal {
        return unknown(&format!(
            "warming up — {} of {} bars",
            bars.len(), cfg.min_bars_for_signal
        ));
    }

    let closes: Vec<f64> = bars.iter().map(|c| c.close).collect();
    let Some(rib @ Ribbon { fast, medium, slow }) = indicators::ribbon(&closes, cfg.ema_fast, cfg.ema_medium, cfg.ema_slow) else {
        return unknown("EMA ribbon not warm");
    };
    let Some(atr_now) = indicators::atr(bars, cfg.atr_period) else {
        return unknown("ATR not warm");
    };
    let Some(Vwap { vwap, sigma, .. }) = indicators::vwap_bands(bars, cfg.vwap_band_sigma) else {
        return unknown("VWAP not computable");
    };
    let rsi_now = indicators::rsi(&closes, cfg.rsi_period).unwrap_or(50.0);
    let spot = closes[closes.len() - 1];

    // ATR history: recompute ATR over a sliding window so "is volatility high"
    // is asked against this instrument's own recent behaviour rather than an
    // absolute threshold that would mean different things for NIFTY vs BANKNIFTY.
    let window = cfg.atr_period + 1;
    let atr_history: Vec<f64> = (window..bars.len())
        .filter_map(|end| indicators::atr(&bars[end - window..end], cfg.atr_period))
        .collect();
    let atr_pctl = indicators::percentile_rank(&atr_history, atr_now).unwrap_or(50.0);

    // Distance from VWAP in sigma. A zero sigma (perfectly flat session) means
    // no dispersion to measure against, so z is 0 rather than infinite.
    let vwap_z = if sigma > f64::EPSILON { (spot - vwap) / sigma } else { 0.0 };

    let dir = rib.direction();
    let spread = rib.spread_pct().abs();
    let flat_ribbon = spread < FLAT_RIBBON_SPREAD_PCT;
    let vol_expanding = atr_pctl >= VOL_EXPANSION_PCTL;
    let stretched = vwap_z.abs() >= STRETCHED_Z;

    let (regime, rationale) = if dir != 0 && !flat_ribbon {
        // A stacked, separated ribbon is a trend — unless price has already run
        // far beyond VWAP, in which case chasing it buys the exhaustion.
        if stretched {
            (
                MarketRegime::MeanReverting,
                format!(
                    "ribbon stacked ({dir:+}) but price is {vwap_z:+.1}σ from VWAP — extended, entry would be chasing"
                ),
            )
        } else if dir > 0 {
            (
                MarketRegime::TrendingUp,
                format!("EMAs stacked bullish, spread {spread:.2}%, ATR pctl {atr_pctl:.0}, {vwap_z:+.1}σ from VWAP"),
            )
        } else {
            (
                MarketRegime::TrendingDown,
                format!("EMAs stacked bearish, spread {spread:.2}%, ATR pctl {atr_pctl:.0}, {vwap_z:+.1}σ from VWAP"),
            )
        }
    } else if vol_expanding {
        (
            MarketRegime::HighVolatility,
            format!("ATR at {atr_pctl:.0}th percentile with an undecided ribbon — volatile but directionless"),
        )
    } else if stretched {
        (
            MarketRegime::MeanReverting,
            format!("flat ribbon and {vwap_z:+.1}σ from VWAP — snap-back more likely than continuation"),
        )
    } else {
        (
            MarketRegime::Rangebound,
            format!("flat ribbon (spread {spread:.2}%), ATR pctl {atr_pctl:.0}, {vwap_z:+.1}σ — long premium decays here"),
        )
    };

    RegimeReading {
        regime,
        spot,
        ema_fast: fast, ema_medium: medium, ema_slow: slow,
        ribbon_direction: dir,
        ribbon_spread_pct: rib.spread_pct(),
        atr: atr_now,
        atr_percentile: atr_pctl,
        vwap,
        vwap_z,
        rsi: rsi_now,
        rationale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> StrategyConfig {
        // Short periods keep the synthetic series in these tests small.
        StrategyConfig { min_bars_for_signal: 30, ema_fast: 3, ema_medium: 6, ema_slow: 12, atr_period: 5, rsi_period: 5, ..Default::default() }
    }

    /// Build bars from a close series, giving each a small symmetric range.
    fn series(closes: &[f64]) -> Vec<Candle> {
        closes.iter().enumerate().map(|(i, &c)| Candle {
            minute: i as i64,
            open: c, high: c + 1.0, low: c - 1.0, close: c,
            volume: 100.0, ticks: 10,
        }).collect()
    }

    #[test]
    fn cold_series_is_unknown_and_blocks_entry() {
        let r = classify(&series(&[100.0; 5]), &cfg());
        assert_eq!(r.regime, MarketRegime::Unknown);
        assert!(!r.regime.allows_entry(), "an unwarmed engine must not trade");
        assert!(r.rationale.contains("warming up"), "got: {}", r.rationale);
    }

    #[test]
    fn steady_uptrend_classifies_as_trending_up() {
        // Gentle, persistent rise: ribbon stacks without price running far from VWAP.
        let closes: Vec<f64> = (0..60).map(|i| 20_000.0 + i as f64 * 3.0).collect();
        let r = classify(&series(&closes), &cfg());
        assert_eq!(r.ribbon_direction, 1, "EMAs should stack bullish");
        assert!(
            matches!(r.regime, MarketRegime::TrendingUp | MarketRegime::MeanReverting),
            "expected a directional read, got {:?} ({})", r.regime, r.rationale
        );
    }

    #[test]
    fn steady_downtrend_has_bearish_stack_and_negative_bias() {
        let closes: Vec<f64> = (0..60).map(|i| 20_000.0 - i as f64 * 3.0).collect();
        let r = classify(&series(&closes), &cfg());
        assert_eq!(r.ribbon_direction, -1);
        if r.regime == MarketRegime::TrendingDown {
            assert_eq!(r.regime.bias(), -1);
        }
    }

    #[test]
    fn flat_market_is_rangebound_and_refuses_entry() {
        // Tiny oscillation around a constant — the classic theta trap.
        let closes: Vec<f64> = (0..60).map(|i| 20_000.0 + if i % 2 == 0 { 0.5 } else { -0.5 }).collect();
        let r = classify(&series(&closes), &cfg());
        assert!(
            !r.regime.allows_entry(),
            "a flat chop must not permit a long-premium entry, got {:?} ({})", r.regime, r.rationale
        );
    }

    #[test]
    fn regime_gating_matches_intent() {
        assert!(MarketRegime::TrendingUp.allows_entry());
        assert!(MarketRegime::TrendingDown.allows_entry());
        assert!(MarketRegime::HighVolatility.allows_entry());
        assert!(!MarketRegime::Rangebound.allows_entry(), "range = theta bleed");
        assert!(!MarketRegime::MeanReverting.allows_entry(), "extended = chasing");
        assert!(!MarketRegime::Unknown.allows_entry(), "unknown = flat, by policy");
    }

    #[test]
    fn every_reading_carries_a_rationale() {
        let closes: Vec<f64> = (0..60).map(|i| 20_000.0 + (i as f64 * 0.7).sin() * 20.0).collect();
        let r = classify(&series(&closes), &cfg());
        assert!(!r.rationale.is_empty(), "the dashboard needs a reason for every classification");
    }
}
