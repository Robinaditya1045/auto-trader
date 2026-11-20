import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { X } from 'lucide-react';

const FOCUSABLE = 'button:not([disabled]), [href], input:not([disabled]), select, textarea, [tabindex]:not([tabindex="-1"])';

/**
 * A bottom sheet on a phone, a centred dialog from `md` up — for an order
 * confirmation, never a new page that loses the position behind it.
 *
 * The scrim, Escape, the close button and a downward drag on the handle all
 * close it, except while `busy` (an order is on its way: closing then would
 * hide the result). Focus stays inside while it is open and returns to
 * whatever opened it. `footer` is pinned under the scrolling body so the
 * primary action stays in reach above the home indicator.
 */
export function Sheet({ open, onClose, title, footer, children, busy = false }: {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  footer?: ReactNode;
  children: ReactNode;
  busy?: boolean;
}) {
  const panel = useRef<HTMLDivElement>(null);
  const dragFrom = useRef<number | null>(null);
  const [drag, setDrag] = useState(0);
  const close = useRef(onClose);
  const busyRef = useRef(busy);
  useEffect(() => { close.current = onClose; busyRef.current = busy; }, [onClose, busy]);

  const tryClose = () => { if (!busyRef.current) close.current(); };

  useEffect(() => {
    if (!open) return;
    const opener = document.activeElement as HTMLElement | null;
    panel.current?.focus();
    function onKey(e: KeyboardEvent) {
      if (e.key === 'Escape') { e.preventDefault(); if (!busyRef.current) close.current(); }
      if (e.key !== 'Tab' || !panel.current) return;
      const items = [...panel.current.querySelectorAll<HTMLElement>(FOCUSABLE)];
      if (items.length === 0) return;
      const first = items[0], last = items[items.length - 1];
      if (e.shiftKey && (document.activeElement === first || document.activeElement === panel.current)) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    }
    document.addEventListener('keydown', onKey);
    return () => { document.removeEventListener('keydown', onKey); opener?.focus?.(); };
  }, [open]);

  if (!open) return null;
  return (
    <div className="fixed inset-0 z-[120]">
      <div className="absolute inset-0 bg-black/45" onClick={tryClose} aria-hidden />
      <div
        ref={panel}
        role="dialog"
        aria-modal="true"
        aria-labelledby="sheet-title"
        tabIndex={-1}
        style={drag > 0 ? { transform: `translateY(${drag}px)` } : undefined}
        className={[
          'absolute flex flex-col bg-surface-container-lowest border-outline-variant shadow-2xl outline-none text-on-surface',
          'inset-x-0 bottom-0 max-h-[92dvh] rounded-t-2xl border-t sheet-in',
          'md:inset-x-auto md:bottom-auto md:left-1/2 md:top-1/2 md:-translate-x-1/2 md:-translate-y-1/2 md:w-[440px] md:max-h-[85dvh] md:rounded-2xl md:border',
        ].join(' ')}
      >
        <div
          className="md:hidden flex justify-center pt-2.5 pb-1 touch-none cursor-grab"
          onPointerDown={(e) => { dragFrom.current = e.clientY; (e.target as HTMLElement).setPointerCapture?.(e.pointerId); }}
          onPointerMove={(e) => { if (dragFrom.current != null) setDrag(Math.max(0, e.clientY - dragFrom.current)); }}
          onPointerUp={() => { const d = drag; dragFrom.current = null; setDrag(0); if (d > 80) tryClose(); }}
          onPointerCancel={() => { dragFrom.current = null; setDrag(0); }}
        >
          <span className="w-10 h-1 rounded-full bg-outline-variant" aria-hidden />
        </div>
        <header className="flex items-start gap-2 px-5 pt-1 pb-3 md:pt-4 border-b border-outline-variant shrink-0">
          <div id="sheet-title" className="flex-1 min-w-0">{title}</div>
          <button
            type="button"
            onClick={tryClose}
            disabled={busy}
            aria-label="Close"
            className="flex items-center justify-center w-10 h-10 -mr-2 -mt-1 rounded-full text-on-surface-variant hover:bg-surface-container disabled:opacity-40"
          >
            <X size={18} />
          </button>
        </header>
        <div className="flex-1 min-h-0 overflow-y-auto overscroll-contain px-5 py-4">{children}</div>
        {footer && (
          <footer className="shrink-0 border-t border-outline-variant px-5 pt-3 pb-safe">{footer}</footer>
        )}
      </div>
    </div>
  );
}
