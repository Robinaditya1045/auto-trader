//! Autonomous F&O strategy engine — multi-agent signal generation.
//!
//! # Where this sits
//!
//! This module is a **signal producer only**. It never talks to the broker and
//! never places an order. Decisions leave here as [`shared_domain::TradeSignal`]
//! values published onto the same broadcast channel the Telegram ingester uses,
//! and the existing OMS in [`crate::monitor`] owns execution, stop management
//! and the "never sell more than we hold" invariant exactly as before.
//!
//! That split is deliberate: the order path is the part that has been proven
//! with real money, so an unproven strategy earns no new access to it.
//!
//! # Safety posture
//!
//! Two independent guards keep this engine off the live order path:
//!
//! 1. [`engine::StrategyEngine`] refuses to publish **any** signal unless
//!    `TradingConfig::mode` is `"PAPER"`. In LIVE mode it still runs the full
//!    pipeline and records what it *would* have done, but publishes nothing.
//! 2. Every signal it emits carries `paper_only: true`, and the live entry gate
//!    in `decide_live` abandons such a signal rather than buying it.
//!
//! Guard 1 alone is sufficient; guard 2 exists so that a future refactor which
//! accidentally weakens guard 1 still cannot put real money behind this code.
//!
//! # Pipeline
//!
//! ```text
//!   ticks ─► CandleSeries ─► indicators ─┬─► TechnicalAnalyst ─┐
//!                                        ├─► OrderFlowAnalyst ─┤
//!                                        └─► RegimeAnalyst ────┤
//!                                                              ▼
//!                                                    DebateCoordinator
//!                                                    (bull vs bear)
//!                                                              │ conviction ≥ threshold
//!                                                              ▼
//!                                                     StrikeSelector
//!                                                    (liquidity gates)
//!                                                              │
//!                                                              ▼
//!                                                      RiskManager
//!                                              (shared budget, veto authority)
//!                                                              │
//!                                                              ▼
//!                                                        TradeSignal
//! ```

pub mod agents;
pub mod candles;
pub mod config;
pub mod engine;
pub mod indicators;
pub mod regime;
pub mod strikes;

pub use candles::{Candle, CandleSeries};
pub use config::StrategyConfig;
pub use engine::{StrategyEngine, StrategySnapshot};
pub use regime::{MarketRegime, RegimeReading};
pub use strikes::{StrikeCandidate, StrikeSelection};
