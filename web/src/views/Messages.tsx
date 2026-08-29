import { useState } from "react";
import type { Message } from "../api/types";
import type { MoteClient } from "../api/client";
import { relativeTime, shortId, useResource, useWrite } from "../store";
import { Avatar, BeadPicker, Empty, Modal } from "../components/ui";

const KINDS = ["note", "request", "handoff", "blocked", "fyi"] as const;

export function MessagesView({
  client, actor, peer, onSelectPeer, onOpenBead,
}: {
  client: MoteClient; actor: string;
  peer: string | null; onSelectPeer: (p: string) => void;
  onOpenBead: (id: string) => void;
}) {
  const [error, setError] = useState<Error | null>(null);
  const { run, busy } = useWrite(setError);
  const [draft, setDraft] = useState("");
  const [kind, setKind] = useState<string>("note");
  const [attached, setAttached] = useState<string | null>(null);
  const [attaching, setAttaching] = useState(false);
  const [composingTo, setComposingTo] = useState(false);
  const [replyingTo, setReplyingTo] = useState<Message | null>(null);

  const { data: actors } = useResource("actors", actor, () => client.actors());
  const active = peer ?? actors?.[0]?.actor ?? null;
  const { data: convo } = useResource("dm", `${actor}:${active ?? "-"}`, () =>
    active ? client.dm(active) : Promise.resolve([]));
  const { data: beads } = useResource("beads", "picker", () => client.beads());

  const send = async () => {
    if (!draft.trim() || !active) return;
    const ok = await run(() => client.sendMessage(active, draft, kind, attached), ["dm", "actors", "board"]);
    if (ok) { setDraft(""); setAttached(null); setKind("note"); }
  };

  return (
    <div className="app" style={{ gridTemplateColumns: "220px minmax(0,1fr)" }}>
      <div className="list-col">
        <div className="pane-head" style={{ padding: "10px 13px", background: "var(--surface-2)" }}>
          <span className="pane-title" style={{ fontSize: 13 }}>Actors</span>
          <span className="spacer" />
          <button className="btn primary" onClick={() => setComposingTo(true)}>New</button>
        </div>
        <div className="scroll">
          {(actors ?? []).length === 0 && <Empty title="No other actors" />}
          {(actors ?? []).map((a) => (
            <button key={a.actor} data-nav-item className={`peer ${active === a.actor ? "on" : ""}`} onClick={() => onSelectPeer(a.actor)}>
              <Avatar actor={a.actor} muted={a.active_claims === 0} />
              <span className="peer-txt">
                <span className="peer-n">
                  {a.actor}
                  {a.inbox_unacked > 0 && <span className="badge hot">{a.inbox_unacked}</span>}
                </span>
                <span className="peer-p">{a.last_message?.body ?? "no messages yet"}</span>
              </span>
            </button>
          ))}
        </div>
      </div>

      <div className="pane">
        <div className="pane-head">
          {active && <Avatar actor={active} />}
          <span className="pane-title">{active ?? "Messages"}</span>
          <span className="spacer" />
          <span className="mono-id">{convo?.length ?? 0} messages</span>
        </div>

        {error && <div className="err">{error.message}</div>}

        <div className="scroll">
          {!convo || convo.length === 0 ? (
            <Empty title="No messages yet">Say something below.</Empty>
          ) : (
            <div className="convo">
              {convo.map((m) => (
                <MessageBubble
                  key={m.msg_id} m={m} actor={actor} busy={busy}
                  onOpenBead={onOpenBead}
                  onAck={() => void run(() => client.ackMessage(m.msg_id), ["dm", "actors", "board"])}
                  onResolve={() => void run(() => client.resolveRequest(m.msg_id), ["dm", "actors"])}
                  onReply={() => setReplyingTo(m)}
                />
              ))}
            </div>
          )}
        </div>

        <div className="composer">
          {attached && (
            <span className="replyto">
              bead {shortId(attached)}
              <button className="btn link" style={{ color: "inherit" }} onClick={() => setAttached(null)} aria-label="Detach bead">✕</button>
            </span>
          )}
          <textarea
            value={draft} onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) void send(); }}
            placeholder={active ? `Message ${active}…` : "Pick an actor first"}
            aria-label="Message body"
          />
          <div className="composer-foot">
            {KINDS.map((k) => (
              <button key={k} className={`chip ${kind === k ? "on" : ""}`} onClick={() => setKind(k)}>{k}</button>
            ))}
            <span className="spacer" />
            <button className="btn" onClick={() => setAttaching(true)}>Attach bead</button>
            <button className="btn primary" disabled={busy || !draft.trim() || !active} onClick={() => void send()}>Send</button>
          </div>
        </div>
      </div>

      {attaching && (
        <BeadPicker title="Attach a bead" beads={beads ?? []} onClose={() => setAttaching(false)}
          onPick={(id) => { setAttached(id); setAttaching(false); }} />
      )}

      {composingTo && (
        <NewPeerModal onClose={() => setComposingTo(false)} onPick={(name) => { onSelectPeer(name); setComposingTo(false); }} />
      )}

      {replyingTo && (
        <ReplyModal
          busy={busy} request={replyingTo} onClose={() => setReplyingTo(null)}
          onSend={async (body, replyKind) => {
            const ok = await run(() => client.replyMessage(replyingTo.msg_id, body, replyKind), ["dm", "actors", "board"]);
            if (ok) setReplyingTo(null);
          }}
        />
      )}
    </div>
  );
}

function MessageBubble({
  m, actor, busy, onOpenBead, onAck, onResolve, onReply,
}: {
  m: Message; actor: string; busy: boolean;
  onOpenBead: (id: string) => void; onAck: () => void; onResolve: () => void; onReply: () => void;
}) {
  const out = m.direction === "out";
  const isRoot = m.request_state !== null;
  const iSent = m.from === actor;

  return (
    <>
      <div className={`bub ${out ? "out" : ""}`}>
        <div className="bub-head">
          <span className="post-who">{m.from}</span>
          {m.msg_kind !== "note" && <span className={`kind ${m.msg_kind === "fyi" ? "plain" : ""}`}>{m.msg_kind}</span>}
          {m.entity && <button className="beadref" onClick={() => onOpenBead(m.entity!)}>{shortId(m.entity)}</button>}
          <span className="post-when">{relativeTime(m.sent_ts)}</span>
        </div>
        <div className="bub-txt">{m.body}</div>
      </div>

      {isRoot && (
        <div className="lifecycle">
          {/* Acknowledgement and response are different things; show both. */}
          <span className="step done">✓ sent</span>
          <span className="step">→</span>
          <span className={m.ack_ts ? "step done" : "step"}>{m.ack_ts ? `✓ acked ${relativeTime(m.ack_ts)}` : "not acked"}</span>
          <span className="step">→</span>
          <span className={m.request_state === "open" ? "step now" : "step done"}>
            {m.request_state === "open" ? "● awaiting response" : `✓ ${m.request_state}`}
          </span>
          <span className="spacer" />
          {!m.ack_ts && !iSent && <button className="btn" disabled={busy} onClick={onAck}>Ack</button>}
          {m.request_state === "open" && !iSent && <button className="btn primary" disabled={busy} onClick={onReply}>Respond</button>}
          {(m.request_state === "responded" || m.request_state === "declined") && iSent && (
            <button className="btn" disabled={busy} onClick={onResolve}>Resolve</button>
          )}
        </div>
      )}
    </>
  );
}

function ReplyModal({
  request, onSend, onClose, busy,
}: { request: Message; onSend: (body: string, kind: "response" | "decline") => void; onClose: () => void; busy: boolean }) {
  const [body, setBody] = useState("");
  const [kind, setKind] = useState<"response" | "decline">("response");
  return (
    <Modal
      title="Answer request"
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>Cancel</button>
          <button className="btn primary" disabled={busy || !body.trim()} onClick={() => onSend(body, kind)}>
            Send {kind}
          </button>
        </>
      }
    >
      <div className="conflict" style={{ borderColor: "var(--rule-2)", background: "var(--surface-2)" }}>
        <div className="conflict-t" style={{ color: "var(--ink-2)" }}>{request.from} asked</div>
        <div className="conflict-b">{request.body}</div>
      </div>
      <div className="formrow">
        <label>Kind</label>
        <div style={{ display: "flex", gap: 7 }}>
          <button className={`chip ${kind === "response" ? "on" : ""}`} onClick={() => setKind("response")}>response</button>
          <button className={`chip ${kind === "decline" ? "on" : ""}`} onClick={() => setKind("decline")}>decline</button>
        </div>
      </div>
      <div className="formrow">
        <label>Body</label>
        <textarea value={body} onChange={(e) => setBody(e.target.value)} autoFocus />
      </div>
    </Modal>
  );
}

function NewPeerModal({ onPick, onClose }: { onPick: (name: string) => void; onClose: () => void }) {
  const [name, setName] = useState("");
  return (
    <Modal
      title="Message an actor"
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>Cancel</button>
          <button className="btn primary" disabled={!name.trim()} onClick={() => onPick(name.trim())}>Open</button>
        </>
      }
    >
      <div className="formrow">
        <label>Actor name</label>
        <input value={name} onChange={(e) => setName(e.target.value)} autoFocus placeholder="parser-session" />
      </div>
    </Modal>
  );
}
