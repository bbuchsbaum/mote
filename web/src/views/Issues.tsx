import { useMemo, useState } from "react";
import type { BeadDetail, BeadQuery, NoteKind, ScalarField, Status } from "../api/types";
import { ConflictError, type MoteClient } from "../api/client";
import { leaseRemaining, relativeTime, shortId, useResource, useWrite } from "../store";
import { Avatar, ConflictDialog, Empty, Modal, StatusPill } from "../components/ui";

const STATUSES: Status[] = ["open", "doing", "blocked", "review", "closed"];

export function IssuesView({
  client, actor, selected, onSelect, onOpenPost,
}: {
  client: MoteClient; actor: string;
  selected: string | null; onSelect: (id: string | null) => void;
  onOpenPost: (postId: string) => void;
}) {
  const [status, setStatus] = useState<Status | null>(null);
  const [readyOnly, setReadyOnly] = useState(false);
  const [mine, setMine] = useState(false);
  const [q, setQ] = useState("");
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<Error | null>(null);
  const { run, busy } = useWrite(setError);

  const query: BeadQuery = useMemo(
    () => ({ status: status ?? undefined, ready: readyOnly || undefined, all: status === "closed" || undefined }),
    [status, readyOnly],
  );
  const key = JSON.stringify(query);
  const { data: beads } = useResource("beads", key, () => client.beads(query));
  const { data: board } = useResource("board", actor, () => client.board());

  const claimOf = (id: string) => board?.active_claims.find((c) => c.id === id) ?? null;

  const visible = (beads ?? [])
    .filter((b) => !q || `${b.title} ${b.id} ${b.tags.join(" ")}`.toLowerCase().includes(q.toLowerCase()))
    .filter((b) => !mine || claimOf(b.id)?.claimed_by === actor);

  return (
    <div className="app" style={{ gridTemplateColumns: selected ? "minmax(0,1fr) 300px" : "minmax(0,1fr)" }}>
      <div className="pane">
        <div className="pane-head">
          <span className="pane-title">Issues</span>
          <span className="mono-id">{visible.length} shown</span>
          <span className="spacer" />
          <button className="btn primary" onClick={() => setCreating(true)}>New bead</button>
        </div>

        <div className="filters">
          {STATUSES.map((s) => (
            <button key={s} className={`chip ${status === s ? "on" : ""}`}
              onClick={() => setStatus(status === s ? null : s)}>{s}</button>
          ))}
          <button className={`chip ${readyOnly ? "on" : ""}`} onClick={() => setReadyOnly(!readyOnly)}>ready only</button>
          <button className={`chip ${mine ? "on" : ""}`} onClick={() => setMine(!mine)}>mine</button>
          <input className="search" value={q} onChange={(e) => setQ(e.target.value)}
            placeholder="filter title, tag, id…" aria-label="Filter beads" data-console-search />
        </div>

        {error && <div className="err">{error.message}</div>}

        <div className="scroll">
          {visible.length === 0 ? (
            <Empty title="Nothing here">No bead matches these filters.</Empty>
          ) : (
            <div className="rows">
              {visible.map((b) => {
                const claim = claimOf(b.id);
                const lease = claim ? leaseRemaining(claim.lease_until_ts) : null;
                return (
                  <button key={b.id} data-nav-item className={`row ${selected === b.id ? "sel" : ""}`}
                    onClick={() => onSelect(b.id === selected ? null : b.id)}>
                    <span className={`pri p${b.priority}`} />
                    <span className="row-body">
                      <span className="row-title">{b.title}</span>
                      <span className="row-meta">
                        <StatusPill status={b.status} />
                        {b.tags.map((t) => <span key={t} className="tagchip">{t}</span>)}
                        {claim && lease && (
                          <span className={`lease ${lease.expiring ? "expiring" : ""}`}>
                            ◷ {claim.claimed_by} · {lease.label}
                          </span>
                        )}
                      </span>
                    </span>
                    <span className="row-right"><span className="mono-id">{shortId(b.id)}</span></span>
                  </button>
                );
              })}
            </div>
          )}
        </div>
      </div>

      {selected && (
        <BeadDetailPane
          client={client} actor={actor} id={selected}
          onClose={() => onSelect(null)} onOpenPost={onOpenPost}
          onError={setError} run={run} busy={busy}
        />
      )}

      {creating && (
        <NewBeadModal
          busy={busy}
          onClose={() => setCreating(false)}
          onCreate={async (input) => {
            const ok = await run(() => client.createBead(input), ["beads", "board"]);
            if (ok) setCreating(false);
          }}
        />
      )}
    </div>
  );
}

/* ------------------------------------------------------------------ */

function BeadDetailPane({
  client, actor, id, onClose, onOpenPost, onError, run, busy,
}: {
  client: MoteClient; actor: string; id: string; onClose: () => void;
  onOpenPost: (postId: string) => void; onError: (e: Error) => void;
  run: (fn: () => Promise<unknown>, slices: Parameters<ReturnType<typeof useWrite>["run"]>[1]) => Promise<boolean>;
  busy: boolean;
}) {
  const { data: bead } = useResource("bead", id, () => client.bead(id));
  const { data: history } = useResource("bead", `${id}:history`, () => client.history(id));
  const { data: board } = useResource("board", actor, () => client.board());
  const [conflict, setConflict] = useState<{ error: ConflictError; field: ScalarField; value: string } | null>(null);
  const [noteText, setNoteText] = useState("");
  const [noteKind, setNoteKind] = useState<NoteKind>("progress");

  if (!bead) return <div className="aside"><div className="aside-body"><Empty title="Loading…" /></div></div>;

  const claim = board?.active_claims.find((c) => c.id === id) ?? null;
  const mineClaim = claim?.claimed_by === actor;

  const patch = async (field: ScalarField, value: string | number, detail: BeadDetail) => {
    try {
      await client.patchBead(id, { [field]: value }, { [field]: detail.clock[field] });
      return true;
    } catch (e) {
      if (e instanceof ConflictError) {
        setConflict({ error: e, field, value: String(value) });
        return false;
      }
      onError(e as Error);
      return false;
    }
  };

  return (
    <div className="aside">
      <div className="pane-head" style={{ background: "var(--surface-2)" }}>
        <span className="pane-title" style={{ fontSize: 13 }}>Detail</span>
        <span className="spacer" />
        <button className="btn" onClick={onClose} aria-label="Close detail">✕</button>
      </div>

      <div className="aside-body">
        <div>
          <div className="row-title" style={{ fontSize: 14, marginBottom: 8 }}>{bead.title}</div>
          <div className="row-meta" style={{ marginBottom: 10 }}>
            <StatusPill status={bead.status} />
            <span className="mono-id">{shortId(bead.id)}</span>
            {bead.ready && <span className="tagchip">ready</span>}
          </div>
          {bead.body && <div className="body-text">{bead.body}</div>}
        </div>

        <div>
          <h5>Actions</h5>
          <div className="card" style={{ display: "flex", flexWrap: "wrap", gap: 7 }}>
            <select
              aria-label="Status"
              value={bead.status}
              disabled={busy}
              onChange={(e) => { void patch("status", e.target.value, bead); }}
              style={{ border: "1px solid var(--rule-2)", borderRadius: 5, background: "var(--surface-2)", padding: "3px 7px", fontSize: 11.5 }}
            >
              {STATUSES.map((s) => <option key={s} value={s}>{s}</option>)}
            </select>
            {mineClaim ? (
              <button className="btn" disabled={busy} onClick={() => void run(() => client.release(id), ["board", "beads"])}>Release</button>
            ) : (
              <button className="btn" disabled={busy} onClick={() => void run(() => client.claim(id, 1800), ["board", "beads"])}>Claim 30m</button>
            )}
            {bead.status !== "closed" && (
              <button className="btn danger" disabled={busy}
                onClick={() => void run(() => client.close(id), ["board", "beads", "bead"])}>Close</button>
            )}
          </div>
        </div>

        <div>
          <h5>Lease</h5>
          <div className="card">
            <div className="kv"><span>claim</span><b>{claim ? `${claim.claimed_by} · ${leaseRemaining(claim.lease_until_ts).label}` : "none"}</b></div>
            <div className="kv"><span>priority</span><b>{bead.priority}</b></div>
            <div className="kv"><span>created</span><b>{relativeTime(bead.created_at)}</b></div>
            {bead.deps.length > 0 && <div className="kv"><span>blocked by</span><b>{bead.deps.length}</b></div>}
          </div>
        </div>

        {bead.discussion_sources.posts.length > 0 && (
          <div>
            <h5>From discussion</h5>
            <div className="card" style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              {bead.discussion_sources.posts.map((p) => (
                <button key={p.post_id} className="src" style={{ textAlign: "left" }} onClick={() => onOpenPost(p.post_id)}>
                  {shortId(p.post_id)} →
                </button>
              ))}
            </div>
          </div>
        )}

        <div>
          <h5>Notes</h5>
          <div className="card" style={{ display: "flex", flexDirection: "column", gap: 9 }}>
            {bead.notes.length === 0 && <span style={{ fontSize: 11, color: "var(--ink-3)" }}>No notes yet.</span>}
            {bead.notes.map((n) => (
              <div key={n.op_id} style={{ fontSize: 11.5 }}>
                <span className="mono-id">{n.actor} · {n.kind} · {relativeTime(n.ts)}</span>
                <div style={{ fontFamily: "var(--serif)", fontSize: 12.5, lineHeight: 1.5 }}>{n.text}</div>
              </div>
            ))}
            <div style={{ display: "flex", gap: 6, flexDirection: "column" }}>
              <textarea className="field" style={{ minHeight: 44, fontSize: 12 }} value={noteText}
                onChange={(e) => setNoteText(e.target.value)} placeholder="Add a note…" aria-label="Note text" />
              <div style={{ display: "flex", gap: 6 }}>
                <select value={noteKind} onChange={(e) => setNoteKind(e.target.value as NoteKind)} aria-label="Note kind"
                  style={{ border: "1px solid var(--rule-2)", borderRadius: 5, background: "var(--surface-2)", padding: "3px 7px", fontSize: 11 }}>
                  {(["note", "progress", "decision", "handoff", "blocker"] as NoteKind[]).map((k) => <option key={k}>{k}</option>)}
                </select>
                <button className="btn primary" disabled={busy || !noteText.trim()}
                  onClick={async () => {
                    const ok = await run(() => client.addNote(id, noteKind, noteText), ["bead"]);
                    if (ok) setNoteText("");
                  }}>Add note</button>
              </div>
            </div>
          </div>
        </div>

        <div>
          <h5>History</h5>
          <div className="tl">
            {(history ?? []).map((h) => (
              <div key={h.op_id} className={`tl-item ${h.accepted ? "acc" : "rej"}`}>
                <span className="t">{relativeTime(h.ts)} · {h.actor}</span>
                <b>{h.accepted ? h.kind : `${h.kind} rejected`}</b>
                {h.reason && <span className="why"> — {h.reason}</span>}
              </div>
            ))}
          </div>
        </div>
      </div>

      {conflict && (
        <ConflictDialog
          error={conflict.error} field={conflict.field} yourValue={conflict.value}
          onDiscard={() => setConflict(null)}
          onRetry={async () => {
            const fresh = await client.bead(id);
            const ok = await patch(conflict.field, conflict.value, fresh);
            if (ok) setConflict(null);
          }}
        />
      )}
    </div>
  );
}

/* ------------------------------------------------------------------ */

export function NewBeadModal({
  onCreate, onClose, busy, initial,
}: {
  onCreate: (input: { title: string; body: string; priority: number; tags: string[] }) => void;
  onClose: () => void; busy: boolean;
  initial?: { title?: string; body?: string };
}) {
  const [title, setTitle] = useState(initial?.title ?? "");
  const [body, setBody] = useState(initial?.body ?? "");
  const [priority, setPriority] = useState(2);
  const [tags, setTags] = useState("");

  return (
    <Modal
      title={initial ? "Promote to bead" : "New bead"}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>Cancel</button>
          <button className="btn primary" disabled={busy || !title.trim()}
            onClick={() => onCreate({
              title: title.trim(), body,
              priority,
              tags: tags.split(/[\s,]+/).filter(Boolean),
            })}>
            {initial ? "Create and route" : "Create bead"}
          </button>
        </>
      }
    >
      <div className="formrow">
        <label>Title</label>
        <input value={title} onChange={(e) => setTitle(e.target.value)} autoFocus
          placeholder="What needs doing" />
      </div>
      <div className="two">
        <div className="formrow">
          <label>Priority</label>
          <select value={priority} onChange={(e) => setPriority(Number(e.target.value))}>
            {[0, 1, 2, 3].map((p) => <option key={p} value={p}>{p}{p === 0 ? " (highest)" : ""}</option>)}
          </select>
        </div>
        <div className="formrow">
          <label>Tags</label>
          <input value={tags} onChange={(e) => setTags(e.target.value)} placeholder="space separated" />
        </div>
      </div>
      <div className="formrow">
        <label>Body</label>
        <textarea value={body} onChange={(e) => setBody(e.target.value)}
          placeholder="Newlines and backticks survive — this is sent as JSON, not an argv string." />
      </div>
    </Modal>
  );
}

export function ActorBadge({ actor }: { actor: string }) {
  return <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}><Avatar actor={actor} /> {actor}</span>;
}
