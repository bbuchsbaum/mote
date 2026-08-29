import { useState } from "react";
import type { Post } from "../api/types";
import type { MoteClient } from "../api/client";
import { relativeTime, shortId, useResource, useWrite } from "../store";
import { BeadPicker, Empty } from "../components/ui";
import { NewBeadModal } from "./Issues";

/**
 * The seam. `discuss unrouted` returns exactly what someone declared actionable
 * and nobody has tracked — declared state, never inferred from prose. Clearing
 * this to zero is the daily job that makes the board and the tracker one thing.
 */
export function TriageView({
  client, actor, onOpenPost,
}: { client: MoteClient; actor: string; onOpenPost: (topic: string, postId: string) => void }) {
  const [error, setError] = useState<Error | null>(null);
  const { run, busy } = useWrite(setError);
  const [promoting, setPromoting] = useState<Post | null>(null);
  const [linking, setLinking] = useState<Post | null>(null);

  const { data, loading } = useResource("unrouted", actor, () => client.unrouted());
  const { data: beads } = useResource("beads", "picker", () => client.beads());
  const posts = data?.posts ?? [];
  const topics = data?.topics ?? [];

  return (
    <div className="pane">
      <div className="pane-head">
        <span className="pane-title">Triage</span>
        <span className="mono-id">{posts.length + topics.length} awaiting a bead</span>
      </div>

      {error && <div className="err">{error.message}</div>}

      <div className="scroll">
        {!loading && posts.length === 0 && topics.length === 0 ? (
          <Empty title="Nothing needs a bead">
            Discussion marked <code>needs bead</code> lands here. Nothing is waiting.
          </Empty>
        ) : (
          <div className="thread">
            {posts.map((p) => (
              <div key={p.post_id} data-nav-item tabIndex={-1} className="post" style={{ borderLeftColor: "var(--warn)" }}>
                <div className="post-head">
                  <span className="post-who">{p.from}</span>
                  <span className="post-when">{relativeTime(p.sent_ts)}</span>
                  <button className="beadref" onClick={() => onOpenPost(p.topic, p.post_id)}>
                    {p.topic} · {shortId(p.post_id)} →
                  </button>
                </div>
                <div className="post-body">{p.body}</div>
                <div className="post-acts">
                  <button className="btn link go" onClick={() => setPromoting(p)}>Promote to bead</button>
                  <button className="btn link" onClick={() => setLinking(p)}>Link existing bead</button>
                  <button className="btn link" disabled={busy}
                    onClick={() => void run(() => client.resolvePost(p.post_id), ["unrouted", "posts"])}>
                    No tracker action needed
                  </button>
                </div>
              </div>
            ))}
            {topics.map((t) => (
              <div key={t.topic} data-nav-item tabIndex={-1} className="post" style={{ borderLeftColor: "var(--warn)" }}>
                <div className="post-head">
                  <span className="post-who">{t.title || t.topic}</span>
                  <span className="post-when">whole topic · {t.post_count} posts</span>
                </div>
                <div className="post-body">{t.body}</div>
              </div>
            ))}
          </div>
        )}
      </div>

      {promoting && (
        <NewBeadModal
          busy={busy}
          initial={{ title: promoting.body.split("\n")[0].slice(0, 90), body: promoting.body }}
          onClose={() => setPromoting(null)}
          onCreate={async (input) => {
            const ok = await run(
              () => client.promote(promoting.post_id, input.title, input.body, input.priority, input.tags),
              ["unrouted", "beads", "posts", "topics"],
            );
            if (ok) setPromoting(null);
          }}
        />
      )}

      {linking && (
        <BeadPicker
          title="Link to an existing bead" beads={beads ?? []} onClose={() => setLinking(null)}
          onPick={async (id) => {
            const ok = await run(() => client.route(linking.post_id, id), ["unrouted", "beads", "posts"]);
            if (ok) setLinking(null);
          }}
        />
      )}
    </div>
  );
}
