import { useEffect, useRef, useState, type ReactNode } from "react";
import type { BeadRow, RouteState, Status } from "../api/types";
import { ConflictError } from "../api/client";
import { initials, shortId } from "../store";

export function Avatar({ actor, muted }: { actor: string; muted?: boolean }) {
  return <span className={muted ? "avatar muted" : "avatar"}>{initials(actor)}</span>;
}

export function StatusPill({ status }: { status: Status }) {
  return <span className={`pill ${status}`}>{status}</span>;
}

const ROUTE_LABEL: Record<RouteState, string> = {
  open: "", needs_bead: "needs bead", routed: "routed", resolved: "resolved",
};

export function RouteChip({ state, issues }: { state: RouteState; issues?: string[] }) {
  if (state === "open") return null;
  const label = state === "routed" && issues?.length
    ? `${issues.length} bead${issues.length > 1 ? "s" : ""}`
    : ROUTE_LABEL[state];
  return <span className={`route ${state}`}>{label}</span>;
}

export function Empty({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="empty">
      <b>{title}</b>
      {children}
    </div>
  );
}

export function Modal({
  title, children, onClose, footer,
}: { title: string; children: ReactNode; onClose: () => void; footer: ReactNode }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="backdrop" onMouseDown={(e) => { if (e.target === e.currentTarget) onClose(); }}>
      <div className="modal" role="dialog" aria-modal="true" aria-label={title}>
        <div className="modal-head">{title}</div>
        <div className="modal-body">{children}</div>
        <div className="modal-foot">{footer}</div>
      </div>
    </div>
  );
}

/**
 * A 409 means the op is durably recorded in `ops/` as a rejected intent — the
 * edit was not lost and was not forced. Three honest exits, no automatic retry:
 * the point of field clocks is that a person decides who wins.
 */
export function ConflictDialog({
  error, field, yourValue, onDiscard, onRetry,
}: {
  error: ConflictError; field: string; yourValue: string;
  onDiscard: () => void; onRetry: () => void;
}) {
  const current = error.current[field];
  return (
    <Modal
      title={`Edit ${field}`}
      onClose={onDiscard}
      footer={
        <>
          <button className="btn" onClick={onDiscard}>Discard</button>
          <button className="btn" onClick={onDiscard}>Take theirs</button>
          <button className="btn primary" onClick={onRetry}>Retry on current</button>
        </>
      }
    >
      <div className="conflict">
        <div className="conflict-t">The reducer rejected this patch</div>
        <div className="conflict-b">
          {error.reason}. {current !== undefined && <>The field is now <b>{String(current)}</b>. </>}
          Your change was recorded as a rejected intent under{" "}
          <code>{shortId(error.opId)}</code>, not applied.
        </div>
      </div>
      <div className="formrow">
        <label>Your value</label>
        <input value={yourValue} readOnly />
      </div>
    </Modal>
  );
}

/** Search-and-pick over open beads, used by Link bead and Attach bead. */
export function BeadPicker({
  beads, onPick, onClose, title,
}: { beads: BeadRow[]; onPick: (id: string) => void; onClose: () => void; title: string }) {
  const [q, setQ] = useState("");
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => ref.current?.focus(), []);
  const hits = beads
    .filter((b) => `${b.title} ${b.id} ${b.tags.join(" ")}`.toLowerCase().includes(q.toLowerCase()))
    .slice(0, 40);

  return (
    <Modal title={title} onClose={onClose} footer={<button className="btn" onClick={onClose}>Cancel</button>}>
      <div className="formrow">
        <label>Search</label>
        <input ref={ref} value={q} onChange={(e) => setQ(e.target.value)} placeholder="title, tag, or id" />
      </div>
      <div className="rows" style={{ maxHeight: 300, overflowY: "auto", border: "1px solid var(--rule)", borderRadius: 6 }}>
        {hits.length === 0 && <Empty title="No match" />}
        {hits.map((b) => (
          <button key={b.id} className="row" onClick={() => onPick(b.id)}>
            <span className={`pri p${b.priority}`} />
            <span className="row-body">
              <span className="row-title">{b.title}</span>
              <span className="row-meta">
                <StatusPill status={b.status} />
                <span className="mono-id">{shortId(b.id)}</span>
              </span>
            </span>
            <span className="row-right" />
          </button>
        ))}
      </div>
    </Modal>
  );
}

export function Toast({ message }: { message: string | null }) {
  if (!message) return null;
  return <div className="toast" role="status">{message}</div>;
}
