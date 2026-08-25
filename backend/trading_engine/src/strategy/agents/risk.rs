//! Risk manager — position sizing, circuit breakers and veto authority.
//!
//! This agent runs **last** and its refusal is final: no conviction score from
//! the debate can override it. Everything upstream decides *what* would be a
//! good trade; this decides whether the account can afford to take it at all.
//!
//! # Shared budget
//!
//! Position limits count positions from **every** source, not just this
//! engine's. The Telegram ingester and the strategy both feed the same account,
//! and limits that only counted algo positions would let the two silently stack
//! exposure on the same index while each believed it was within budget.
//!
//! # Circuit breakers
//!
//! Each is independent and any one halts new entries for the rest of the
//! session:
//!
//! - realised loss reaching the daily cap;
//! - a run of consecutive losing algo trades;
//! - the per-session entry count;
//! - the time-of-day cutoff;
//! - an already-open position on the same underlying.

use serde::{Deserialize, Serialize};
use shared_domain::{MonitoredPosition, TradeState};

use crate::strategy::config::StrategyConfig;

pub struct RiskManager;

/// Mutable risk state, carried across the session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RiskState {
    /// Realised P&L from algo trades today (negative = loss).
    pub realised_pnl: f64,
    /// Algo entries taken today.
    pub entries_today: usize,
    /// Consecutive losing algo trades.
    pub consecutive_losses: usize,
    /// Set once a breaker trips; cleared only by a new session.
    pub halted_reason: Option<String>,
    /// The session date this state belongs to, as `YYYY-MM-DD` IST.
    pub session_date: String,
}

impl RiskState {
    /// Reset the state when the session date rolls over.
    ///
    /// Without this, a process left running across midnight would carry
    /// yesterday's loss cap and halt state into a fresh session — or worse,
    /// carry a *stale* entry count that silently shrinks today's budget.
    pub fn roll_session(&mut self, today: &str) {
        if self.session_date != today {
            *self = RiskState { session_date: today.to_string(), ..Default::default() };
        }
    }

    /// Record a closed algo trade's realised P&L.
    pub fn record_close(&mut self, pnl: f64) {
        self.realised_pnl += pnl;
        if pnl < 0.0 {
            self.consecutive_losses += 1;
        } else {
            self.consecutive_losses = 0;
        }
    }
}

/// What the risk manager decided.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RiskDecision {
    /// Cleared to trade this many lots.
    Approved { lots: i32, rationale: String },
    /// Refused, with the reason to surface on the dashboard.
    Rejected { reason: String },
}

impl RiskDecision {
    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved { .. })
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::Approved { rationale, .. } => rationale,
            Self::Rejected { reason } => reason,
        }
    }
}

impl RiskManager {
    /// Evaluate a proposed entry.
    ///
    /// `open_positions` must include positions from *all* sources. `premium` is
    /// the option's current price per unit, `lot_size` the contract multiplier.
    pub fn evaluate(
        underlying: &str,
        premium: f64,
        lot_size: i32,
        open_positions: &[MonitoredPosition],
        state: &RiskState,
        cfg: &StrategyConfig,
        max_trade_amount_inr: f64,
        now_hm: (u32, u32),
    ) -> RiskDecision {
        let reject = |reason: String| RiskDecision::Rejected { reason };

        // ── Circuit breakers, cheapest and most decisive first ──────────── //
        if let Some(why) = &state.halted_reason {
            return reject(format!("engine halted for the session: {why}"));
        }

        let (h, m) = now_hm;
        if h > cfg.no_entry_hour || (h == cfg.no_entry_hour && m >= cfg.no_entry_minute) {
            return reject(format!(
                "past the {:02}:{:02} IST entry cutoff",
                cfg.no_entry_hour, cfg.no_entry_minute
            ));
        }

        if state.realised_pnl <= -cfg.max_daily_loss_inr {
            return reject(format!(
                "daily loss limit reached (realised ₹{:.0} against a ₹{:.0} cap)",
                state.realised_pnl, cfg.max_daily_loss_inr
            ));
        }

        if state.consecutive_losses >= cfg.max_consecutive_losses {
            return reject(format!(
                "{} consecutive losses — standing down for the session",
                state.consecutive_losses
            ));
        }

        if state.entries_today >= cfg.max_algo_entries_per_day {
            return reject(format!(
                "already took {} of {} permitted entries today",
                state.entries_today, cfg.max_algo_entries_per_day
            ));
        }

        // ── Exposure limits, counting every source ──────────────────────── //
        let live: Vec<&MonitoredPosition> = open_positions
            .iter()
            .filter(|p| !matches!(p.state, TradeState::Closed))
            .collect();

        if live.len() >= cfg.max_open_positions {
            return reject(format!(
                "{} positions already open across all sources (limit {})",
                live.len(), cfg.max_open_positions
            ));
        }

        if cfg.one_position_per_underlying {
            let clash = live.iter().any(|p| p.signal.instrument_name.eq_ignore_ascii_case(underlying));
            if clash {
                return reject(format!("already holding a {underlying} position"));
            }
        }

        // ── Sizing ──────────────────────────────────────────────────────── //
        if premium <= 0.0 || lot_size <= 0 {
            return reject("cannot size a position without a valid premium and lot size".into());
        }
        let lot_cost = premium * lot_size as f64;
        if lot_cost > max_trade_amount_inr {
            return reject(format!(
                "one lot costs ₹{lot_cost:.0}, above the ₹{max_trade_amount_inr:.0} per-trade cap"
            ));
        }
        let lots = (max_trade_amount_inr / lot_cost).floor() as i32;
        if lots < 1 {
            return reject("per-trade cap does not cover a single lot".into());
        }

        RiskDecision::Approved {
            lots,
            rationale: format!(
                "{lots} lot(s) × {lot_size} × ₹{premium:.2} = ₹{:.0}, within the ₹{max_trade_amount_inr:.0} cap; \
                 {} of {} positions open, {} of {} entries used",
                lots as f64 * lot_cost,
                live.len(), cfg.max_open_positions,
                state.entries_today, cfg.max_algo_entries_per_day,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_domain::TradeSignal;

    fn cfg() -> StrategyConfig {
        StrategyConfig::default()
    }

    fn state() -> RiskState {
        RiskState { session_date: "2026-08-25".into(), ..Default::default() }
    }

    fn position(instrument: &str, st: TradeState) -> MonitoredPosition {
        MonitoredPosition {
            id: format!("id-{instrument}"),
            signal: TradeSignal {
                instrument_name: instrument.into(),
                strike: Some(24_000.0), option_type: Some("CE".into()), expiry: None,
                action: "BUY".into(), entry_condition: "ABOVE".into(), entry_price: 100.0,
                targets: vec![130.0], stop_loss: 70.0, source: "test".into(),
                signal_id: None, raw_message: None, paper_only: true,
            },
            state: st,
            current_sl: 70.0, next_dynamic_target: None, manual_sell_qty: None,
            executed_qty: 75, avg_buy_price: 100.0, override_qty: None, resolved_order: None,
            ltp: None, ws_scrip_key: None, force_exit: None, override_exit_price: None,
            tick_size: 0.05, entry_order_id: None, sl_order_id: None, sl_order_qty: 0,
            sl_order_trigger: 0.0, target_order_id: None, pending_exit_order_id: None,
            pending_exit_qty: 0, pending_exit_reason: None, entry_cancel_sent: false,
            exit_attempts: 0, live_halt: None,
        }
    }

    /// Standard approval case: liquid ATM-ish option, nothing open, mid-session.
    fn approve_with(open: &[MonitoredPosition], st: &RiskState) -> RiskDecision {
        RiskManager::evaluate("NIFTY", 120.0, 75, open, st, &cfg(), 15_000.0, (11, 0))
    }

    #[test]
    fn approves_a_clean_setup_and_sizes_within_the_cap() {
        let d = approve_with(&[], &state());
        match d {
            RiskDecision::Approved { lots, .. } => {
                // 120 × 75 = ₹9,000 per lot; ₹15,000 cap allows exactly 1.
                assert_eq!(lots, 1);
            }
            RiskDecision::Rejected { reason } => panic!("should approve: {reason}"),
        }
    }

    #[test]
    fn daily_loss_limit_halts_new_entries() {
        let mut s = state();
        s.realised_pnl = -cfg().max_daily_loss_inr;
        let d = approve_with(&[], &s);
        assert!(!d.is_approved());
        assert!(d.reason().contains("daily loss limit"), "got {}", d.reason());
    }

    #[test]
    fn consecutive_losses_halt_new_entries() {
        let mut s = state();
        s.consecutive_losses = cfg().max_consecutive_losses;
        let d = approve_with(&[], &s);
        assert!(!d.is_approved());
        assert!(d.reason().contains("consecutive losses"));
    }

    #[test]
    fn position_limit_counts_every_source_not_just_algo_positions() {
        // These carry source "test" — the limit must still see them.
        let open: Vec<MonitoredPosition> = ["NIFTY", "BANKNIFTY", "FINNIFTY"]
            .iter().map(|i| position(i, TradeState::Active)).collect();
        let d = RiskManager::evaluate("SENSEX", 120.0, 75, &open, &state(), &cfg(), 15_000.0, (11, 0));
        assert!(!d.is_approved(), "Telegram positions must count toward the shared budget");
        assert!(d.reason().contains("across all sources"), "got {}", d.reason());
    }

    #[test]
    fn closed_positions_do_not_consume_the_budget() {
        let open: Vec<MonitoredPosition> = ["NIFTY", "BANKNIFTY", "FINNIFTY"]
            .iter().map(|i| position(i, TradeState::Closed)).collect();
        let d = RiskManager::evaluate("SENSEX", 120.0, 75, &open, &state(), &cfg(), 15_000.0, (11, 0));
        assert!(d.is_approved(), "closed positions hold no exposure: {}", d.reason());
    }

    #[test]
    fn refuses_a_second_position_on_the_same_underlying() {
        let open = vec![position("NIFTY", TradeState::Active)];
        let d = approve_with(&open, &state());
        assert!(!d.is_approved());
        assert!(d.reason().contains("already holding"));
    }

    #[test]
    fn same_underlying_check_is_case_insensitive() {
        let open = vec![position("nifty", TradeState::Active)];
        let d = approve_with(&open, &state());
        assert!(!d.is_approved(), "casing must not defeat the duplicate check");
    }

    #[test]
    fn time_cutoff_blocks_late_entries() {
        let c = cfg();
        let d = RiskManager::evaluate("NIFTY", 120.0, 75, &[], &state(), &c, 15_000.0, (c.no_entry_hour, c.no_entry_minute));
        assert!(!d.is_approved());
        assert!(d.reason().contains("entry cutoff"));
    }

    #[test]
    fn refuses_when_one_lot_exceeds_the_per_trade_cap() {
        // 900 × 75 = ₹67,500 against a ₹15,000 cap.
        let d = RiskManager::evaluate("NIFTY", 900.0, 75, &[], &state(), &cfg(), 15_000.0, (11, 0));
        assert!(!d.is_approved());
        assert!(d.reason().contains("above the"), "got {}", d.reason());
    }

    #[test]
    fn refuses_to_size_against_a_missing_premium() {
        let d = RiskManager::evaluate("NIFTY", 0.0, 75, &[], &state(), &cfg(), 15_000.0, (11, 0));
        assert!(!d.is_approved(), "no premium means no sizing, not a default size");
    }

    #[test]
    fn an_explicit_halt_overrides_everything() {
        let mut s = state();
        s.halted_reason = Some("manual kill switch".into());
        let d = approve_with(&[], &s);
        assert!(!d.is_approved());
        assert!(d.reason().contains("manual kill switch"));
    }

    #[test]
    fn session_rollover_clears_yesterdays_state() {
        let mut s = RiskState {
            session_date: "2026-08-24".into(),
            realised_pnl: -9_000.0,
            entries_today: 4,
            consecutive_losses: 3,
            halted_reason: Some("yesterday's halt".into()),
        };
        s.roll_session("2026-08-25");
        assert_eq!(s.realised_pnl, 0.0);
        assert_eq!(s.entries_today, 0);
        assert_eq!(s.consecutive_losses, 0);
        assert!(s.halted_reason.is_none(), "a new session must start clean");
        assert_eq!(s.session_date, "2026-08-25");
    }

    #[test]
    fn same_session_roll_is_a_no_op() {
        let mut s = state();
        s.realised_pnl = -1_200.0;
        s.entries_today = 2;
        s.roll_session("2026-08-25");
        assert_eq!(s.realised_pnl, -1_200.0, "same-day roll must not wipe live state");
        assert_eq!(s.entries_today, 2);
    }

    #[test]
    fn record_close_tracks_streaks_and_resets_on_a_win() {
        let mut s = state();
        s.record_close(-500.0);
        s.record_close(-300.0);
        assert_eq!(s.consecutive_losses, 2);
        assert_eq!(s.realised_pnl, -800.0);

        s.record_close(1_000.0);
        assert_eq!(s.consecutive_losses, 0, "a win breaks the streak");
        assert_eq!(s.realised_pnl, 200.0);
    }
}
