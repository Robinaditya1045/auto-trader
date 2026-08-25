//! Trading engine — stateful OMS with fee calculator.
//!
//! # Modules
//! - [`fees`]     — `FeeCalculator`, `ChargeBreakdown`
//! - [`monitor`]  — `start_position_monitor` (50 ms state machine)
//! - [`strategy`] — autonomous multi-agent F&O signal generation (PAPER only)

pub mod fees;
pub mod monitor;
pub mod scrip_master;
pub mod strategy;

pub use fees::{ChargeBreakdown, FeeCalculator};
pub use monitor::{start_position_monitor, preview_reconciliation, apply_reconciliation};
pub use scrip_master::{ScripStore, ScripRecord};
pub use strategy::{StrategyConfig, StrategyEngine, StrategySnapshot};
