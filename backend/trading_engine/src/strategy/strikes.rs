//! Intelligent strike selection — liquidity first.
//!
//! # Why not Greeks
//!
//! The specification asks for a delta filter (~0.40–0.55). Kotak publishes no
//! Greeks and no implied volatility, so a true delta would have to come from
//! inverting Black-Scholes on the option's own last traded price. That inversion
//! is least reliable exactly when this engine is most active — near expiry,
//! where `T → 0` makes vega vanish and the IV solve becomes numerically
//! unstable, and on any contract whose last print is stale.
//!
//! A wrong delta is not a neutral error: it silently picks the wrong strike and
//! sizes real risk against it. So moneyness is approximated structurally — by
//! distance from spot in strike steps, where ATM-to-slightly-OTM is the
//! 0.40–0.55 delta region for an index option — and the *decisive* filters are
//! ones measured directly from the live feed rather than modelled:
//!
//! - **Open interest** — is anyone actually positioned here?
//! - **Bid/ask spread** — what does a round trip cost before the trade even
//!   starts working?
//! - **Premium band** — a ₹5 option is a lottery ticket that decays to zero; a
//!   ₹900 option spends the whole per-trade budget on one lot.
//! - **Quote freshness** — a contract whose feed has gone quiet is not tradeable
//!   at any price.
//!
//! Every one of those is observed, not modelled. A contract that fails any of
//! them is rejected with a recorded reason, so the dashboard can show why a
//! setup produced no trade.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use shared_domain::{MarketTick, TickStore};

use super::config::{IndexSpec, StrategyConfig};
use crate::scrip_master::ScripRecord;

/// A contract considered for selection, with everything measured about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrikeCandidate {
    pub trading_symbol: String,
    pub instrument_token: String,
    pub exchange_segment: String,
    /// Strike, normalised out of the scrip master's scaling.
    pub strike: f64,
    pub option_type: String,
    pub expiry: NaiveDate,
    pub lot_size: i32,
    pub tick_size: f64,
    /// Feed key for this contract.
    pub ws_key: String,
    /// Steps from at-the-money; positive = out-of-the-money.
    pub otm_steps: i32,
    pub premium: Option<f64>,
    pub open_interest: Option<f64>,
    pub spread_pct: Option<f64>,
    /// `None` when accepted; otherwise why it was rejected.
    pub rejected: Option<String>,
    /// Lower is better. Only meaningful for accepted candidates.
    pub score: f64,
}

/// Outcome of a selection pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrikeSelection {
    pub chosen: Option<StrikeCandidate>,
    /// Everything considered, accepted and rejected, for the dashboard.
    pub considered: Vec<StrikeCandidate>,
    pub reason: String,
}

/// Kotak's scrip master stores strike prices at inconsistent scales across
/// segments — some rows are the true strike, others are ×100.
///
/// Guessing per-row is fragile, so the scale is resolved **once for the whole
/// chain**: whichever divisor lands the chain's median strike closest to spot
/// wins. A chain is internally consistent, so one factor is correct for all of
/// its rows, and using the median makes the choice immune to a few outlier
/// strikes at the far wings.
fn resolve_strike_scale(records: &[&ScripRecord], spot: f64) -> f64 {
    if records.is_empty() || spot <= 0.0 {
        return 1.0;
    }
    let mut strikes: Vec<f64> = records.iter().map(|r| r.strike_price).filter(|s| *s > 0.0).collect();
    if strikes.is_empty() {
        return 1.0;
    }
    strikes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = strikes[strikes.len() / 2];

    [1.0_f64, 100.0, 1000.0]
        .into_iter()
        .min_by(|a, b| {
            let da = (median / a - spot).abs();
            let db = (median / b - spot).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(1.0)
}

/// Pick the expiry to trade: the nearest one not already past.
///
/// Returns `None` when the only available expiry is today *and* the expiry-day
/// cutoff has passed — at that point a long option is pure decay into
/// settlement, so having no contract to trade is the correct answer.
pub fn select_expiry(
    records: &[&ScripRecord],
    today: NaiveDate,
    now_hm: (u32, u32),
    cfg: &StrategyConfig,
) -> Option<NaiveDate> {
    let mut expiries: Vec<NaiveDate> = records.iter().map(|r| r.expiry_date).filter(|d| *d >= today).collect();
    expiries.sort();
    expiries.dedup();

    for exp in expiries {
        let dte = (exp - today).num_days();
        if exp == today {
            let (h, m) = now_hm;
            let past_cutoff = h > cfg.expiry_day_no_entry_hour
                || (h == cfg.expiry_day_no_entry_hour && m >= cfg.expiry_day_no_entry_minute);
            if past_cutoff {
                continue; // too late in the expiry session to open a long
            }
        }
        if dte < cfg.min_days_to_expiry {
            continue;
        }
        return Some(exp);
    }
    None
}

/// Select the best contract for a directional view.
///
/// `direction` is `+1` for a bullish view (buy CE) or `-1` for bearish (buy PE);
/// no other value trades. Options are only ever **bought** — this function has
/// no path that returns a contract to sell.
#[allow(clippy::too_many_arguments)]
pub fn select_strike(
    records: &[&ScripRecord],
    spot: f64,
    direction: i8,
    index: &IndexSpec,
    ticks: &TickStore,
    cfg: &StrategyConfig,
    today: NaiveDate,
    now_hm: (u32, u32),
    now_ms: i64,
) -> StrikeSelection {
    let none = |reason: String| StrikeSelection { chosen: None, considered: Vec::new(), reason };

    let option_type = match direction {
        1 => "CE",
        -1 => "PE",
        _ => return none("no directional view — nothing to select".into()),
    };
    if spot <= 0.0 {
        return none("spot price unavailable".into());
    }

    let Some(expiry) = select_expiry(records, today, now_hm, cfg) else {
        return none("no tradeable expiry (expiry-day cutoff passed or none listed)".into());
    };

    let scale = resolve_strike_scale(records, spot);
    let step = if index.strike_step > 0.0 { index.strike_step } else { 50.0 };

    // ATM = the listed strike nearest spot, on the ladder.
    let atm = (spot / step).round() * step;

    let mut considered: Vec<StrikeCandidate> = Vec::new();

    for rec in records {
        if rec.option_type != option_type || rec.expiry_date != expiry {
            continue;
        }
        let strike = rec.strike_price / scale;
        if strike <= 0.0 {
            continue;
        }

        // Signed distance from ATM in steps, expressed as out-of-the-money-ness
        // for the side being bought: a CE is OTM above spot, a PE below.
        let steps_from_atm = ((strike - atm) / step).round() as i32;
        let otm_steps = if option_type == "CE" { steps_from_atm } else { -steps_from_atm };

        if otm_steps < -cfg.strike_search_steps || otm_steps > cfg.strike_search_steps {
            continue; // outside the search window entirely — not worth reporting
        }

        let ws_key = format!("{}|{}", rec.exchange_segment_code, rec.instrument_token);
        let tick: Option<MarketTick> = ticks.get(&ws_key).map(|t| t.clone());

        let premium = tick.as_ref().and_then(|t| t.mid());
        let open_interest = tick.as_ref().and_then(|t| t.open_interest);
        let spread_pct = tick.as_ref().and_then(|t| t.spread_pct());

        let mut cand = StrikeCandidate {
            trading_symbol: rec.trading_symbol.clone(),
            instrument_token: rec.instrument_token.clone(),
            exchange_segment: rec.exchange_segment_code.clone(),
            strike,
            option_type: rec.option_type.clone(),
            expiry: rec.expiry_date,
            lot_size: rec.lot_size.max(1) as i32,
            tick_size: if rec.tick_size > 0.0 { rec.tick_size } else { 0.05 },
            ws_key,
            otm_steps,
            premium,
            open_interest,
            spread_pct,
            rejected: None,
            score: f64::MAX,
        };

        // ── Liquidity gates. Every one errs toward rejection on missing data:
        //    an unquoted contract is untradeable, not neutrally scored. ──
        cand.rejected = if tick.is_none() {
            Some("no live quote — contract not subscribed or never traded".into())
        } else if tick.as_ref().is_some_and(|t| t.is_stale(now_ms, cfg.max_tick_age_ms)) {
            Some("quote is stale".into())
        } else {
            match (premium, open_interest, spread_pct) {
                (None, _, _) => Some("no premium quoted".into()),
                (Some(p), _, _) if p < cfg.min_premium => {
                    Some(format!("premium ₹{p:.2} below ₹{:.2} floor — decays to zero too easily", cfg.min_premium))
                }
                (Some(p), _, _) if p > cfg.max_premium => {
                    Some(format!("premium ₹{p:.2} above ₹{:.2} cap — one lot would eat the budget", cfg.max_premium))
                }
                (_, None, _) => Some("open interest not reported".into()),
                (_, Some(oi), _) if oi < cfg.min_open_interest => {
                    Some(format!("open interest {oi:.0} below {:.0} floor — too thin to exit cleanly", cfg.min_open_interest))
                }
                (_, _, None) => Some("no two-sided quote — cannot measure spread".into()),
                (_, _, Some(s)) if s > cfg.max_spread_pct => {
                    Some(format!("spread {s:.2}% exceeds {:.2}% cap — round trip costs too much", cfg.max_spread_pct))
                }
                _ => None,
            }
        };

        if cand.rejected.is_none() {
            // Score: distance from the preferred moneyness dominates, with
            // spread as the tie-breaker so the cheaper round trip wins between
            // two equally-positioned strikes.
            let moneyness_err = (cand.otm_steps - cfg.preferred_otm_steps).abs() as f64;
            cand.score = moneyness_err * 10.0 + spread_pct.unwrap_or(0.0);
        }

        considered.push(cand);
    }

    if considered.is_empty() {
        return StrikeSelection {
            chosen: None,
            considered,
            reason: format!("no {option_type} contracts listed near ATM {atm:.0} for expiry {expiry}"),
        };
    }

    let best = considered
        .iter()
        .filter(|c| c.rejected.is_none())
        .min_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
        .cloned();

    let reason = match &best {
        Some(c) => format!(
            "{} {:.0} {} — {} steps OTM, premium ₹{:.2}, OI {:.0}, spread {:.2}%",
            index.symbol, c.strike, c.option_type, c.otm_steps,
            c.premium.unwrap_or(0.0), c.open_interest.unwrap_or(0.0), c.spread_pct.unwrap_or(0.0),
        ),
        None => {
            let n = considered.len();
            let sample = considered.iter().filter_map(|c| c.rejected.clone()).next().unwrap_or_default();
            format!("all {n} candidate strikes failed the liquidity gates (e.g. {sample})")
        }
    };

    StrikeSelection { chosen: best, considered, reason }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashmap::DashMap;
    use serde_json::json;
    use std::sync::Arc;

    fn rec(strike: f64, opt: &str, exp: NaiveDate, token: &str) -> ScripRecord {
        ScripRecord {
            instrument_token: token.to_string(),
            trading_symbol: format!("NIFTY{}{}", strike as i64, opt),
            symbol_name: "NIFTY".to_string(),
            exchange_segment_code: "nse_fo".to_string(),
            strike_price: strike,
            option_type: opt.to_string(),
            expiry_date: exp,
            lot_size: 75,
            tick_size: 0.05,
        }
    }

    fn spec() -> IndexSpec {
        IndexSpec { symbol: "NIFTY".into(), spot_key: "nse_cm|Nifty 50".into(), strike_step: 50.0 }
    }

    /// A chain around 24000 spot, 9 strikes each side, all CE and PE.
    fn chain(exp: NaiveDate, scale: f64) -> Vec<ScripRecord> {
        let mut v = Vec::new();
        for i in -9..=9 {
            let k = 24_000.0 + i as f64 * 50.0;
            v.push(rec(k * scale, "CE", exp, &format!("CE{i}")));
            v.push(rec(k * scale, "PE", exp, &format!("PE{i}")));
        }
        v
    }

    /// Quote every contract as liquid unless overridden.
    fn liquid_ticks(recs: &[ScripRecord]) -> TickStore {
        let store: TickStore = Arc::new(DashMap::new());
        for r in recs {
            let key = format!("{}|{}", r.exchange_segment_code, r.instrument_token);
            let mut t = MarketTick::new(&key);
            t.merge_from(&json!({"ltp": 120.0, "bp": 119.5, "sp": 120.5, "oi": 200000.0}), 1_000);
            store.insert(key, t);
        }
        store
    }

    fn today() -> NaiveDate { NaiveDate::from_ymd_opt(2026, 8, 25).unwrap() }
    fn thursday() -> NaiveDate { NaiveDate::from_ymd_opt(2026, 8, 27).unwrap() }

    #[test]
    fn picks_preferred_otm_call_for_a_bullish_view() {
        let recs = chain(thursday(), 1.0);
        let refs: Vec<&ScripRecord> = recs.iter().collect();
        let ticks = liquid_ticks(&recs);
        let cfg = StrategyConfig::default();

        let sel = select_strike(&refs, 24_000.0, 1, &spec(), &ticks, &cfg, today(), (10, 0), 1_000);
        let c = sel.chosen.expect("a liquid chain must yield a strike");
        assert_eq!(c.option_type, "CE", "a bullish view buys calls, never puts");
        assert_eq!(c.otm_steps, cfg.preferred_otm_steps);
        assert_eq!(c.strike, 24_050.0, "1 step OTM above 24000 spot");
    }

    #[test]
    fn bearish_view_buys_a_put_below_spot() {
        let recs = chain(thursday(), 1.0);
        let refs: Vec<&ScripRecord> = recs.iter().collect();
        let ticks = liquid_ticks(&recs);
        let cfg = StrategyConfig::default();

        let c = select_strike(&refs, 24_000.0, -1, &spec(), &ticks, &cfg, today(), (10, 0), 1_000)
            .chosen.expect("chain is liquid");
        assert_eq!(c.option_type, "PE");
        assert_eq!(c.strike, 23_950.0, "1 step OTM below spot for a put");
        assert_eq!(c.otm_steps, 1);
    }

    #[test]
    fn hundred_times_scaled_strikes_are_normalised() {
        // Kotak stores some segments' strikes ×100.
        let recs = chain(thursday(), 100.0);
        let refs: Vec<&ScripRecord> = recs.iter().collect();
        let ticks = liquid_ticks(&recs);

        let c = select_strike(&refs, 24_000.0, 1, &spec(), &ticks, &StrategyConfig::default(), today(), (10, 0), 1_000)
            .chosen.expect("scaling must not prevent selection");
        assert!(
            (c.strike - 24_050.0).abs() < 1e-6,
            "expected a normalised 24050 strike, got {}", c.strike
        );
    }

    #[test]
    fn a_wide_spread_contract_is_rejected_with_a_reason() {
        let recs = chain(thursday(), 1.0);
        let refs: Vec<&ScripRecord> = recs.iter().collect();
        let ticks = liquid_ticks(&recs);
        // Blow out the spread on the otherwise-preferred strike (1 step OTM CE).
        let key = "nse_fo|CE1";
        let mut t = MarketTick::new(key);
        t.merge_from(&json!({"ltp": 120.0, "bp": 100.0, "sp": 140.0, "oi": 200000.0}), 1_000);
        ticks.insert(key.to_string(), t);

        let sel = select_strike(&refs, 24_000.0, 1, &spec(), &ticks, &StrategyConfig::default(), today(), (10, 0), 1_000);
        let c = sel.chosen.expect("other strikes remain tradeable");
        assert_ne!(c.instrument_token, "CE1", "the wide-spread strike must not be chosen");

        let wide = sel.considered.iter().find(|c| c.instrument_token == "CE1").unwrap();
        assert!(wide.rejected.as_ref().unwrap().contains("spread"), "got {:?}", wide.rejected);
    }

    #[test]
    fn illiquid_chain_yields_no_trade_not_a_bad_trade() {
        let recs = chain(thursday(), 1.0);
        let refs: Vec<&ScripRecord> = recs.iter().collect();
        let ticks: TickStore = Arc::new(DashMap::new());
        for r in &recs {
            let key = format!("{}|{}", r.exchange_segment_code, r.instrument_token);
            let mut t = MarketTick::new(&key);
            // Quoted, but nobody is positioned there.
            t.merge_from(&json!({"ltp": 120.0, "bp": 119.5, "sp": 120.5, "oi": 10.0}), 1_000);
            ticks.insert(key, t);
        }
        let sel = select_strike(&refs, 24_000.0, 1, &spec(), &ticks, &StrategyConfig::default(), today(), (10, 0), 1_000);
        assert!(sel.chosen.is_none(), "thin OI must produce no trade");
        assert!(sel.reason.contains("liquidity gates"), "got {}", sel.reason);
    }

    #[test]
    fn stale_quotes_are_never_tradeable() {
        let recs = chain(thursday(), 1.0);
        let refs: Vec<&ScripRecord> = recs.iter().collect();
        let ticks = liquid_ticks(&recs);   // all stamped at t=1000
        let cfg = StrategyConfig::default();

        // Evaluate far in the future: every quote is now stale.
        let sel = select_strike(&refs, 24_000.0, 1, &spec(), &ticks, &cfg, today(), (10, 0), 1_000 + cfg.max_tick_age_ms + 1);
        assert!(sel.chosen.is_none(), "a frozen feed must not produce a trade");
        assert!(
            sel.considered.iter().all(|c| c.rejected.is_some()),
            "every candidate should be rejected as stale"
        );
    }

    #[test]
    fn premium_outside_the_band_is_rejected() {
        let recs = chain(thursday(), 1.0);
        let refs: Vec<&ScripRecord> = recs.iter().collect();
        let ticks: TickStore = Arc::new(DashMap::new());
        for r in &recs {
            let key = format!("{}|{}", r.exchange_segment_code, r.instrument_token);
            let mut t = MarketTick::new(&key);
            // ₹2 lottery tickets: liquid, but pure decay.
            t.merge_from(&json!({"ltp": 2.0, "bp": 1.95, "sp": 2.05, "oi": 500000.0}), 1_000);
            ticks.insert(key, t);
        }
        let sel = select_strike(&refs, 24_000.0, 1, &spec(), &ticks, &StrategyConfig::default(), today(), (10, 0), 1_000);
        assert!(sel.chosen.is_none());
        assert!(
            sel.considered.iter().any(|c| c.rejected.as_ref().is_some_and(|r| r.contains("below"))),
            "cheap premium should be rejected explicitly"
        );
    }

    #[test]
    fn no_direction_selects_nothing() {
        let recs = chain(thursday(), 1.0);
        let refs: Vec<&ScripRecord> = recs.iter().collect();
        let sel = select_strike(&refs, 24_000.0, 0, &spec(), &liquid_ticks(&recs), &StrategyConfig::default(), today(), (10, 0), 1_000);
        assert!(sel.chosen.is_none(), "a neutral view must not produce a contract");
    }

    #[test]
    fn expiry_day_cutoff_skips_today_and_rolls_forward() {
        let recs = [chain(today(), 1.0), chain(thursday(), 1.0)].concat();
        let refs: Vec<&ScripRecord> = recs.iter().collect();
        let cfg = StrategyConfig::default();

        // Before the cutoff, today's expiry is fair game.
        let before = select_expiry(&refs, today(), (10, 0), &cfg);
        assert_eq!(before, Some(today()));

        // After it, the engine rolls to the next expiry rather than buying decay.
        let after = select_expiry(&refs, today(), (14, 30), &cfg);
        assert_eq!(after, Some(thursday()), "past the cutoff we must roll forward, not trade today's expiry");
    }

    #[test]
    fn no_expiry_at_all_when_only_todays_is_listed_and_cutoff_passed() {
        let recs = chain(today(), 1.0);
        let refs: Vec<&ScripRecord> = recs.iter().collect();
        assert_eq!(
            select_expiry(&refs, today(), (14, 30), &StrategyConfig::default()),
            None,
            "no contract is the right answer, not a decaying one"
        );
    }
}
