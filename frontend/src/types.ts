// ---------------------------------------------------------------------------
// Types (mirror shared_domain structs)
// ---------------------------------------------------------------------------

export interface TradingConfig {
  max_trade_amount_inr: number;
  index_lots: number;
  other_lots: number;
  /** Per-index default lot count (key: index symbol, e.g. "NIFTY"). Falls back to `index_lots` when absent. */
  index_lots_by_symbol: Record<string, number>;
  mode: string;
  brokerage_per_order: number;
  target_1_exit_pct: number;
  target_2_exit_pct: number;
  entry_market_protection: number;
  /** Sell one lot at target 1, then trail an ever-extending target ladder for
   * the runner instead of exiting at the signal's fixed target 2. */
  dynamic_targeting: boolean;
}

export interface PaperTrade {
  id: number;
  ticker: string;
  action: string;
  qty: number;
  executed_price: number;
  gross_value: number;
  brokerage: number;
  stt_charge: number;
  sebi_fee: number;
  stamp_duty: number;
  transaction_charge: number;
  gst: number;
  net_value: number;
  timestamp: string;
  exit_reason?: string;
  mode?: string;
  signal_id?: string;
  raw_message?: string;
}

export interface Portfolio {
  balance: number;
  /** "LIVE" | "PAPER" | "LIVE_UNAVAILABLE" — see backend Portfolio struct. */
  balance_source: string;
  trades: PaperTrade[];
}

export interface HealthSnapshot {
  generated_at_ist: string;
  hostname: string | null;
  os_name: string | null;
  os_version: string | null;
  kernel_version: string | null;
  uptime_secs: number;
  cpu_cores: number;
  cpu_usage_pct: number;
  load_average: {
    one: number;
    five: number;
    fifteen: number;
  };
  memory: {
    total_mib: number;
    used_mib: number;
    free_mib: number;
  };
  swap: {
    total_mib: number;
    used_mib: number;
    free_mib: number;
  };
  current_process: {
    pid: string;
    name: string;
    cpu_usage_pct: number;
    memory_mib: number;
    virtual_memory_mib: number;
    run_time_secs: number;
  } | null;
}

export interface MonitoredPosition {
  id: string;
  signal: {
    instrument_name: string;
    action: string;
    entry_condition: string;
    entry_price: number;
    stop_loss: number;
    targets: number[];
  };
  state: string;
  current_sl: number;
  executed_qty: number;
  avg_buy_price: number;
  override_qty: number | null;
  resolved_order?: {
    quantity?: string;
    trading_symbol?: string;
    exchange_segment?: string;
    order_type?: string;
    product_code?: string;
    validity?: string;
    transaction_type?: string;
    trigger_price?: string;
    price?: string;
  };
  ltp?: number;
  ws_scrip_key?: string | null;
}

export interface KotakForm {
  server_base: string;
  access_token: string;
  mobile_number: string;
  ucc: string;
  totp: string;
  mpin: string;
}

export interface TelegramChat {
  id: number;
  name: string;
  kind: string;
}

// ---------------------------------------------------------------------------
// On-demand broker reconciliation ("Sync with Kotak")
// ---------------------------------------------------------------------------

export type ReconcileCategory =
  | 'Matches'
  | 'QtyReduced'
  | 'QtyZero'
  | 'QtyIncreased'
  | 'UnexplainedExposure'
  | 'DuplicateAmbiguous';

export type ReconcileActionKind = 'AdoptQty' | 'Close' | 'Ignore';

export interface ReconcileOption {
  action: ReconcileActionKind;
  label: string;
  recommended: boolean;
}

export interface ReconcileFinding {
  position_id: string | null;
  trading_symbol: string;
  instrument: string;
  category: ReconcileCategory;
  engine_qty: number;
  broker_qty: number;
  message: string;
  options: ReconcileOption[];
}

export interface ReconcileApplyItem {
  position_id: string | null;
  trading_symbol: string;
  action: ReconcileActionKind;
}

export type ScreenId = 'dashboard' | 'positions' | 'analytics' | 'portfolio' | 'strategy' | 'settings';

// ---------------------------------------------------------------------------
// Autonomous strategy engine (mirrors trading_engine::strategy)
// ---------------------------------------------------------------------------

export type StanceLabel = 'BULLISH' | 'BEARISH' | 'NEUTRAL';

export type RegimeLabel =
  | 'TRENDING_UP'
  | 'TRENDING_DOWN'
  | 'RANGEBOUND'
  | 'MEAN_REVERTING'
  | 'HIGH_VOLATILITY'
  | 'UNKNOWN';

export interface AgentView {
  agent: string;
  stance: StanceLabel;
  confidence: number;
  weight: number;
  evidence: string[];
}

export interface RegimeReading {
  regime: RegimeLabel;
  spot: number;
  ema_fast: number;
  ema_medium: number;
  ema_slow: number;
  ribbon_direction: number;
  ribbon_spread_pct: number;
  atr: number;
  atr_percentile: number;
  vwap: number;
  vwap_z: number;
  rsi: number;
  rationale: string;
}

export interface DebateOutcome {
  views: AgentView[];
  bull_score: number;
  bear_score: number;
  conviction: number;
  margin: number;
  stance: StanceLabel;
  actionable: boolean;
  summary: string;
}

export interface StrikeCandidate {
  trading_symbol: string;
  instrument_token: string;
  exchange_segment: string;
  strike: number;
  option_type: string;
  expiry: string;
  lot_size: number;
  tick_size: number;
  ws_key: string;
  otm_steps: number;
  premium: number | null;
  open_interest: number | null;
  spread_pct: number | null;
  /** Null when accepted; otherwise why this contract was rejected. */
  rejected: string | null;
  score: number;
}

export interface StrikeSelection {
  chosen: StrikeCandidate | null;
  considered: StrikeCandidate[];
  reason: string;
}

/** Serde externally-tagged enum: exactly one key is present. */
export type RiskDecision =
  | { Approved: { lots: number; rationale: string } }
  | { Rejected: { reason: string } };

export interface RiskState {
  realised_pnl: number;
  entries_today: number;
  consecutive_losses: number;
  halted_reason: string | null;
  session_date: string;
}

export interface StrategyDecision {
  underlying: string;
  regime: RegimeReading;
  debate: DebateOutcome;
  selection: StrikeSelection | null;
  risk: RiskDecision | null;
  signal: TradeSignalLike | null;
  outcome: string;
  evaluated_at: string;
}

export interface TradeSignalLike {
  instrument_name: string;
  strike: number | null;
  option_type: string | null;
  expiry: string | null;
  action: string;
  entry_price: number;
  targets: number[];
  stop_loss: number;
  source: string;
  paper_only: boolean;
}

export interface IndexSpec {
  symbol: string;
  spot_key: string;
  strike_step: number;
}

export interface StrategyConfig {
  enabled: boolean;
  indices: IndexSpec[];
  min_bars_for_signal: number;
  series_capacity: number;
  ema_fast: number;
  ema_medium: number;
  ema_slow: number;
  atr_period: number;
  rsi_period: number;
  opening_range_bars: number;
  vwap_band_sigma: number;
  max_tick_age_ms: number;
  conviction_threshold: number;
  min_debate_margin: number;
  strike_search_steps: number;
  max_spread_pct: number;
  min_open_interest: number;
  min_premium: number;
  max_premium: number;
  preferred_otm_steps: number;
  min_days_to_expiry: number;
  expiry_day_no_entry_hour: number;
  expiry_day_no_entry_minute: number;
  max_open_positions: number;
  max_algo_entries_per_day: number;
  max_daily_loss_inr: number;
  max_consecutive_losses: number;
  one_position_per_underlying: boolean;
  no_entry_hour: number;
  no_entry_minute: number;
  stop_loss_pct: number;
  target_1_pct: number;
  target_2_pct: number;
  min_reward_risk: number;
}

export interface StrategySnapshot {
  enabled: boolean;
  /** True only when the engine is enabled AND trading mode is PAPER. */
  publishing: boolean;
  publish_block_reason: string | null;
  risk: RiskState;
  decisions: StrategyDecision[];
  /** [feed key, closed bar count] pairs. */
  warmup: [string, number][];
  config: StrategyConfig;
}

export type TgStep = 'idle' | 'code' | 'twofa' | 'chats' | 'running';
