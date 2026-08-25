//! Strategy engine API — state, agent debate logs and configuration.

use axum::{extract::{Query, State}, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use trading_engine::StrategyConfig;

use crate::AppState;

/// `GET /api/strategy` — full engine state: regime, agent views, debate scores,
/// strike selection, risk state and warm-up progress.
pub async fn strategy_state_handler(State(st): State<AppState>) -> Json<Value> {
    let trading_cfg = st.trading_cfg.read().await.clone();
    let snap = st.strategy.snapshot(&trading_cfg).await;
    Json(serde_json::to_value(snap).unwrap_or_else(|_| json!({})))
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    limit: Option<i64>,
}

/// `GET /api/strategy/decisions?limit=50` — the decision audit log, newest first.
pub async fn strategy_decisions_handler(
    State(st): State<AppState>,
    Query(q): Query<HistoryQuery>,
) -> Json<Value> {
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    Json(json!(crate::db::load_recent_decisions(&st.db_pool, limit).await))
}

/// `GET /api/strategy/config` — the current strategy configuration.
pub async fn get_strategy_config_handler(State(st): State<AppState>) -> Json<Value> {
    let cfg = st.strategy.engine.read().await.cfg.clone();
    Json(serde_json::to_value(cfg).unwrap_or_else(|_| json!({})))
}

/// `POST /api/strategy/config` — replace the strategy configuration.
///
/// Validation runs before anything is stored or applied, so a bad threshold is
/// rejected at the edge rather than silently producing a strategy that risks
/// more than intended.
pub async fn post_strategy_config_handler(
    State(st): State<AppState>,
    Json(cfg): Json<StrategyConfig>,
) -> Result<Json<Value>, (StatusCode, String)> {
    cfg.validate().map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    crate::db::save_strategy_config(&st.db_pool, &cfg)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let was_enabled = {
        let mut e = st.strategy.engine.write().await;
        let prev = e.cfg.enabled;
        e.cfg = cfg.clone();
        prev
    };

    if cfg.enabled != was_enabled {
        let mode = st.trading_cfg.read().await.mode.clone();
        tracing::warn!(enabled = cfg.enabled, %mode, "strategy engine toggled");
    }

    Ok(Json(json!({ "ok": true, "enabled": cfg.enabled })))
}

/// `POST /api/strategy/halt` — stop the engine taking new entries for the
/// session. A manual kill switch that survives until the next session rollover.
pub async fn post_strategy_halt_handler(State(st): State<AppState>) -> Json<Value> {
    let mut e = st.strategy.engine.write().await;
    e.risk.halted_reason = Some("halted manually from the dashboard".into());
    Json(json!({ "ok": true, "halted": true }))
}

/// `POST /api/strategy/resume` — clear a manual halt.
///
/// Deliberately clears **only** the halt flag. The realised-loss and
/// consecutive-loss counters stay as they are, so resuming cannot be used to
/// reset a breaker that tripped on real losses — those clear only at the next
/// session.
pub async fn post_strategy_resume_handler(State(st): State<AppState>) -> Json<Value> {
    let mut e = st.strategy.engine.write().await;
    e.risk.halted_reason = None;
    Json(json!({
        "ok": true,
        "halted": false,
        "realised_pnl": e.risk.realised_pnl,
        "consecutive_losses": e.risk.consecutive_losses,
    }))
}
