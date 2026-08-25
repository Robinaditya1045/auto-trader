//! Background task that drives the autonomous strategy engine.
//!
//! Responsibilities, in order each cycle:
//!
//! 1. fold the live tick store into minute bars and persist any bar that closed;
//! 2. keep the index spot feeds and the near option chain subscribed;
//! 3. evaluate each configured index through the agent pipeline;
//! 4. record the decision, and publish its signal **only** in PAPER mode.
//!
//! The task holds no lock across an await that touches the broker, and it never
//! calls the broker itself — it only reads the shared tick store and writes to
//! the same signal channel the Telegram ingester uses.

use std::sync::Arc;

use dashmap::DashMap;
use shared_domain::{DbWriteMessage, MonitoredPosition, TickStore, TradeSignal, TradingConfig};
use sqlx::SqlitePool;
use tokio::sync::{broadcast, mpsc, RwLock};
use trading_engine::strategy::{StrategyEngine, StrategySnapshot};
use trading_engine::ScripStore;

/// How often the pipeline runs. Minute bars are the decision unit, so
/// evaluating several times a minute is enough to react promptly without
/// re-deriving the same bar repeatedly.
const EVAL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// How often ticks are folded into bars. Faster than evaluation so a bar's
/// high/low reflect the real intra-minute path rather than 5-second samples.
const INGEST_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Days of minute bars to retain.
const CANDLE_RETENTION_DAYS: i64 = 30;

/// Shared handle so the API can read engine state and swap its config.
pub struct StrategyHandle {
    pub engine: Arc<RwLock<StrategyEngine>>,
}

impl StrategyHandle {
    pub async fn snapshot(&self, trading_cfg: &TradingConfig) -> StrategySnapshot {
        self.engine.read().await.snapshot(trading_cfg)
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    engine: Arc<RwLock<StrategyEngine>>,
    ticks: TickStore,
    prices: Arc<DashMap<String, f64>>,
    scrip_store: Arc<RwLock<Option<ScripStore>>>,
    positions: Arc<RwLock<Vec<MonitoredPosition>>>,
    trading_cfg: Arc<RwLock<TradingConfig>>,
    signal_tx: broadcast::Sender<TradeSignal>,
    ws_tx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedSender<String>>>>,
    db_tx: mpsc::Sender<DbWriteMessage>,
    pool: SqlitePool,
) {
    // Reload persisted bars so the engine starts warm after its first session.
    {
        let keys = crate::db::candle_keys(&pool).await;
        let mut e = engine.write().await;
        let cap = e.cfg.series_capacity as i64;
        let mut restored = 0usize;
        for key in keys {
            let bars = crate::db::load_candles(&pool, &key, cap).await;
            restored += bars.len();
            e.seed_series(&key, bars);
        }
        if restored > 0 {
            tracing::info!(bars = restored, "strategy: reloaded persisted minute bars");
        } else {
            tracing::info!("strategy: no persisted bars — the first session runs cold until warm-up completes");
        }
    }

    crate::db::prune_candles(&pool, CANDLE_RETENTION_DAYS).await;

    let mut ingest = tokio::time::interval(INGEST_INTERVAL);
    ingest.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut evaluate = tokio::time::interval(EVAL_INTERVAL);
    evaluate.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    tracing::info!("strategy task started");

    loop {
        tokio::select! {
            _ = ingest.tick() => {
                let now_ms = chrono::Utc::now().timestamp_millis();
                let closed = { engine.write().await.ingest_ticks(&ticks, now_ms) };
                for (key, bar) in closed {
                    crate::db::save_candle(&pool, &key, &bar).await;
                }
            }

            _ = evaluate.tick() => {
                // The engine is disabled by default; when off it costs one
                // config read per cycle and nothing else.
                let enabled = { engine.read().await.cfg.enabled };
                if !enabled {
                    continue;
                }
                if !shared_domain::is_market_open() {
                    continue;
                }

                { engine.write().await.roll_session(); }

                let cfg_now = { trading_cfg.read().await.clone() };
                let indices = { engine.read().await.cfg.indices.clone() };

                // Keep the spot feeds subscribed. Without this the strategy has
                // no underlying to analyse — the original engine only ever
                // subscribed the option contracts it already held.
                let spot_keys: Vec<String> = indices.iter().map(|i| i.spot_key.clone()).collect();
                subscribe(&engine, &ws_tx, &prices, &spot_keys).await;

                let store_guard = scrip_store.read().await;
                let Some(store) = store_guard.as_ref() else {
                    continue; // no scrip master yet — nothing can be resolved
                };

                let open_positions = { positions.read().await.clone() };
                let now_ms = chrono::Utc::now().timestamp_millis();

                for index in &indices {
                    // Subscribe the near chain so OI, spread and premium are
                    // actually observable for the strike gates.
                    let spot = ticks.get(&index.spot_key).and_then(|t| t.ltp).unwrap_or(0.0);
                    if spot > 0.0 {
                        let chain: Vec<String> = {
                            let e = engine.read().await;
                            e.chain_keys(index, store, spot).into_iter().map(|(k, _)| k).collect()
                        };
                        subscribe(&engine, &ws_tx, &prices, &chain).await;
                    }

                    let decision = {
                        let mut e = engine.write().await;
                        e.evaluate_once(index, &ticks, store, &open_positions, cfg_now.max_trade_amount_inr, now_ms)
                    };

                    // Publishing is gated inside the engine: PAPER mode only.
                    let published = {
                        let mut e = engine.write().await;
                        match e.publish(&decision, &cfg_now, &signal_tx) {
                            Ok(sent) => sent,
                            Err(reason) => {
                                // Only worth logging when a signal actually
                                // existed and was withheld — otherwise this is
                                // just the disabled/LIVE steady state.
                                if decision.signal.is_some() {
                                    let msg = format!(
                                        r#"{{"event":"ALGO_SIGNAL_WITHHELD","instrument":"{}","reason":"{}"}}"#,
                                        decision.underlying, reason.replace('"', "'")
                                    );
                                    let _ = db_tx.send(DbWriteMessage::Log { level: "WARN".into(), message: msg }).await;
                                }
                                false
                            }
                        }
                    };

                    // Only decisions that concluded something are worth an audit
                    // row; a "still warming up" every 5 seconds is noise.
                    if decision.signal.is_some() || decision.debate.actionable {
                        crate::db::save_decision(&pool, &decision, published).await;
                    }
                    if published {
                        let msg = format!(
                            r#"{{"event":"ALGO_SIGNAL","instrument":"{}","regime":"{}","stance":"{}","conviction":{:.0},"mode":"PAPER"}}"#,
                            decision.underlying,
                            decision.regime.regime.as_str(),
                            decision.debate.stance.as_str(),
                            decision.debate.conviction,
                        );
                        let _ = db_tx.send(DbWriteMessage::Log { level: "INFO".into(), message: msg }).await;
                    }

                    engine.write().await.record(decision);
                }
            }
        }
    }
}

/// Ask the bridge to subscribe feed keys that are not already subscribed.
///
/// Seeds a `0.0` placeholder in the LTP map the same way the position monitor
/// does, so a key is never absent while its first tick is in flight.
async fn subscribe(
    engine: &Arc<RwLock<StrategyEngine>>,
    ws_tx: &Arc<tokio::sync::Mutex<Option<mpsc::UnboundedSender<String>>>>,
    prices: &Arc<DashMap<String, f64>>,
    keys: &[String],
) {
    if keys.is_empty() {
        return;
    }
    let fresh = { engine.write().await.newly_subscribed(keys) };
    if fresh.is_empty() {
        return;
    }
    let guard = ws_tx.lock().await;
    let Some(tx) = guard.as_ref() else { return };
    for key in fresh {
        prices.entry(key.clone()).or_insert(0.0);
        let _ = tx.send(serde_json::json!({"action": "subscribe", "scrips": key}).to_string());
        tracing::info!(%key, "strategy: subscribed feed");
    }
}
