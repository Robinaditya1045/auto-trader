use shared_domain::{current_ist_timestamp_string, DbWriteMessage, MonitoredPosition};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use shared_domain::TradingConfig;
use std::str::FromStr;
use tokio::sync::mpsc;

async fn ensure_column(pool: &SqlitePool, sql: &str) {
    if let Err(e) = sqlx::query(sql).execute(pool).await {
        let msg = e.to_string();
        if !msg.contains("duplicate column name") {
            panic!("failed schema migration: {msg}");
        }
    }
}

// ---------------------------------------------------------------------------
// Initialisation
// ---------------------------------------------------------------------------

/// Open (or create) the SQLite database, enable WAL, and create all tables.
pub async fn init_db(db_url: &str) -> SqlitePool {
    let opts = SqliteConnectOptions::from_str(db_url)
        .expect("invalid DATABASE_URL")
        .create_if_missing(true);
    let pool = SqlitePool::connect_with(opts).await.expect("cannot open SQLite");

    sqlx::query("PRAGMA journal_mode=WAL;").execute(&pool).await.unwrap();

    sqlx::query("CREATE TABLE IF NOT EXISTS wallet (id INTEGER PRIMARY KEY, balance REAL NOT NULL)")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT OR IGNORE INTO wallet (id, balance) VALUES (1, 1000000.0)")
        .execute(&pool).await.unwrap();

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS paper_trades (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ticker TEXT NOT NULL, action TEXT NOT NULL, qty INTEGER NOT NULL,
            executed_price REAL NOT NULL,
            gross_value REAL NOT NULL DEFAULT 0.0, brokerage REAL NOT NULL DEFAULT 0.0,
            stt_charge REAL NOT NULL DEFAULT 0.0, sebi_fee REAL NOT NULL DEFAULT 0.0,
            stamp_duty REAL NOT NULL DEFAULT 0.0, transaction_charge REAL NOT NULL DEFAULT 0.0,
            gst REAL NOT NULL DEFAULT 0.0, net_value REAL NOT NULL DEFAULT 0.0,
            timestamp DATETIME NOT NULL
        )",
    ).execute(&pool).await.unwrap();

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS system_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            level TEXT NOT NULL, message TEXT NOT NULL,
            timestamp DATETIME NOT NULL
        )",
    ).execute(&pool).await.unwrap();

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS trading_config (
            id INTEGER PRIMARY KEY DEFAULT 1 CHECK (id = 1),
            max_trade_amount_inr REAL NOT NULL DEFAULT 10000.0,
            index_lots INTEGER NOT NULL DEFAULT 1,
            other_lots INTEGER NOT NULL DEFAULT 1,
            mode TEXT NOT NULL DEFAULT 'PAPER',
            brokerage_per_order REAL NOT NULL DEFAULT 20.0,
            target_1_exit_pct REAL NOT NULL DEFAULT 50.0,
            target_2_exit_pct REAL NOT NULL DEFAULT 100.0,
            entry_market_protection REAL NOT NULL DEFAULT 5.0
        )",
    ).execute(&pool).await.unwrap();
    sqlx::query("INSERT OR IGNORE INTO trading_config (id) VALUES (1)")
        .execute(&pool).await.unwrap();
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN index_lots INTEGER NOT NULL DEFAULT 1",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN other_lots INTEGER NOT NULL DEFAULT 1",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN entry_market_protection REAL NOT NULL DEFAULT 5.0",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN dynamic_targeting INTEGER NOT NULL DEFAULT 0",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE trading_config ADD COLUMN index_lots_by_symbol TEXT NOT NULL DEFAULT '{}'",
    ).await;

    ensure_column(
        &pool,
        "ALTER TABLE paper_trades ADD COLUMN signal_id TEXT",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE paper_trades ADD COLUMN raw_message TEXT",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE paper_trades ADD COLUMN exit_reason TEXT",
    ).await;
    ensure_column(
        &pool,
        "ALTER TABLE paper_trades ADD COLUMN mode TEXT NOT NULL DEFAULT 'PAPER'",
    ).await;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS open_positions (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            json TEXT NOT NULL DEFAULT '[]',
            updated_at DATETIME NOT NULL
        )",
    ).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO open_positions (id, json, updated_at) VALUES (1, '[]', ?)",
    )
    .bind(current_ist_timestamp_string())
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS kotak_session (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            access_token TEXT NOT NULL,
            auth_token TEXT NOT NULL,
            sid TEXT NOT NULL,
            base_url TEXT NOT NULL,
            updated_at DATETIME NOT NULL
        )",
    ).execute(&pool).await.unwrap();

    // ── Strategy engine tables ───────────────────────────────────────────
    // Both are additive `CREATE TABLE IF NOT EXISTS`, so an existing trades.db
    // gains them in place. No migration wipes any data and no reset is needed.

    // Minute bars, persisted so indicators survive a restart and each session
    // after the first starts warm. Kotak publishes no historical candle API, so
    // this table is the only price history the strategy will ever have.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS strategy_candles (
            scrip_key TEXT NOT NULL,
            minute INTEGER NOT NULL,
            open REAL NOT NULL, high REAL NOT NULL, low REAL NOT NULL, close REAL NOT NULL,
            volume REAL NOT NULL DEFAULT 0.0,
            ticks INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (scrip_key, minute)
        )",
    ).execute(&pool).await.unwrap();

    // Decision log — one row per evaluation that produced a signal or an
    // explicit refusal, so the debate can be audited after the fact.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS strategy_decisions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            underlying TEXT NOT NULL,
            regime TEXT NOT NULL,
            stance TEXT NOT NULL,
            conviction REAL NOT NULL,
            actionable INTEGER NOT NULL DEFAULT 0,
            published INTEGER NOT NULL DEFAULT 0,
            outcome TEXT NOT NULL,
            detail_json TEXT NOT NULL,
            timestamp DATETIME NOT NULL
        )",
    ).execute(&pool).await.unwrap();

    // Strategy configuration, stored as JSON so tuning a threshold never needs
    // a schema migration.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS strategy_config (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            json TEXT NOT NULL,
            updated_at DATETIME NOT NULL
        )",
    ).execute(&pool).await.unwrap();

    pool
}

// ---------------------------------------------------------------------------
// Strategy persistence
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct CandleRow {
    minute: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    ticks: i64,
}

/// Load the most recent `limit` bars for one instrument, oldest first.
pub async fn load_candles(
    pool: &SqlitePool,
    scrip_key: &str,
    limit: i64,
) -> Vec<trading_engine::strategy::Candle> {
    // Newest-first with LIMIT, then reversed — the series expects ascending
    // order but we want the *latest* window, not the oldest.
    let rows = sqlx::query_as::<_, CandleRow>(
        "SELECT minute, open, high, low, close, volume, ticks
         FROM strategy_candles WHERE scrip_key = ? ORDER BY minute DESC LIMIT ?",
    )
    .bind(scrip_key)
    .bind(limit)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .rev()
        .map(|r| trading_engine::strategy::Candle {
            minute: r.minute,
            open: r.open, high: r.high, low: r.low, close: r.close,
            volume: r.volume,
            ticks: r.ticks.clamp(0, u32::MAX as i64) as u32,
        })
        .collect()
}

/// Distinct instruments that already have persisted history.
pub async fn candle_keys(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar::<_, String>("SELECT DISTINCT scrip_key FROM strategy_candles")
        .fetch_all(pool)
        .await
        .unwrap_or_default()
}

/// Persist one closed bar. Idempotent — replaying the same minute overwrites
/// rather than duplicating, which matters after a restart mid-minute.
pub async fn save_candle(pool: &SqlitePool, scrip_key: &str, c: &trading_engine::strategy::Candle) {
    let _ = sqlx::query(
        "INSERT INTO strategy_candles (scrip_key, minute, open, high, low, close, volume, ticks)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(scrip_key, minute) DO UPDATE SET
            open = excluded.open, high = excluded.high, low = excluded.low,
            close = excluded.close, volume = excluded.volume, ticks = excluded.ticks",
    )
    .bind(scrip_key)
    .bind(c.minute)
    .bind(c.open).bind(c.high).bind(c.low).bind(c.close)
    .bind(c.volume)
    .bind(c.ticks as i64)
    .execute(pool)
    .await;
}

/// Drop bars older than `keep_days`, so the table cannot grow without bound.
pub async fn prune_candles(pool: &SqlitePool, keep_days: i64) {
    let cutoff_minute = (chrono::Utc::now().timestamp() / 60) - keep_days * 24 * 60;
    let _ = sqlx::query("DELETE FROM strategy_candles WHERE minute < ?")
        .bind(cutoff_minute)
        .execute(pool)
        .await;
}

/// Append one decision to the audit log.
pub async fn save_decision(
    pool: &SqlitePool,
    d: &trading_engine::strategy::engine::Decision,
    published: bool,
) {
    let detail = serde_json::to_string(d).unwrap_or_else(|_| "{}".into());
    let _ = sqlx::query(
        "INSERT INTO strategy_decisions
            (underlying, regime, stance, conviction, actionable, published, outcome, detail_json, timestamp)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&d.underlying)
    .bind(d.regime.regime.as_str())
    .bind(d.debate.stance.as_str())
    .bind(d.debate.conviction)
    .bind(d.debate.actionable as i32)
    .bind(published as i32)
    .bind(&d.outcome)
    .bind(detail)
    .bind(current_ist_timestamp_string())
    .execute(pool)
    .await;
}

/// Recent decisions, newest first, as raw JSON detail for the dashboard.
pub async fn load_recent_decisions(pool: &SqlitePool, limit: i64) -> Vec<serde_json::Value> {
    sqlx::query_scalar::<_, String>(
        "SELECT detail_json FROM strategy_decisions ORDER BY id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .filter_map(|s| serde_json::from_str(&s).ok())
    .collect()
}

/// Load the persisted strategy configuration.
///
/// Falls back to defaults when absent **or invalid** — a config that fails
/// validation is treated as no config at all, since defaults are disabled and
/// therefore safe, while a half-parsed one is not.
pub async fn load_strategy_config(pool: &SqlitePool) -> trading_engine::StrategyConfig {
    let stored = sqlx::query_scalar::<_, String>("SELECT json FROM strategy_config WHERE id = 1")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

    match stored.as_deref().map(serde_json::from_str::<trading_engine::StrategyConfig>) {
        Some(Ok(cfg)) => match cfg.validate() {
            Ok(()) => cfg,
            Err(e) => {
                tracing::error!(error = %e, "stored strategy config is invalid — falling back to safe defaults");
                trading_engine::StrategyConfig::default()
            }
        },
        Some(Err(e)) => {
            tracing::error!(error = %e, "stored strategy config could not be parsed — falling back to safe defaults");
            trading_engine::StrategyConfig::default()
        }
        None => trading_engine::StrategyConfig::default(),
    }
}

/// Persist the strategy configuration.
pub async fn save_strategy_config(pool: &SqlitePool, cfg: &trading_engine::StrategyConfig) -> Result<(), String> {
    cfg.validate()?;
    let json = serde_json::to_string(cfg).map_err(|e| e.to_string())?;
    sqlx::query(
        "INSERT INTO strategy_config (id, json, updated_at) VALUES (1, ?, ?)
         ON CONFLICT(id) DO UPDATE SET json = excluded.json, updated_at = excluded.updated_at",
    )
    .bind(json)
    .bind(current_ist_timestamp_string())
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Config loader
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct TradingConfigRow {
    max_trade_amount_inr: f64,
    index_lots: i32,
    other_lots: i32,
    mode: String,
    brokerage_per_order: f64,
    target_1_exit_pct: f64,
    target_2_exit_pct: f64,
    entry_market_protection: f64,
    dynamic_targeting: bool,
    index_lots_by_symbol: String,
}

/// Load `TradingConfig` from SQLite, falling back to safe defaults.
pub async fn load_config_from_db(pool: &SqlitePool) -> TradingConfig {
    sqlx::query_as::<_, TradingConfigRow>(
        "SELECT max_trade_amount_inr, index_lots, other_lots, mode, brokerage_per_order,
                target_1_exit_pct, target_2_exit_pct, entry_market_protection, dynamic_targeting,
                index_lots_by_symbol
         FROM trading_config WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .map(|r| TradingConfig {
        max_trade_amount_inr: r.max_trade_amount_inr,
        index_lots: r.index_lots.max(1),
        other_lots: r.other_lots.max(1),
        mode: r.mode,
        brokerage_per_order: r.brokerage_per_order,
        target_1_exit_pct: r.target_1_exit_pct,
        target_2_exit_pct: r.target_2_exit_pct,
        entry_market_protection: r.entry_market_protection,
        dynamic_targeting: r.dynamic_targeting,
        index_lots_by_symbol: serde_json::from_str(&r.index_lots_by_symbol).unwrap_or_default(),
    })
    .unwrap_or_else(|| TradingConfig {
        max_trade_amount_inr: 10_000.0,
        index_lots: 1,
        other_lots: 1,
        mode: "PAPER".into(),
        brokerage_per_order: 20.0,
        target_1_exit_pct: 50.0,
        target_2_exit_pct: 100.0,
        entry_market_protection: 5.0,
        dynamic_targeting: false,
        index_lots_by_symbol: Default::default(),
    })
}

pub async fn load_open_positions(pool: &SqlitePool) -> Vec<MonitoredPosition> {
    let json = sqlx::query_scalar::<_, String>(
        "SELECT json FROM open_positions WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "[]".to_string());

    serde_json::from_str::<Vec<MonitoredPosition>>(&json)
        .unwrap_or_default()
}

/// Mirrors the `kotak_session` row. `updated_at` is unused by the restore
/// path but kept so the struct matches the table it is selected from.
#[allow(dead_code)]
pub struct KotakSessionRow {
    pub access_token: String,
    pub auth_token: String,
    pub sid: String,
    pub base_url: String,
    pub updated_at: String,
}

pub async fn save_kotak_session(pool: &SqlitePool, access: &str, auth: &str, sid: &str, base_url: &str) {
    sqlx::query(
        "INSERT INTO kotak_session (id, access_token, auth_token, sid, base_url, updated_at) 
         VALUES (1, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET 
         access_token = excluded.access_token,
         auth_token = excluded.auth_token,
         sid = excluded.sid,
         base_url = excluded.base_url,
         updated_at = excluded.updated_at"
    )
    .bind(access)
    .bind(auth)
    .bind(sid)
    .bind(base_url)
    .bind(shared_domain::current_ist_timestamp_string())
    .execute(pool)
    .await
    .unwrap();
}

pub async fn load_kotak_session(pool: &SqlitePool) -> Option<KotakSessionRow> {
    let row = sqlx::query_as::<_, (String, String, String, String, String)>(
        "SELECT access_token, auth_token, sid, base_url, updated_at FROM kotak_session WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;

    let (access_token, auth_token, sid, base_url, updated_at) = row;
    
    // Check if the session is from today in IST. If not, it's expired.
    if !updated_at.starts_with(&shared_domain::now_ist().format("%Y-%m-%d").to_string()) {
        return None;
    }

    Some(KotakSessionRow {
        access_token,
        auth_token,
        sid,
        base_url,
        updated_at,
    })
}

// ---------------------------------------------------------------------------
// Sequential writer task
// ---------------------------------------------------------------------------

/// Dedicated SQLite writer — processes one `DbWriteMessage` at a time,
/// eliminating "database is locked" errors from concurrent writers.
pub async fn db_writer(mut rx: mpsc::Receiver<DbWriteMessage>, pool: SqlitePool) {
    while let Some(msg) = rx.recv().await {
        match msg {
            DbWriteMessage::Trade {
                ticker, action, qty, executed_price,
                gross_value, brokerage, stt_charge, sebi_fee,
                stamp_duty, transaction_charge, gst, net_value,
                signal_id, raw_message, exit_reason, mode,
            } => {
                let timestamp = current_ist_timestamp_string();
                let mut tx = match pool.begin().await {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!("DB begin tx: {e}");
                        continue;
                    }
                };

                if let Err(e) = sqlx::query(
                    "INSERT INTO paper_trades
                     (ticker, action, qty, executed_price, timestamp,
                      gross_value, brokerage, stt_charge, sebi_fee,
                      stamp_duty, transaction_charge, gst, net_value,
                      signal_id, raw_message, exit_reason, mode)
                     VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                )
                 .bind(&ticker).bind(&action).bind(qty as i64).bind(executed_price).bind(&timestamp)
                .bind(gross_value).bind(brokerage).bind(stt_charge).bind(sebi_fee)
                .bind(stamp_duty).bind(transaction_charge).bind(gst).bind(net_value)
                .bind(&signal_id).bind(&raw_message).bind(&exit_reason).bind(&mode)
                .execute(&mut *tx).await
                {
                    tracing::error!("DB trade insert: {e}");
                    let _ = tx.rollback().await;
                    continue;
                }

                let wallet_delta = if action.eq_ignore_ascii_case("BUY") {
                    -net_value
                } else {
                    net_value
                };

                if let Err(e) = sqlx::query("UPDATE wallet SET balance = balance + ? WHERE id = 1")
                    .bind(wallet_delta)
                    .execute(&mut *tx)
                    .await
                {
                    tracing::error!("DB wallet update: {e}");
                    let _ = tx.rollback().await;
                    continue;
                }

                if let Err(e) = tx.commit().await {
                    tracing::error!("DB commit tx: {e}");
                }
            }
            DbWriteMessage::Log { level, message } => {
                let timestamp = current_ist_timestamp_string();
                if let Err(e) = sqlx::query(
                    "INSERT INTO system_logs (level, message, timestamp) VALUES (?, ?, ?)",
                )
                .bind(&level).bind(&message).bind(&timestamp).execute(&pool).await
                {
                    tracing::error!("DB log insert: {e}");
                }
            }
            DbWriteMessage::PositionsSnapshot { json } => {
                let timestamp = current_ist_timestamp_string();
                if let Err(e) = sqlx::query(
                    "UPDATE open_positions SET json = ?, updated_at = ? WHERE id = 1",
                )
                .bind(&json)
                .bind(&timestamp)
                .execute(&pool)
                .await
                {
                    tracing::error!("DB positions snapshot update: {e}");
                }
            }
        }
    }
}
