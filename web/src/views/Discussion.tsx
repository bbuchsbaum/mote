import { useEffect, useMemo, useRef, useState } from "react";
import type { DiscussionReference, Post, StructuredDecision } from "../api/types";
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
  useEffect(() => {
    if (!focusPost) return;
    focusRef.current?.focus();
    focusRef.current?.scrollIntoView?.({ block: "center" });
  }, [focusPost, posts]);

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
            <button key={t.topic} data-nav-item className={`topic ${active === t.topic ? "on" : ""}`}
              onClick={() => onSelectTopic(t.topic)}>
              <span className="topic-t">
                {(t.unread ?? 0) > 0 && <span className="unread-dot" aria-label="unread" />}
                {t.title || t.topic}
              </span>
              <span className="topic-m">
                <span>{t.post_count} posts</span>
                {t.structured_decision_count > 0 && <span>{t.structured_decision_count} cited decision{t.structured_decision_count === 1 ? "" : "s"}</span>}
                {t.unresolved_question_count > 0 && <span>{t.unresolved_question_count} unresolved question{t.unresolved_question_count === 1 ? "" : "s"}</span>}
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
                  id={`post-${post.post_id}`}
                  ref={post.post_id === focusPost ? focusRef : undefined}
                  data-nav-item
                  tabIndex={-1}
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
                  {post.decision && <DecisionRecord decision={post.decision} onOpenBead={onOpenBead} />}
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

function DecisionRecord({
  decision, onOpenBead,
}: { decision: StructuredDecision; onOpenBead: (id: string) => void }) {
  const revealPost = (postId: string) => {
    document.getElementById(`post-${postId}`)?.scrollIntoView?.({ block: "center" });
  };
  return (
    <section className="decision-record" aria-label={`Cited decision ${shortId(decision.decision_id)}`}>
      <div className="decision-heading">
        <span>Cited decision</span>
        <span className="mono-id">{decision.agreed.length} agreed · {decision.questions.filter((q) => q.unresolved).length} unresolved</span>
      </div>
      <div className="decision-section-label">Agreed evidence</div>
      <div className="decision-agreements">
        {decision.agreed.map((agreed) => (
          <div className="decision-agreed" key={agreed.post_id}>
            <button className="btn link" onClick={() => revealPost(agreed.post_id)}>{shortId(agreed.post_id)}</button>
            <span className={`decision-status ${agreed.disposition}`}>{agreed.disposition.toUpperCase()}</span>
            <span className="decision-author">{agreed.from}</span>
            <span className="decision-quote">{agreed.body}</span>
          </div>
        ))}
      </div>
      {decision.references.length > 0 && (
        <div className="decision-references">
          <span className="decision-section-label">References</span>
          {decision.references.map((reference, index) => (
            <DecisionReference
              key={`${reference.kind}-${index}`}
              reference={reference}
              revealPost={revealPost}
              onOpenBead={onOpenBead}
            />
          ))}
        </div>
      )}
      {decision.questions.length > 0 && (
        <div className="decision-questions">
          <div className="decision-section-label">Tracked questions</div>
          {decision.questions.map((question) => (
            <article className="decision-question" key={question.question_id}>
              <div className="decision-question-head">
                <span className={`decision-status ${question.status}`}>{question.status.toUpperCase()}</span>
                <span>{question.text}</span>
                <span className="mono-id">{shortId(question.question_id)}</span>
              </div>
              {question.candidate_answers.map((answer) => (
                <div className="decision-answer" key={answer.op_id}>
                  <span className="decision-status answer">CANDIDATE ANSWER</span>
                  <span>by {answer.actor}</span>
                  {answer.note && <span>{answer.note}</span>}
                  {answer.references.map((reference, index) => (
                    <DecisionReference
                      key={`${answer.op_id}-${index}`}
                      reference={reference}
                      revealPost={revealPost}
                      onOpenBead={onOpenBead}
                    />
                  ))}
                </div>
              ))}
              {question.transitions.filter((transition) => transition.action !== "answer").map((transition) => (
                <div className="decision-answer" key={transition.op_id}>
                  <span className={`decision-status ${transition.action}`}>{transition.action.toUpperCase()}</span>
                  <span>by {transition.actor}</span>
                  {transition.note && <span>{transition.note}</span>}
                </div>
              ))}
            </article>
          ))}
        </div>
      )}
    </section>
  );
}

function DecisionReference({
  reference, revealPost, onOpenBead,
}: {
  reference: DiscussionReference;
  revealPost: (postId: string) => void;
  onOpenBead: (id: string) => void;
}) {
  switch (reference.kind) {
    case "post":
      return <button className="decision-ref" onClick={() => revealPost(reference.post_id)}>post:{shortId(reference.post_id)}</button>;
    case "issue":
      return <button className="decision-ref" onClick={() => onOpenBead(reference.issue_id)}>issue:{shortId(reference.issue_id)}</button>;
    case "url":
      return <a className="decision-ref" href={reference.url} target="_blank" rel="noreferrer">url:{reference.url}</a>;
    case "topic":
      return <span className="decision-ref">topic:{reference.topic}</span>;
    case "candidate":
      return <span className="decision-ref">candidate:{shortId(reference.candidate_id)}</span>;
  }
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
