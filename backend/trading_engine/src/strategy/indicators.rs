//! Technical indicator math.
//!
//! Every function here is pure and total: it takes a slice, returns `Option`,
//! and returns `None` rather than a fabricated number whenever the input is too
//! short to define the indicator. That matters more than usual here — a
//! half-warmed EMA returned as a real value would be read by the agents as a
//! genuine trend signal on the first minute of the session.
//!
//! Formulas follow Wilder / standard definitions:
//!
//! - **EMA**   `α = 2/(n+1)`, `EMAₜ = α·Pₜ + (1−α)·EMAₜ₋₁`, seeded with the SMA
//!   of the first `n` samples.
//! - **ATR**   `TR = max(H−L, |H−C₋₁|, |L−C₋₁|)`, Wilder-smoothed
//!   `ATRₜ = (ATRₜ₋₁·(n−1) + TRₜ)/n`.
//! - **RSI**   Wilder-smoothed average gain/loss, `RSI = 100 − 100/(1+RS)`.
//! - **VWAP**  `Σ(typical·vol)/Σ(vol)` with `typical = (H+L+C)/3`; bands are
//!   `VWAP ± k·σ` where `σ` is the volume-weighted standard deviation of the
//!   typical price.

use super::candles::Candle;

/// Simple moving average of the last `n` samples.
pub fn sma(values: &[f64], n: usize) -> Option<f64> {
    if n == 0 || values.len() < n {
        return None;
    }
    let slice = &values[values.len() - n..];
    Some(slice.iter().sum::<f64>() / n as f64)
}

/// Exponential moving average over the whole series, seeded with the SMA of the
/// first `n` samples.
///
/// `None` until at least `n` samples exist.
pub fn ema(values: &[f64], n: usize) -> Option<f64> {
    if n == 0 || values.len() < n {
        return None;
    }
    let alpha = 2.0 / (n as f64 + 1.0);
    let mut acc = values[..n].iter().sum::<f64>() / n as f64;
    for &v in &values[n..] {
        acc = alpha * v + (1.0 - alpha) * acc;
    }
    Some(acc)
}

/// Wilder's Average True Range over `n` bars.
///
/// Needs `n + 1` bars: the first bar only supplies the previous close.
pub fn atr(candles: &[Candle], n: usize) -> Option<f64> {
    if n == 0 || candles.len() < n + 1 {
        return None;
    }
    let trs: Vec<f64> = candles
        .windows(2)
        .map(|w| w[1].true_range(Some(w[0].close)))
        .collect();
    // Seed with the simple mean of the first n true ranges, then Wilder-smooth.
    let mut acc = trs[..n].iter().sum::<f64>() / n as f64;
    for &tr in &trs[n..] {
        acc = (acc * (n as f64 - 1.0) + tr) / n as f64;
    }
    Some(acc)
}

/// Wilder's RSI over `n` periods, in `[0, 100]`.
pub fn rsi(values: &[f64], n: usize) -> Option<f64> {
    if n == 0 || values.len() < n + 1 {
        return None;
    }
    let deltas: Vec<f64> = values.windows(2).map(|w| w[1] - w[0]).collect();
    let (mut avg_gain, mut avg_loss) = (0.0, 0.0);
    for d in &deltas[..n] {
        if *d > 0.0 { avg_gain += d } else { avg_loss -= d }
    }
    avg_gain /= n as f64;
    avg_loss /= n as f64;
    for d in &deltas[n..] {
        let (g, l) = if *d > 0.0 { (*d, 0.0) } else { (0.0, -*d) };
        avg_gain = (avg_gain * (n as f64 - 1.0) + g) / n as f64;
        avg_loss = (avg_loss * (n as f64 - 1.0) + l) / n as f64;
    }
    // A run with no losses is RSI 100 by definition; guard the divide.
    if avg_loss <= f64::EPSILON {
        return Some(if avg_gain > 0.0 { 100.0 } else { 50.0 });
    }
    let rs = avg_gain / avg_loss;
    Some(100.0 - 100.0 / (1.0 + rs))
}

/// Volume-weighted average price and its ±k·σ bands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vwap {
    pub vwap: f64,
    pub upper: f64,
    pub lower: f64,
    /// Volume-weighted standard deviation of the typical price.
    pub sigma: f64,
}

/// Session VWAP with standard-deviation bands at `k` sigma.
///
/// Falls back to an *unweighted* mean of typical prices when the feed carries
/// no volume at all (index feeds report none). That keeps VWAP meaningful as a
/// mean-reversion reference for indices instead of returning `None`.
pub fn vwap_bands(candles: &[Candle], k: f64) -> Option<Vwap> {
    if candles.is_empty() {
        return None;
    }
    let total_vol: f64 = candles.iter().map(|c| c.volume).sum();
    let use_volume = total_vol > 0.0;

    let weight = |c: &Candle| if use_volume { c.volume } else { 1.0 };
    let wsum: f64 = candles.iter().map(weight).sum();
    if wsum <= 0.0 {
        return None;
    }

    let vwap = candles.iter().map(|c| c.typical() * weight(c)).sum::<f64>() / wsum;
    let var = candles
        .iter()
        .map(|c| {
            let d = c.typical() - vwap;
            d * d * weight(c)
        })
        .sum::<f64>()
        / wsum;
    let sigma = var.max(0.0).sqrt();

    Some(Vwap { vwap, upper: vwap + k * sigma, lower: vwap - k * sigma, sigma })
}

/// An EMA ribbon: fast/medium/slow, all computed on the same closes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ribbon {
    pub fast: f64,
    pub medium: f64,
    pub slow: f64,
}

impl Ribbon {
    /// `+1` fully bullish stack (fast > medium > slow), `-1` fully bearish,
    /// `0` interleaved/undecided.
    pub fn direction(&self) -> i8 {
        if self.fast > self.medium && self.medium > self.slow {
            1
        } else if self.fast < self.medium && self.medium < self.slow {
            -1
        } else {
            0
        }
    }

    /// Ribbon spread as a fraction of the slow EMA — how *separated* the stack
    /// is, which distinguishes a real trend from a flat tangle.
    pub fn spread_pct(&self) -> f64 {
        if self.slow.abs() <= f64::EPSILON {
            return 0.0;
        }
        (self.fast - self.slow) / self.slow * 100.0
    }
}

pub fn ribbon(closes: &[f64], fast: usize, medium: usize, slow: usize) -> Option<Ribbon> {
    Some(Ribbon {
        fast: ema(closes, fast)?,
        medium: ema(closes, medium)?,
        slow: ema(closes, slow)?,
    })
}

/// Population standard deviation of the last `n` samples.
pub fn stdev(values: &[f64], n: usize) -> Option<f64> {
    if n < 2 || values.len() < n {
        return None;
    }
    let slice = &values[values.len() - n..];
    let mean = slice.iter().sum::<f64>() / n as f64;
    let var = slice.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
    Some(var.sqrt())
}

/// Where `value` sits within `history`, as a percentile in `[0, 100]`.
///
/// Used to ask "is current ATR unusually high *for this instrument today*"
/// without hard-coding absolute volatility thresholds that differ wildly
/// between NIFTY and BANKNIFTY.
pub fn percentile_rank(history: &[f64], value: f64) -> Option<f64> {
    if history.is_empty() {
        return None;
    }
    let below = history.iter().filter(|h| **h < value).count() as f64;
    Some(below / history.len() as f64 * 100.0)
}

/// Linear-regression slope of `values` against sample index, normalised to
/// percent-of-mean per bar so it is comparable across instruments.
pub fn slope_pct(values: &[f64], n: usize) -> Option<f64> {
    if n < 2 || values.len() < n {
        return None;
    }
    let slice = &values[values.len() - n..];
    let nf = n as f64;
    let mean_x = (nf - 1.0) / 2.0;
    let mean_y = slice.iter().sum::<f64>() / nf;
    if mean_y.abs() <= f64::EPSILON {
        return None;
    }
    let mut num = 0.0;
    let mut den = 0.0;
    for (i, y) in slice.iter().enumerate() {
        let dx = i as f64 - mean_x;
        num += dx * (y - mean_y);
        den += dx * dx;
    }
    if den <= f64::EPSILON {
        return None;
    }
    Some((num / den) / mean_y * 100.0)
}

/// Opening-range high/low over the first `bars` bars of the session.
pub fn opening_range(candles: &[Candle], bars: usize) -> Option<(f64, f64)> {
    if bars == 0 || candles.len() < bars {
        return None;
    }
    let slice = &candles[..bars];
    let hi = slice.iter().fold(f64::MIN, |a, c| a.max(c.high));
    let lo = slice.iter().fold(f64::MAX, |a, c| a.min(c.low));
    (hi > lo || (hi - lo).abs() < f64::EPSILON).then_some((hi, lo))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(h: f64, l: f64, c: f64, v: f64) -> Candle {
        Candle { minute: 0, open: c, high: h, low: l, close: c, volume: v, ticks: 1 }
    }

    #[test]
    fn indicators_are_none_before_warm_up() {
        let short = [1.0, 2.0, 3.0];
        assert_eq!(ema(&short, 10), None, "EMA must not fabricate a value while cold");
        assert_eq!(rsi(&short, 14), None);
        assert_eq!(sma(&short, 10), None);
        assert_eq!(atr(&[bar(1.0, 0.5, 1.0, 0.0)], 14), None);
        assert_eq!(stdev(&short, 10), None);
    }

    #[test]
    fn ema_of_a_constant_series_is_that_constant() {
        let v = vec![50.0; 40];
        let e = ema(&v, 9).expect("warm");
        assert!((e - 50.0).abs() < 1e-9, "got {e}");
    }

    #[test]
    fn ema_tracks_faster_than_sma_on_a_ramp() {
        let v: Vec<f64> = (1..=40).map(|i| i as f64).collect();
        let e = ema(&v, 9).unwrap();
        let s = sma(&v, 9).unwrap();
        assert!(e > s, "EMA {e} should lead SMA {s} on a rising series");
        assert!(e < 40.0, "EMA must stay below the newest value");
    }

    #[test]
    fn rsi_saturates_on_a_pure_uptrend_and_bottoms_on_a_downtrend() {
        let up: Vec<f64> = (1..=30).map(|i| i as f64).collect();
        assert_eq!(rsi(&up, 14), Some(100.0));

        let down: Vec<f64> = (1..=30).rev().map(|i| i as f64).collect();
        let r = rsi(&down, 14).unwrap();
        assert!(r < 1.0, "pure downtrend should pin RSI near 0, got {r}");
    }

    #[test]
    fn rsi_of_a_flat_series_is_neutral() {
        let flat = vec![100.0; 30];
        assert_eq!(rsi(&flat, 14), Some(50.0), "no movement is neither overbought nor oversold");
    }

    #[test]
    fn atr_of_constant_range_bars_equals_that_range() {
        // Every bar spans exactly 10 points and closes mid-range, with no gaps.
        let bars: Vec<Candle> = (0..30)
            .map(|i| Candle { minute: i, open: 100.0, high: 105.0, low: 95.0, close: 100.0, volume: 0.0, ticks: 1 })
            .collect();
        let a = atr(&bars, 14).expect("warm");
        assert!((a - 10.0).abs() < 1e-9, "expected ATR 10, got {a}");
    }

    #[test]
    fn atr_counts_a_gap_larger_than_the_bar_range() {
        let mut bars = vec![Candle { minute: 0, open: 100.0, high: 100.0, low: 100.0, close: 100.0, volume: 0.0, ticks: 1 }];
        // Gap far above the prior close; true range must exceed the 2-pt body.
        bars.push(Candle { minute: 1, open: 150.0, high: 151.0, low: 149.0, close: 150.0, volume: 0.0, ticks: 1 });
        let tr = bars[1].true_range(Some(bars[0].close));
        assert_eq!(tr, 51.0, "true range must span the gap, not just the bar");
    }

    #[test]
    fn vwap_weights_toward_the_heavy_volume_bar() {
        let bars = vec![bar(100.0, 100.0, 100.0, 10.0), bar(200.0, 200.0, 200.0, 990.0)];
        let v = vwap_bands(&bars, 2.0).unwrap();
        assert!(v.vwap > 198.0, "VWAP {} should sit near the heavy bar", v.vwap);
        assert!(v.upper > v.vwap && v.lower < v.vwap);
    }

    #[test]
    fn vwap_falls_back_to_unweighted_mean_when_feed_has_no_volume() {
        // Index feeds report zero volume; VWAP must still be usable.
        let bars = vec![bar(100.0, 100.0, 100.0, 0.0), bar(200.0, 200.0, 200.0, 0.0)];
        let v = vwap_bands(&bars, 1.0).expect("must not be None just because volume is absent");
        assert!((v.vwap - 150.0).abs() < 1e-9, "got {}", v.vwap);
    }

    #[test]
    fn ribbon_direction_detects_stacking() {
        assert_eq!(Ribbon { fast: 3.0, medium: 2.0, slow: 1.0 }.direction(), 1);
        assert_eq!(Ribbon { fast: 1.0, medium: 2.0, slow: 3.0 }.direction(), -1);
        assert_eq!(Ribbon { fast: 2.0, medium: 1.0, slow: 3.0 }.direction(), 0, "interleaved is undecided");
    }

    #[test]
    fn slope_is_positive_on_a_ramp_and_zero_on_a_flat() {
        let ramp: Vec<f64> = (1..=20).map(|i| 100.0 + i as f64).collect();
        assert!(slope_pct(&ramp, 10).unwrap() > 0.0);

        let flat = vec![100.0; 20];
        assert!(slope_pct(&flat, 10).unwrap().abs() < 1e-9);
    }

    #[test]
    fn percentile_rank_places_value_in_history() {
        let h: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        let p = percentile_rank(&h, 50.5).unwrap();
        assert!((p - 50.0).abs() < 1.0, "got {p}");
        assert_eq!(percentile_rank(&h, 0.0), Some(0.0));
        assert_eq!(percentile_rank(&h, 1000.0), Some(100.0));
    }

    #[test]
    fn opening_range_spans_the_first_bars_only() {
        let bars = vec![
            bar(110.0, 90.0, 100.0, 1.0),
            bar(120.0, 95.0, 115.0, 1.0),
            bar(500.0, 5.0, 300.0, 1.0), // outside the range window
        ];
        let (hi, lo) = opening_range(&bars, 2).unwrap();
        assert_eq!((hi, lo), (120.0, 90.0));
    }
}
