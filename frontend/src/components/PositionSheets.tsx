import { useState } from 'react';
import type { ReactNode } from 'react';
import { AlertTriangle, CheckCircle2, Clock, Loader2, Minus, Plus, XCircle } from 'lucide-react';
import type { MonitoredPosition } from '../types';
import { fmt, fmtInrSigned } from '../lib/format';
import { Sheet } from './Sheet';

/** What became of a manual close or sell: done (PAPER), handed to the engine (LIVE, 202), or refused. */
export type ActionResult =
  | { kind: 'done'; message: string }
  | { kind: 'requested'; message: string }
  | { kind: 'error'; message: string };

/**
 * The quantity step the server accepts for a manual sell. `sell_position_handler`
 * rejects any quantity that is not a multiple of `resolved_order.quantity`, so the
 * stepper moves in exactly that step; without one, any whole number is accepted.
 */
export function sellStep(p: MonitoredPosition): number {
  const n = parseInt(p.resolved_order?.quantity ?? '', 10);
  return Number.isFinite(n) && n > 0 ? n : 1;
}

/** An exit of some kind is already in flight for this position: no second one from the UI. */
export function exitInFlight(p: MonitoredPosition): string | null {
  if (p.force_exit) return 'Exit requested — waiting for the engine to sell';
  if (p.manual_sell_qty) return `Sell of ${p.manual_sell_qty} requested — waiting for the engine`;
  if (p.pending_exit_order_id) {
    return `Sell${p.pending_exit_qty ? ` of ${p.pending_exit_qty}` : ''} placed at the broker — waiting for the fill`;
  }
  return null;
}

/** Why this sheet must not send now: the position changed under it after it opened. */
function blockedReason(p: MonitoredPosition): string | null {
  if (p.state !== 'Active' && p.state !== 'Target1Hit') return 'This position is no longer open.';
  const f = exitInFlight(p);
  return f ? `${f}. Nothing more can be sent until it settles.` : null;
}

function ModeBadge({ mode }: { mode?: string }) {
  if (!mode) return null;
  const live = mode === 'LIVE';
  return (
    <span className={`px-1.5 py-0.5 rounded text-[10px] font-bold uppercase tracking-wide ${
      live ? 'bg-error-container text-on-error-container' : 'bg-surface-container-high text-on-surface-variant'
    }`}>
      {live ? 'Live · Kotak' : 'Paper'}
    </span>
  );
}

function Title({ p, mode, verb }: { p: MonitoredPosition; mode?: string; verb: string }) {
  return (
    <div className="min-w-0">
      <div className="flex items-center gap-2">
        <span className="text-[11px] font-bold uppercase tracking-wide text-error">{verb}</span>
        <ModeBadge mode={mode} />
      </div>
      <h2 className="text-lg font-bold text-on-surface truncate">{p.signal.instrument_name}</h2>
      <div className="text-xs text-on-surface-variant">
        LTP <span className="font-mono-code font-semibold text-on-surface tabular-nums">{p.ltp != null ? `₹${fmt(p.ltp)}` : '—'}</span>
        {' · '}held <span className="font-mono-code font-semibold text-on-surface tabular-nums">{p.executed_qty}</span>
        {' @ '}<span className="font-mono-code tabular-nums">₹{fmt(p.avg_buy_price)}</span>
      </div>
    </div>
  );
}

function Rows({ rows }: { rows: [ReactNode, ReactNode, string?][] }) {
  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm bg-surface-container-low border border-outline-variant/60 rounded-xl px-4 py-3">
      {rows.map(([k, v, cls], i) => (
        <div key={i} className="contents">
          <dt className="text-on-surface-variant">{k}</dt>
          <dd className={`text-right font-mono-code font-semibold tabular-nums ${cls ?? 'text-on-surface'}`}>{v}</dd>
        </div>
      ))}
    </dl>
  );
}

function ResultNote({ result }: { result: ActionResult }) {
  const style = {
    done:      { cls: 'bg-secondary/10 text-secondary border-secondary/30', Icon: CheckCircle2 },
    requested: { cls: 'bg-primary/10 text-primary border-primary/30', Icon: Clock },
    error:     { cls: 'bg-error-container text-on-error-container border-error/30', Icon: XCircle },
  }[result.kind];
  return (
    <p role="status" className={`flex items-start gap-2 text-sm rounded-xl px-3 py-2.5 border ${style.cls}`}>
      <style.Icon size={16} className="shrink-0 mt-0.5" />
      <span>{result.message}</span>
    </p>
  );
}

const pnlClass = (n: number) => (n > 0 ? 'text-secondary' : n < 0 ? 'text-error' : 'text-on-surface');

const btnDanger = 'w-full h-12 rounded-xl bg-error text-on-error font-bold text-sm flex items-center justify-center gap-2 disabled:opacity-40 transition-opacity';
const btnPlain = 'w-full h-12 rounded-xl bg-surface-container hover:bg-surface-container-high text-on-surface font-bold text-sm';

/**
 * Sell part of a position at market. The quantity starts at one step (the
 * smallest chunk the server accepts) so a mis-tap sells the least; Max is one
 * tap away. It can never ask for more than is held, and it sends the same
 * `POST /api/positions/:id/sell { qty }` the old prompt did.
 */
export function SellQtySheet({ p, mode, onSubmit, onClose }: {
  p: MonitoredPosition;
  mode?: string;
  onSubmit: (qty: number) => Promise<ActionResult>;
  onClose: () => void;
}) {
  const held = p.executed_qty;
  const step = sellStep(p);
  const max = Math.floor(held / step) * step;
  const [qty, setQty] = useState(() => (max >= step ? step : 0));
  const [text, setText] = useState(() => String(max >= step ? step : 0));
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ActionResult | null>(null);

  const valid = qty > 0 && qty <= max && qty % step === 0;
  const set = (n: number) => { const c = Math.max(step, Math.min(max, n)); setQty(c); setText(String(c)); setResult(null); };
  const onType = (raw: string) => {
    setText(raw);
    setResult(null);
    const n = parseInt(raw, 10);
    setQty(Number.isFinite(n) ? n : 0);
  };

  const half = Math.floor(held / 2 / step) * step;
  const chips = [
    { label: `Min · ${step}`, n: step },
    ...(half > step && half < max ? [{ label: `Half · ${half}`, n: half }] : []),
    ...(max > step ? [{ label: `Max · ${max}`, n: max }] : []),
  ];

  const ltp = p.ltp ?? null;
  const pnl = ltp != null && valid ? (ltp - p.avg_buy_price) * qty : null;
  const sent = result?.kind === 'done' || result?.kind === 'requested';
  const blocked = sent ? null : blockedReason(p);

  async function submit() {
    if (!valid || busy || blocked) return;
    setBusy(true);
    const r = await onSubmit(qty);
    setBusy(false);
    setResult(r);
    navigator.vibrate?.(r.kind === 'error' ? [30, 40, 30] : 15);
  }

  let problem: string | null = null;
  if (max < step) problem = `Holding ${held} is less than one sellable step of ${step}. Use Close Position to exit it.`;
  else if (!valid && text !== '') {
    problem = qty > max ? `At most ${max} can be sold (${held} held${max < held ? `, in steps of ${step}` : ''}).`
      : qty % step !== 0 ? `Must be a multiple of ${step}.`
      : 'Enter a quantity greater than 0.';
  }

  const footer = sent ? (
    <button type="button" className={btnPlain} onClick={onClose}>Done</button>
  ) : (
    <button type="button" className={btnDanger} disabled={!valid || busy || blocked != null} onClick={submit}>
      {busy ? <><Loader2 size={16} className="animate-spin" /> Sending…</> : <>Sell {valid ? qty : ''} at market</>}
    </button>
  );

  return (
    <Sheet open onClose={onClose} busy={busy} title={<Title p={p} mode={mode} verb="Sell quantity" />} footer={footer}>
      <div className="space-y-4">
        <div>
          <label htmlFor="sell-qty" className="text-xs font-semibold text-on-surface-variant">
            Quantity{step > 1 && <> · steps of {step}</>}
          </label>
          <div className="mt-1.5 flex items-stretch gap-2">
            <button type="button" aria-label="Less" disabled={busy || sent || qty <= step} onClick={() => set(qty - step)}
              className="w-12 h-12 shrink-0 rounded-xl border border-outline-variant bg-surface-container-low flex items-center justify-center disabled:opacity-40">
              <Minus size={18} />
            </button>
            <input
              id="sell-qty"
              inputMode="numeric"
              enterKeyHint="done"
              pattern="[0-9]*"
              value={text}
              disabled={busy || sent || max < step}
              onChange={(e) => onType(e.target.value.replace(/[^0-9]/g, ''))}
              onKeyDown={(e) => { if (e.key === 'Enter') (e.target as HTMLInputElement).blur(); }}
              className="min-w-0 flex-1 h-12 rounded-xl border border-outline-variant bg-surface-container-lowest text-center text-xl font-bold font-mono-code tabular-nums focus:outline-none focus:ring-2 focus:ring-primary"
            />
            <button type="button" aria-label="More" disabled={busy || sent || qty >= max} onClick={() => set(qty + step)}
              className="w-12 h-12 shrink-0 rounded-xl border border-outline-variant bg-surface-container-low flex items-center justify-center disabled:opacity-40">
              <Plus size={18} />
            </button>
          </div>
          {chips.length > 1 && (
            <div className="mt-2 flex flex-wrap gap-2">
              {chips.map((c) => (
                <button key={c.label} type="button" disabled={busy || sent} onClick={() => set(c.n)}
                  className={`h-9 px-3 rounded-full border text-xs font-semibold tabular-nums ${
                    qty === c.n ? 'bg-primary/10 border-primary text-primary' : 'border-outline-variant text-on-surface-variant'
                  }`}>
                  {c.label}
                </button>
              ))}
            </div>
          )}
          {problem && (
            <p className="mt-2 flex items-start gap-1.5 text-xs text-error"><AlertTriangle size={14} className="shrink-0" />{problem}</p>
          )}
        </div>

        <Rows rows={[
          ['Order', 'Market sell'],
          ['Remaining after', valid ? `${held - qty} of ${held}` : '—'],
          ['Est. value at LTP', ltp != null && valid ? `₹${fmt(ltp * qty)}` : '—'],
          ['Est. P&L on this sale', pnl != null ? fmtInrSigned(pnl) : '—', pnl != null ? pnlClass(pnl) : undefined],
        ]} />

        {blocked && !busy && <ResultNote result={{ kind: 'error', message: blocked }} />}
        {result && <ResultNote result={result} />}

        <p className="text-xs text-on-surface-variant leading-relaxed">
          A market order fills at the broker's price, which can differ from the LTP above. The rest of the
          position keeps its stop and targets. Before charges.
        </p>
      </div>
    </Sheet>
  );
}

/**
 * Phone-only confirmation for Close Position: the whole holding at market. It
 * sends the same `POST /api/positions/:id/close` the button always did.
 */
export function ClosePositionSheet({ p, mode, onSubmit, onClose }: {
  p: MonitoredPosition;
  mode?: string;
  onSubmit: () => Promise<ActionResult>;
  onClose: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ActionResult | null>(null);
  const ltp = p.ltp ?? null;
  const pnl = ltp != null ? (ltp - p.avg_buy_price) * p.executed_qty : null;
  const sent = result?.kind === 'done' || result?.kind === 'requested';
  const blocked = sent ? null : blockedReason(p);

  async function submit() {
    if (busy || blocked) return;
    setBusy(true);
    const r = await onSubmit();
    setBusy(false);
    setResult(r);
    navigator.vibrate?.(r.kind === 'error' ? [30, 40, 30] : 15);
  }

  const footer = sent ? (
    <button type="button" className={btnPlain} onClick={onClose}>Done</button>
  ) : (
    <div className="grid grid-cols-[auto_1fr] gap-2">
      <button type="button" className={`${btnPlain} px-5`} disabled={busy} onClick={onClose}>Keep</button>
      <button type="button" className={btnDanger} disabled={busy || blocked != null} onClick={submit}>
        {busy ? <><Loader2 size={16} className="animate-spin" /> Sending…</> : <>Sell all {p.executed_qty} at market</>}
      </button>
    </div>
  );

  return (
    <Sheet open onClose={onClose} busy={busy} title={<Title p={p} mode={mode} verb="Close position" />} footer={footer}>
      <div className="space-y-4">
        <Rows rows={[
          ['Order', 'Market sell, whole holding'],
          ['Quantity', `${p.executed_qty}`],
          ['Stop loss now', `₹${fmt(p.current_sl)}`, 'text-error'],
          ['Est. value at LTP', ltp != null ? `₹${fmt(ltp * p.executed_qty)}` : '—'],
          ['Est. P&L since entry', pnl != null ? fmtInrSigned(pnl) : '—', pnl != null ? pnlClass(pnl) : undefined],
        ]} />

        {blocked && !busy && <ResultNote result={{ kind: 'error', message: blocked }} />}
        {result && <ResultNote result={result} />}

        <p className="text-xs text-on-surface-variant leading-relaxed">
          Exits the whole position at market and stops managing it. A market order fills at the broker's
          price, which can differ from the LTP above. Before charges.
        </p>
      </div>
    </Sheet>
  );
}
