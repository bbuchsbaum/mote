import { useEffect, useMemo, useRef, useState } from "react";
import type { Post } from "../api/types";
import type { MoteClient } from "../api/client";
import { relativeTime, shortId, useResource, useWrite } from "../store";
import { BeadPicker, Empty, Modal, RouteChip } from "../components/ui";
import { NewBeadModal } from "./Issues";

export function DiscussionView({
  client, actor, topic, onSelectTopic, focusPost, onOpenBead,
}: {
  client: MoteClient; actor: string;
  topic: string | null; onSelectTopic: (t: string) => void;
  focusPost: string | null; onOpenBead: (id: string) => void;
}) {
  const [error, setError] = useState<Error | null>(null);
  const { run, busy } = useWrite(setError);
  const [replyTo, setReplyTo] = useState<Post | null>(null);
  const [draft, setDraft] = useState("");
  const [creatingTopic, setCreatingTopic] = useState(false);
  const [promoting, setPromoting] = useState<Post | null>(null);
  const [linking, setLinking] = useState<Post | null>(null);

  const { data: topics } = useResource("topics", actor, () => client.topics());
  const active = topic ?? topics?.[0]?.topic ?? null;
  const { data: posts } = useResource("posts", active ?? "-", () =>
    active ? client.posts(active) : Promise.resolve([]));
  const { data: beads } = useResource("beads", "picker", () => client.beads());

  const focusRef = useRef<HTMLDivElement>(null);
  useEffect(() => { if (focusPost) focusRef.current?.scrollIntoView({ block: "center" }); }, [focusPost, posts]);

  // Indent on the reply graph, exactly as `discuss thread` reports depth.
  const ordered = useMemo(() => nest(posts ?? []), [posts]);
  const activeTopic = topics?.find((t) => t.topic === active) ?? null;

  const submit = async () => {
    if (!draft.trim() || !active) return;
    const ok = await run(() => client.post(active, draft, replyTo?.post_id ?? null), ["posts", "topics", "board"]);
    if (ok) { setDraft(""); setReplyTo(null); }
  };

  return (
    <div className="app" style={{ gridTemplateColumns: "230px minmax(0,1fr)" }}>
      <div className="list-col">
        <div className="pane-head" style={{ padding: "10px 13px", background: "var(--surface-2)" }}>
          <span className="pane-title" style={{ fontSize: 13 }}>Topics</span>
          <span className="spacer" />
          <button className="btn primary" onClick={() => setCreatingTopic(true)}>New</button>
        </div>
        <div className="scroll">
          {(topics ?? []).map((t) => (
            <button key={t.topic} className={`topic ${active === t.topic ? "on" : ""}`}
              onClick={() => onSelectTopic(t.topic)}>
              <span className="topic-t">
                {(t.unread ?? 0) > 0 && <span className="unread-dot" aria-label="unread" />}
                {t.title || t.topic}
              </span>
              <span className="topic-m">
                <span>{t.post_count} posts</span>
                <span>{relativeTime(t.last_activity_ts)}</span>
                <RouteChip state={t.route_state} issues={t.issues} />
              </span>
            </button>
          ))}
        </div>
      </div>

      <div className="pane">
        <div className="pane-head">
          <span className="pane-title">{activeTopic?.title ?? active ?? "Discussion"}</span>
          {activeTopic && <RouteChip state={activeTopic.route_state} issues={activeTopic.issues} />}
          <span className="spacer" />
          <button className="btn" disabled={busy || !active}
            onClick={() => void run(() => client.markRead(active ?? undefined), ["topics", "board"])}>
            Mark all read
          </button>
        </div>

        {error && <div className="err">{error.message}</div>}

        <div className="scroll">
          {ordered.length === 0 ? (
            <Empty title="No posts yet">Start the thread below.</Empty>
          ) : (
            <div className="thread">
              {ordered.map(({ post, depth }) => (
                <div
                  key={post.post_id}
                  ref={post.post_id === focusPost ? focusRef : undefined}
                  className={`post ${post.sticky ? "sticky" : ""} ${post.post_id === focusPost ? "unread" : ""}`}
                  style={{ marginLeft: Math.min(depth, 3) * 26 }}
                >
                  <div className="post-head">
                    <span className="post-who">{post.from}</span>
                    {post.post_kind !== "post" && <span className="kind">{post.post_kind}</span>}
                    <span className="post-when">{relativeTime(post.sent_ts)}</span>
                    <span className="mono-id">{shortId(post.post_id)}</span>
                    <RouteChip state={post.route_state} issues={post.issues} />
                    {post.issues.map((id) => (
                      <button key={id} className="beadref" onClick={() => onOpenBead(id)}>{shortId(id)}</button>
                    ))}
                  </div>
                  <div className="post-body">{post.body}</div>
                  <div className="post-acts">
                    <button className="btn link go" onClick={() => setReplyTo(post)}>Reply</button>
                    <button className="btn link" onClick={() => setPromoting(post)}>Promote to bead</button>
                    <button className="btn link" onClick={() => setLinking(post)}>Link bead</button>
                    {post.route_state !== "needs_bead" && (
                      <button className="btn link" disabled={busy}
                        onClick={() => void run(() => client.needsBead(post.post_id), ["posts", "unrouted"])}>
                        Needs bead
                      </button>
                    )}
                    {post.route_state !== "resolved" && (
                      <button className="btn link" disabled={busy}
                        onClick={() => void run(() => client.resolvePost(post.post_id), ["posts", "unrouted"])}>
                        Resolve
                      </button>
                    )}
                    <button className="btn link" disabled={busy}
                      onClick={() => void run(() => client.setSticky(post.post_id, !post.sticky), ["posts"])}>
                      {post.sticky ? "Unsticky" : "Sticky"}
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>

        <div className="composer">
          {replyTo && (
            <span className="replyto">
              replying to {shortId(replyTo.post_id)} · {replyTo.from}
              <button className="btn link" style={{ color: "inherit" }} onClick={() => setReplyTo(null)} aria-label="Clear reply target">✕</button>
            </span>
          )}
          <textarea
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) void submit(); }}
            placeholder={replyTo ? "Write a reply…" : "Post to this topic…"}
            aria-label="Post body"
          />
          <div className="composer-foot">
            <span className="mono-id">⌘↵ to post</span>
            <span className="spacer" />
            <button className="btn primary" disabled={busy || !draft.trim() || !active} onClick={() => void submit()}>
              {replyTo ? "Post reply" : "Post"}
            </button>
          </div>
        </div>
      </div>

      {creatingTopic && (
        <NewTopicModal
          busy={busy}
          onClose={() => setCreatingTopic(false)}
          onCreate={async (name, title, body) => {
            const ok = await run(() => client.createTopic(name, title, body), ["topics"]);
            if (ok) { setCreatingTopic(false); onSelectTopic(name.trim().toLowerCase().replace(/\s+/g, "-")); }
          }}
        />
      )}

      {promoting && (
        <NewBeadModal
          busy={busy}
          initial={{ title: promoting.body.split("\n")[0].slice(0, 90), body: promoting.body }}
          onClose={() => setPromoting(null)}
          onCreate={async (input) => {
            const ok = await run(
              () => client.promote(promoting.post_id, input.title, input.body, input.priority, input.tags),
              ["posts", "beads", "unrouted", "topics"],
            );
            if (ok) setPromoting(null);
          }}
        />
      )}

      {linking && (
        <BeadPicker
          title="Link this post to a bead"
          beads={beads ?? []}
          onClose={() => setLinking(null)}
          onPick={async (id) => {
            const ok = await run(() => client.route(linking.post_id, id), ["posts", "beads", "unrouted"]);
            if (ok) setLinking(null);
          }}
        />
      )}
    </div>
  );
}

/** Depth-first ordering over `reply_to`, so a thread reads top to bottom. */
function nest(posts: Post[]): { post: Post; depth: number }[] {
  const byParent = new Map<string | null, Post[]>();
  for (const p of posts) {
    const key = p.reply_to;
    byParent.set(key, [...(byParent.get(key) ?? []), p]);
  }
  const out: { post: Post; depth: number }[] = [];
  const walk = (parent: string | null, depth: number) => {
    const children = [...(byParent.get(parent) ?? [])]
      .sort((a, b) => Number(b.sticky) - Number(a.sticky) || a.sent_ts.localeCompare(b.sent_ts));
    for (const post of children) {
      out.push({ post, depth });
      walk(post.post_id, depth + 1);
    }
  };
  walk(null, 0);
  // Orphans (parent outside this topic view) still have to render.
  const seen = new Set(out.map((o) => o.post.post_id));
  for (const p of posts) if (!seen.has(p.post_id)) out.push({ post: p, depth: 0 });
  return out;
}

function NewTopicModal({
  onCreate, onClose, busy,
}: { onCreate: (name: string, title: string, body: string) => void; onClose: () => void; busy: boolean }) {
  const [name, setName] = useState("");
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");

  return (
    <Modal
      title="New topic"
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>Cancel</button>
          <button className="btn primary" disabled={busy || !name.trim()} onClick={() => onCreate(name, title, body)}>
            Create topic
          </button>
        </>
      }
    >
      <div className="two">
        <div className="formrow">
          <label>Name</label>
          <input value={name} onChange={(e) => setName(e.target.value)} autoFocus placeholder="lease-semantics" />
        </div>
        <div className="formrow">
          <label>Title</label>
          <input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Lease semantics" />
        </div>
      </div>
      <div className="formrow">
        <label>First post</label>
        <textarea value={body} onChange={(e) => setBody(e.target.value)}
          placeholder="A topic with no posts does not show up in listings. Seed it here." />
      </div>
    </Modal>
  );
}
