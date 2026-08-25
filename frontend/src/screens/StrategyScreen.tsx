import { useCallback, useEffect, useState } from 'react';
import {
  AlertTriangle,
  Activity,
  Ban,
  Brain,
  CheckCircle2,
  Gauge,
  Loader2,
  Pause,
  Play,
  ShieldAlert,
  TrendingDown,
  TrendingUp,
  XCircle,
} from 'lucide-react';
import { apiFetch } from '../lib/api';
import type {
  AgentView,
  RegimeLabel,
  StanceLabel,
  StrategyDecision,
  StrategySnapshot,
} from '../types';

const POLL_MS = 5000;

// ---------------------------------------------------------------------------
// Presentation helpers
// ---------------------------------------------------------------------------

function stanceColor(stance: StanceLabel) {
  if (stance === 'BULLISH') return 'text-green-500';
  if (stance === 'BEARISH') return 'text-error';
  return 'text-on-surface-variant';
}

function stanceBadge(stance: StanceLabel) {
  if (stance === 'BULLISH') return 'bg-green-500/15 text-green-500';
  if (stance === 'BEARISH') return 'bg-error/15 text-error';
  return 'bg-surface-container-high text-on-surface-variant';
}

/** Regimes that permit a long-premium entry are highlighted; the rest read as "standing down". */
function regimeBadge(regime: RegimeLabel) {
  switch (regime) {
    case 'TRENDING_UP':
      return 'bg-green-500/15 text-green-500';
    case 'TRENDING_DOWN':
      return 'bg-error/15 text-error';
    case 'HIGH_VOLATILITY':
      return 'bg-amber-500/15 text-amber-500';
    default:
      return 'bg-surface-container-high text-on-surface-variant';
  }
}

function num(v: number | null | undefined, digits = 2, fallback = '—') {
  return v === null || v === undefined || Number.isNaN(v) ? fallback : v.toFixed(digits);
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function AgentCard({ view }: { view: AgentView }) {
  const Icon =
    view.stance === 'BULLISH' ? TrendingUp : view.stance === 'BEARISH' ? TrendingDown : Activity;

  return (
    <div className="rounded-lg border border-outline-variant bg-surface-container p-3">
      <div className="flex items-center justify-between mb-2">
        <div className="flex items-center gap-2 min-w-0">
          <Icon size={15} className={`shrink-0 ${stanceColor(view.stance)}`} />
          <span className="text-sm font-bold text-on-surface truncate">{view.agent}</span>
        </div>
        <span className={`px-2 py-0.5 rounded text-[10px] font-bold shrink-0 ${stanceBadge(view.stance)}`}>
          {view.stance}
        </span>
      </div>

      <div className="flex items-center gap-2 mb-2">
        <div className="flex-1 h-1.5 rounded-full bg-surface-container-high overflow-hidden">
          <div
            className={`h-full rounded-full ${
              view.stance === 'BULLISH'
                ? 'bg-green-500'
                : view.stance === 'BEARISH'
                  ? 'bg-error'
                  : 'bg-outline'
            }`}
            style={{ width: `${Math.min(100, Math.max(0, view.confidence))}%` }}
          />
        </div>
        <span className="text-[11px] font-mono text-on-surface-variant shrink-0">
          {num(view.confidence, 0)}%
        </span>
        <span className="text-[10px] text-on-surface-variant shrink-0" title="Weight in the debate">
          ×{num(view.weight, 1)}
        </span>
      </div>

      <ul className="space-y-1">
        {view.evidence.map((e, i) => (
          <li key={i} className="text-[11px] leading-snug text-on-surface-variant flex gap-1.5">
            <span className="text-outline shrink-0">•</span>
            <span>{e}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

function DecisionCard({ d }: { d: StrategyDecision }) {
  const rejected = d.selection?.considered.filter((c) => c.rejected) ?? [];
  const chosen = d.selection?.chosen ?? null;

  return (
    <div className="rounded-xl border border-outline-variant bg-surface p-4 space-y-4">
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2.5">
          <h3 className="text-base font-bold text-on-surface">{d.underlying}</h3>
          <span className={`px-2 py-0.5 rounded text-[10px] font-bold ${regimeBadge(d.regime.regime)}`}>
            {d.regime.regime.replace(/_/g, ' ')}
          </span>
          {d.signal ? (
            <span className="px-2 py-0.5 rounded text-[10px] font-bold bg-primary-container text-on-primary flex items-center gap-1">
              <CheckCircle2 size={11} /> SIGNAL
            </span>
          ) : null}
        </div>
        <span className="text-[11px] font-mono text-on-surface-variant">{d.evaluated_at}</span>
      </div>

      {/* Regime metrics */}
      <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-6 gap-2">
        {[
          ['Spot', num(d.regime.spot, 1)],
          ['VWAP', num(d.regime.vwap, 1)],
          ['VWAP σ', `${d.regime.vwap_z >= 0 ? '+' : ''}${num(d.regime.vwap_z, 1)}`],
          ['ATR', num(d.regime.atr, 1)],
          ['ATR %ile', num(d.regime.atr_percentile, 0)],
          ['RSI', num(d.regime.rsi, 0)],
        ].map(([label, value]) => (
          <div key={label} className="rounded-lg bg-surface-container px-2.5 py-1.5">
            <div className="text-[10px] font-semibold uppercase tracking-wider text-on-surface-variant">
              {label}
            </div>
            <div className="text-sm font-mono font-bold text-on-surface">{value}</div>
          </div>
        ))}
      </div>

      <p className="text-xs text-on-surface-variant italic leading-relaxed">{d.regime.rationale}</p>

      {/* Debate */}
      <div>
        <div className="flex items-center gap-2 mb-2">
          <Brain size={14} className="text-primary" />
          <span className="text-xs font-bold uppercase tracking-wider text-on-surface-variant">
            Bull vs Bear Debate
          </span>
        </div>

        {/* Opposing score bar */}
        <div className="flex items-center gap-2 mb-2">
          <span className="text-[11px] font-mono font-bold text-green-500 w-8 text-right">
            {num(d.debate.bull_score, 0)}
          </span>
          <div className="flex-1 h-2.5 rounded-full bg-surface-container-high overflow-hidden flex">
            <div className="h-full bg-green-500" style={{ width: `${d.debate.bull_score}%` }} />
            <div className="h-full bg-error ml-auto" style={{ width: `${d.debate.bear_score}%` }} />
          </div>
          <span className="text-[11px] font-mono font-bold text-error w-8">
            {num(d.debate.bear_score, 0)}
          </span>
        </div>

        <p
          className={`text-xs font-medium mb-3 ${
            d.debate.actionable ? 'text-on-surface' : 'text-on-surface-variant'
          }`}
        >
          {d.debate.summary}
        </p>

        <div className="grid grid-cols-1 md:grid-cols-3 gap-2">
          {d.debate.views.map((v) => (
            <AgentCard key={v.agent} view={v} />
          ))}
        </div>
      </div>

      {/* Strike selection */}
      {d.selection ? (
        <div>
          <div className="flex items-center gap-2 mb-2">
            <Gauge size={14} className="text-primary" />
            <span className="text-xs font-bold uppercase tracking-wider text-on-surface-variant">
              Strike Selection
            </span>
          </div>

          {chosen ? (
            <div className="rounded-lg border border-primary/40 bg-primary-container/10 p-3 mb-2">
              <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
                <span className="text-sm font-bold text-on-surface">{chosen.trading_symbol}</span>
                <span className="text-xs text-on-surface-variant">
                  {chosen.otm_steps === 0 ? 'ATM' : `${chosen.otm_steps} step OTM`}
                </span>
              </div>
              <div className="flex flex-wrap gap-x-4 gap-y-1 mt-1.5 text-[11px] font-mono text-on-surface-variant">
                <span>Premium ₹{num(chosen.premium)}</span>
                <span>OI {num(chosen.open_interest, 0)}</span>
                <span>Spread {num(chosen.spread_pct)}%</span>
                <span>Lot {chosen.lot_size}</span>
              </div>
            </div>
          ) : (
            <p className="text-xs text-on-surface-variant mb-2">{d.selection.reason}</p>
          )}

          {rejected.length > 0 ? (
            <details className="group">
              <summary className="text-[11px] font-semibold text-on-surface-variant cursor-pointer hover:text-on-surface select-none">
                {rejected.length} contract{rejected.length === 1 ? '' : 's'} failed the liquidity gates
              </summary>
              <ul className="mt-1.5 space-y-1 pl-3">
                {rejected.map((c) => (
                  <li key={c.instrument_token} className="text-[11px] text-on-surface-variant flex gap-2">
                    <XCircle size={11} className="text-error shrink-0 mt-0.5" />
                    <span>
                      <span className="font-mono">{num(c.strike, 0)} {c.option_type}</span> — {c.rejected}
                    </span>
                  </li>
                ))}
              </ul>
            </details>
          ) : null}
        </div>
      ) : null}

      {/* Risk */}
      {d.risk ? (
        <div className="flex items-start gap-2 rounded-lg bg-surface-container p-2.5">
          <ShieldAlert
            size={14}
            className={`shrink-0 mt-0.5 ${'Approved' in d.risk ? 'text-green-500' : 'text-amber-500'}`}
          />
          <span className="text-[11px] leading-snug text-on-surface-variant">
            {'Approved' in d.risk ? d.risk.Approved.rationale : d.risk.Rejected.reason}
          </span>
        </div>
      ) : null}

      {/* Outcome */}
      <div className="pt-2 border-t border-outline-variant">
        <span className="text-[10px] font-bold uppercase tracking-wider text-on-surface-variant">
          Outcome
        </span>
        <p className="text-xs text-on-surface mt-0.5">{d.outcome}</p>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Screen
// ---------------------------------------------------------------------------

export function StrategyScreen({ serverBase }: { serverBase: string }) {
  const [snap, setSnap] = useState<StrategySnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    apiFetch(serverBase, '/api/strategy')
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(`HTTP ${r.status}`))))
      .then((data: StrategySnapshot) => {
        setSnap(data);
        setError(null);
      })
      .catch((e: Error) => setError(e.message))
      .finally(() => setLoading(false));
  }, [serverBase]);

  useEffect(() => {
    load();
    const id = setInterval(load, POLL_MS);
    return () => clearInterval(id);
  }, [load]);

  async function toggleEnabled(next: boolean) {
    if (!snap) return;
    setBusy(true);
    try {
      const res = await apiFetch(serverBase, '/api/strategy/config', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ...snap.config, enabled: next }),
      });
      if (!res.ok) setError(await res.text());
      else load();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function haltOrResume(path: 'halt' | 'resume') {
    setBusy(true);
    try {
      await apiFetch(serverBase, `/api/strategy/${path}`, { method: 'POST' });
      load();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64 text-on-surface-variant gap-2">
        <Loader2 size={18} className="animate-spin" />
        <span className="text-sm">Loading strategy engine…</span>
      </div>
    );
  }

  if (!snap) {
    return (
      <div className="rounded-xl border border-error/40 bg-error/10 p-4 text-sm text-error">
        Could not load the strategy engine{error ? `: ${error}` : '.'}
      </div>
    );
  }

  const halted = snap.risk.halted_reason !== null;
  // The warm-up gate reads closed bars, so surface the weakest series.
  const minBars = snap.warmup.length > 0 ? Math.min(...snap.warmup.map(([, n]) => n)) : 0;
  const warmupPct = Math.min(100, (minBars / Math.max(1, snap.config.min_bars_for_signal)) * 100);

  return (
    <div className="space-y-4">
      {/* Control bar */}
      <div className="rounded-xl border border-outline-variant bg-surface p-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-3">
            <Brain size={20} className="text-primary" />
            <div>
              <h2 className="text-base font-bold text-on-surface leading-tight">
                Autonomous Strategy Engine
              </h2>
              <p className="text-[11px] text-on-surface-variant">
                Multi-agent F&amp;O signal generation
              </p>
            </div>
          </div>

          <div className="flex items-center gap-2">
            <button
              onClick={() => toggleEnabled(!snap.enabled)}
              disabled={busy}
              className={`px-3 py-1.5 rounded-lg text-xs font-bold flex items-center gap-1.5 transition-colors disabled:opacity-50 ${
                snap.enabled
                  ? 'bg-error/15 text-error hover:bg-error/25'
                  : 'bg-primary-container text-on-primary hover:bg-primary'
              }`}
            >
              {snap.enabled ? <Pause size={13} /> : <Play size={13} />}
              {snap.enabled ? 'Disable' : 'Enable'}
            </button>
            <button
              onClick={() => haltOrResume(halted ? 'resume' : 'halt')}
              disabled={busy}
              className="px-3 py-1.5 rounded-lg text-xs font-bold bg-surface-container-high text-on-surface hover:bg-surface-container flex items-center gap-1.5 transition-colors disabled:opacity-50"
            >
              <Ban size={13} />
              {halted ? 'Clear halt' : 'Halt session'}
            </button>
          </div>
        </div>

        {/* Publishing status — the single most important thing on this screen. */}
        <div
          className={`mt-3 flex items-start gap-2 rounded-lg p-2.5 ${
            snap.publishing ? 'bg-green-500/10' : 'bg-amber-500/10'
          }`}
        >
          {snap.publishing ? (
            <CheckCircle2 size={15} className="text-green-500 shrink-0 mt-0.5" />
          ) : (
            <AlertTriangle size={15} className="text-amber-500 shrink-0 mt-0.5" />
          )}
          <div className="text-xs leading-snug">
            <span className="font-bold text-on-surface">
              {snap.publishing ? 'Publishing signals (PAPER)' : 'Not publishing'}
            </span>
            <span className="text-on-surface-variant">
              {snap.publishing
                ? ' — simulated fills only, no broker orders.'
                : ` — ${snap.publish_block_reason ?? 'unknown reason'}.`}
            </span>
          </div>
        </div>

        {halted ? (
          <div className="mt-2 flex items-center gap-2 rounded-lg bg-error/10 p-2.5 text-xs text-error">
            <Ban size={14} className="shrink-0" />
            <span>Session halted — {snap.risk.halted_reason}</span>
          </div>
        ) : null}

        {error ? (
          <div className="mt-2 rounded-lg bg-error/10 p-2.5 text-xs text-error">{error}</div>
        ) : null}
      </div>

      {/* Risk + warm-up */}
      <div className="grid grid-cols-2 lg:grid-cols-5 gap-3">
        {[
          {
            label: 'Realised P&L',
            value: `₹${snap.risk.realised_pnl.toFixed(0)}`,
            tone: snap.risk.realised_pnl < 0 ? 'text-error' : 'text-green-500',
          },
          {
            label: 'Entries today',
            value: `${snap.risk.entries_today} / ${snap.config.max_algo_entries_per_day}`,
            tone: 'text-on-surface',
          },
          {
            label: 'Loss streak',
            value: `${snap.risk.consecutive_losses} / ${snap.config.max_consecutive_losses}`,
            tone: snap.risk.consecutive_losses > 0 ? 'text-amber-500' : 'text-on-surface',
          },
          {
            label: 'Daily loss cap',
            value: `₹${snap.config.max_daily_loss_inr.toFixed(0)}`,
            tone: 'text-on-surface',
          },
          {
            label: 'Conviction bar',
            value: `${snap.config.conviction_threshold.toFixed(0)}%`,
            tone: 'text-on-surface',
          },
        ].map((s) => (
          <div key={s.label} className="rounded-xl border border-outline-variant bg-surface p-3">
            <div className="text-[10px] font-semibold uppercase tracking-wider text-on-surface-variant">
              {s.label}
            </div>
            <div className={`text-lg font-bold font-mono mt-0.5 ${s.tone}`}>{s.value}</div>
          </div>
        ))}
      </div>

      {/* Warm-up progress — explains an idle engine at the start of a session. */}
      {minBars < snap.config.min_bars_for_signal ? (
        <div className="rounded-xl border border-outline-variant bg-surface p-4">
          <div className="flex items-center justify-between mb-2">
            <span className="text-xs font-bold uppercase tracking-wider text-on-surface-variant">
              Indicator warm-up
            </span>
            <span className="text-xs font-mono text-on-surface-variant">
              {minBars} / {snap.config.min_bars_for_signal} bars
            </span>
          </div>
          <div className="h-2 rounded-full bg-surface-container-high overflow-hidden">
            <div className="h-full rounded-full bg-primary transition-all" style={{ width: `${warmupPct}%` }} />
          </div>
          <p className="text-[11px] text-on-surface-variant mt-2">
            No signal is generated until every series is warm. Bars persist across restarts, so this only runs
            from empty on a first session.
          </p>
        </div>
      ) : null}

      {/* Decisions */}
      {snap.decisions.length === 0 ? (
        <div className="rounded-xl border border-outline-variant bg-surface p-8 text-center">
          <Activity size={24} className="mx-auto text-on-surface-variant mb-2" />
          <p className="text-sm text-on-surface-variant">
            {snap.enabled
              ? 'No evaluations yet — waiting for market data.'
              : 'The engine is disabled. Enable it to start evaluating.'}
          </p>
        </div>
      ) : (
        <div className="space-y-4">
          {snap.decisions.map((d) => (
            <DecisionCard key={d.underlying} d={d} />
          ))}
        </div>
      )}
    </div>
  );
}
