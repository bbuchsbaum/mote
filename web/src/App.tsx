import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { MoteClient } from "./api/client";
import { useEventStream, useResource } from "./store";
import { Avatar, Modal } from "./components/ui";
import { IssuesView } from "./views/Issues";
import { DiscussionView } from "./views/Discussion";
import { MessagesView } from "./views/Messages";
import { TriageView } from "./views/Triage";

type View = "issues" | "discussion" | "messages" | "triage";

interface Route {
  view: View;
  arg: string | null;
  focus: string | null;
}

function parseHash(): Route {
  const raw = window.location.hash.replace(/^#\/?/, "");
  const [view, arg, focus] = raw.split("/").map((s) => (s ? decodeURIComponent(s) : ""));
  const known: View[] = ["issues", "discussion", "messages", "triage"];
  return {
    view: (known as string[]).includes(view) ? (view as View) : "issues",
    arg: arg || null,
    focus: focus || null,
  };
}

function useHashRoute(): [Route, (view: View, arg?: string | null, focus?: string | null) => void] {
  const [route, setRoute] = useState<Route>(parseHash);
  useEffect(() => {
    const onHash = () => setRoute(parseHash());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  const go = useCallback((view: View, arg?: string | null, focus?: string | null) => {
    const parts = [view, arg ?? "", focus ?? ""].map((p) => encodeURIComponent(p));
    while (parts.length > 1 && parts[parts.length - 1] === "") parts.pop();
    window.location.hash = `/${parts.join("/")}`;
  }, []);
  return [route, go];
}

/**
 * Mote's actor is per-process, but a browser is one process watching several
 * agents. So identity is an explicit control, and switching it changes what
 * "unread", "mine", and "inbox" mean. The console never writes
 * .mote/local/actor and never marks anything read on its own.
 */
function useActor(fallback: string): [string, (a: string) => void] {
  const [actor, setActor] = useState(() => localStorage.getItem("mote.actor") ?? fallback);
  const set = useCallback((a: string) => {
    localStorage.setItem("mote.actor", a);
    setActor(a);
  }, []);
  return [actor, set];
}

export function App({ client: makeClient }: { client: (getActor: () => string) => MoteClient }) {
  const [route, go] = useHashRoute();
  const [actor, setActor] = useActor("admin");
  const [switching, setSwitching] = useState(false);

  // The client reads the actor lazily, so switching identity needs no rebuild.
  const actorRef = useMemo(() => ({ current: actor }), []);
  actorRef.current = actor;
  const client = useMemo(() => makeClient(() => actorRef.current), [makeClient, actorRef]);
  useKeyboardNavigation(route, go, client);

  const connected = useEventStream(client);
  const { data: board } = useResource("board", actor, () => client.board());
  const { data: unrouted } = useResource("unrouted", actor, () => client.unrouted());

  const openBead = useCallback((id: string) => go("issues", id), [go]);
  const openPost = useCallback((topic: string, postId: string) => go("discussion", topic, postId), [go]);

  const triageCount = (unrouted?.posts.length ?? 0) + (unrouted?.topics.length ?? 0);
  const openCount = Object.entries(board?.status_counts ?? {})
    .filter(([s]) => s !== "closed")
    .reduce((n, [, v]) => n + (v ?? 0), 0);

  return (
    <div className="app">
      <nav className="rail" aria-label="Views">
        <div className="rail-brand">
          mote
          <small title={document.location.host}>console</small>
        </div>

        <RailItem label="Issues" shortcut="g i" count={openCount} on={route.view === "issues"} onClick={() => go("issues")} />
        <RailItem label="Discussion" shortcut="g d" count={board?.discussion_unread ?? 0} hot on={route.view === "discussion"} onClick={() => go("discussion")} />
        <RailItem label="Messages" shortcut="g m" count={board?.inbox_unacked ?? 0} hot on={route.view === "messages"} onClick={() => go("messages")} />
        <RailItem label="Triage" shortcut="g t" count={triageCount} amber on={route.view === "triage"} onClick={() => go("triage")} />

        <div className="rail-foot">
          <button className="actor-pick" onClick={() => setSwitching(true)} aria-label={`Switch actor. Acting as ${actor}`}>
            <Avatar actor={actor} />
            <span className="who"><span className="who-label">Acting as</span>{actor}</span>
            <span className="caret">▾</span>
          </button>
          <div className={`rail-hint ${connected ? "" : "off"}`}>
            {connected ? "connected · live" : "stream offline"}
          </div>
        </div>
      </nav>

      {route.view === "issues" && (
        <IssuesView
          client={client} actor={actor}
          selected={route.arg} onSelect={(id) => go("issues", id)}
          onOpenPost={(postId) => go("discussion", "", postId)}
        />
      )}
      {route.view === "discussion" && (
        <DiscussionView
          client={client} actor={actor}
          topic={route.arg} onSelectTopic={(t) => go("discussion", t)}
          focusPost={route.focus} onOpenBead={openBead}
        />
      )}
      {route.view === "messages" && (
        <MessagesView
          client={client} actor={actor}
          peer={route.arg} onSelectPeer={(p) => go("messages", p)}
          onOpenBead={openBead}
        />
      )}
      {route.view === "triage" && (
        <TriageView client={client} actor={actor} onOpenPost={openPost} />
      )}

      {switching && (
        <ActorSwitcher
          client={client} current={actor}
          onClose={() => setSwitching(false)}
          onPick={(a) => { setActor(a); setSwitching(false); }}
        />
      )}
    </div>
  );
}

function useKeyboardNavigation(
  route: Route,
  go: (view: View, arg?: string | null, focus?: string | null) => void,
  client: MoteClient,
) {
  const awaitingView = useRef(false);
  const sequenceTimer = useRef<number | null>(null);

  useEffect(() => {
    const isEditing = (target: EventTarget | null) =>
      target instanceof HTMLElement
      && !!target.closest("input, textarea, select, [contenteditable='true']");
    const focusSearch = () => {
      const focus = () => document.querySelector<HTMLElement>("[data-console-search]")?.focus();
      if (document.querySelector("[data-console-search]")) focus();
      else {
        go("issues");
        window.setTimeout(focus, 0);
      }
    };
    const move = (delta: number) => {
      const items = [...document.querySelectorAll<HTMLElement>("[data-nav-item]")];
      if (items.length === 0) return;
      const current = items.findIndex((item) => item === document.activeElement);
      const next = current < 0
        ? (delta > 0 ? 0 : items.length - 1)
        : (current + delta + items.length) % items.length;
      items[next].focus();
      items[next].scrollIntoView?.({ block: "nearest" });
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey || isEditing(event.target)) return;

      if (awaitingView.current) {
        awaitingView.current = false;
        if (sequenceTimer.current !== null) window.clearTimeout(sequenceTimer.current);
        sequenceTimer.current = null;
        const destination: Record<string, View> = {
          i: "issues", d: "discussion", m: "messages", t: "triage",
        };
        if (destination[event.key]) {
          event.preventDefault();
          go(destination[event.key]);
          return;
        }
      }

      if (event.key === "g") {
        event.preventDefault();
        awaitingView.current = true;
        sequenceTimer.current = window.setTimeout(() => {
          awaitingView.current = false;
          sequenceTimer.current = null;
        }, 800);
      } else if (event.key === "j" || event.key === "k") {
        event.preventDefault();
        move(event.key === "j" ? 1 : -1);
      } else if (event.key === "/") {
        event.preventDefault();
        focusSearch();
      } else if (event.key === "u") {
        event.preventDefault();
        void client.unread().then((posts) => {
          if (posts.length === 0) return;
          const current = route.focus
            ? posts.findIndex((post) => post.post_id === route.focus)
            : -1;
          const post = posts[(current + 1) % posts.length];
          go("discussion", post.topic, post.post_id);
        });
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      if (sequenceTimer.current !== null) window.clearTimeout(sequenceTimer.current);
    };
  }, [client, go, route.focus]);
}

function RailItem({
  label, shortcut, count, on, hot, amber, onClick,
}: { label: string; shortcut: string; count: number; on: boolean; hot?: boolean; amber?: boolean; onClick: () => void }) {
  return (
    <button className={`rail-item ${on ? "on" : ""}`} onClick={onClick}
      title={`${label} (${shortcut})`} aria-current={on ? "page" : undefined}>
      {label}
      {count > 0 && <span className={`badge ${hot ? "hot" : amber ? "amber" : ""}`}>{count}</span>}
    </button>
  );
}

function ActorSwitcher({
  client, current, onPick, onClose,
}: { client: MoteClient; current: string; onPick: (a: string) => void; onClose: () => void }) {
  const { data: actors } = useResource("actors", current, () => client.actors());
  const [custom, setCustom] = useState("");
  const names = [current, ...(actors ?? []).map((a) => a.actor).filter((a) => a !== current)];

  return (
    <Modal
      title="Act as"
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>Cancel</button>
          <button className="btn primary" disabled={!custom.trim()} onClick={() => onPick(custom.trim())}>Use this name</button>
        </>
      }
    >
      <div className="conflict" style={{ borderColor: "var(--rule-2)", background: "var(--surface-2)" }}>
        <div className="conflict-b">
          Every op you publish is attributed to this actor, and unread state and
          inbox are scoped to it. Nothing is ever marked read automatically.
        </div>
      </div>
      <div className="rows" style={{ border: "1px solid var(--rule)", borderRadius: 6, overflow: "hidden" }}>
        {names.map((a) => (
          <button key={a} className={`peer ${a === current ? "on" : ""}`} onClick={() => onPick(a)}>
            <Avatar actor={a} muted={a !== current} />
            <span className="peer-txt">
              <span className="peer-n">{a}{a === current && <span className="badge">current</span>}</span>
            </span>
          </button>
        ))}
      </div>
      <div className="formrow">
        <label>Or type a name</label>
        <input value={custom} onChange={(e) => setCustom(e.target.value)} placeholder="parser-session" />
      </div>
    </Modal>
  );
}
