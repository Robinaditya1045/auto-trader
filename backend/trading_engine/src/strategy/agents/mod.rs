//! Multi-agent reasoning layer, adapted from the TradingAgents paradigm.
//!
//! Each analyst examines one facet of the market in isolation and returns a
//! stance with a confidence and its supporting evidence. The
//! [`debate::DebateCoordinator`] then plays the bull case against the bear case
//! and only produces a decision when one side wins *decisively*. Finally the
//! [`risk::RiskManager`] holds unconditional veto authority over whatever the
//! debate concluded.
//!
//! The separation matters for auditability as much as for accuracy: when the
//! engine takes a trade, the dashboard can show which agent argued for it and
//! on what evidence, and when it declines, exactly which check said no.

pub mod debate;
pub mod orderflow;
pub mod risk;
pub mod technical;

use serde::{Deserialize, Serialize};

pub use debate::{DebateCoordinator, DebateOutcome};
pub use orderflow::OrderFlowAnalyst;
pub use risk::{RiskDecision, RiskManager, RiskState};
pub use technical::TechnicalAnalyst;

/// Which way an agent leans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stance {
    Bullish,
    Bearish,
    /// No opinion — counts toward neither side of the debate.
    Neutral,
}

impl Stance {
    /// `+1` bullish, `-1` bearish, `0` neutral.
    pub fn direction(&self) -> i8 {
        match self {
            Self::Bullish => 1,
            Self::Bearish => -1,
            Self::Neutral => 0,
        }
    }

    pub fn from_direction(d: i8) -> Self {
        match d {
            1.. => Self::Bullish,
            0 => Self::Neutral,
            _ => Self::Bearish,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bullish => "BULLISH",
            Self::Bearish => "BEARISH",
            Self::Neutral => "NEUTRAL",
        }
    }
}

/// One agent's contribution to the debate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentView {
    /// Agent name, e.g. `"TechnicalAnalyst"`.
    pub agent: String,
    pub stance: Stance,
    /// How strongly this agent holds its view, 0–100.
    pub confidence: f64,
    /// This agent's say in the debate relative to the others.
    pub weight: f64,
    /// Human-readable support for the stance, shown in the debate log.
    pub evidence: Vec<String>,
}

impl AgentView {
    pub fn neutral(agent: &str, weight: f64, why: impl Into<String>) -> Self {
        Self {
            agent: agent.to_string(),
            stance: Stance::Neutral,
            confidence: 0.0,
            weight,
            evidence: vec![why.into()],
        }
    }

    /// Confidence clamped into `[0, 100]`, so a miscomputed score can never
    /// give one agent outsized pull in the debate.
    pub fn clamped_confidence(&self) -> f64 {
        self.confidence.clamp(0.0, 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stance_direction_round_trips() {
        for s in [Stance::Bullish, Stance::Bearish, Stance::Neutral] {
            assert_eq!(Stance::from_direction(s.direction()), s);
        }
    }

    #[test]
    fn confidence_is_clamped_into_range() {
        let v = AgentView { agent: "X".into(), stance: Stance::Bullish, confidence: 900.0, weight: 1.0, evidence: vec![] };
        assert_eq!(v.clamped_confidence(), 100.0, "a runaway score must not dominate the debate");

        let n = AgentView { agent: "X".into(), stance: Stance::Bearish, confidence: -50.0, weight: 1.0, evidence: vec![] };
        assert_eq!(n.clamped_confidence(), 0.0);
    }

    #[test]
    fn neutral_view_carries_its_reason() {
        let v = AgentView::neutral("TechnicalAnalyst", 1.0, "not warm yet");
        assert_eq!(v.stance, Stance::Neutral);
        assert_eq!(v.confidence, 0.0);
        assert_eq!(v.evidence, vec!["not warm yet".to_string()]);
    }
}
