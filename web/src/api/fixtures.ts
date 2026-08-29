import type {
  Actor, Board, BeadDetail, BeadQuery, BeadRow, HistoryEntry, Message, MoteEvent,
  NewBeadInput, NoteKind, Post, RouteState, ScalarField, Status, Topic, Unrouted,
} from "./types";
import { ConflictError, ValidationError, type MoteClient } from "./client";

/**
 * An in-memory stand-in for `mote serve`, faithful to the parts of the store's
 * behaviour the UI has to cope with: per-field clocks and patch rejection,
 * append-only notes and posts, TTL leases, declared route state, and an event
 * stream. It also runs a small agent simulator so live updates and write
 * conflicts are exercisable before any Rust exists.
 */

let counter = 0;
const ulid = (prefix: string) =>
  `${prefix}-01M${Date.now().toString(36).toUpperCase()}${(counter++).toString(36).toUpperCase().padStart(4, "0")}`;
const opId = () => `${new Date().toISOString().replace(/[-:]/g, "").replace(/\..+/, "")}.${(counter++).toString().padStart(6, "0")}Z-p1-c0-r0-h0`;
const now = () => new Date().toISOString();
const plus = (seconds: number) => new Date(Date.now() + seconds * 1000).toISOString();
const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

interface Bead extends BeadDetail {
  claim: { by: string; until: string } | null;
}

function seedBead(
  title: string, status: Status, priority: number, tags: string[], minutesAgo: number,
  extra: Partial<Bead> = {},
): Bead {
  const stamp = opId();
  return {
    id: ulid("bd"), title, status, priority, tags, assignee: null,
    body: "", created_at: ago(minutesAgo), deleted_at: null, ready: true,
    notes: [], deps: [], dependents: [], children: [], relations: [],
    discussion_sources: { posts: [], topics: [] },
    clock: { title: stamp, status: stamp, priority: stamp, body: stamp },
    claim: null,
    ...extra,
  };
}

export class FixtureClient implements MoteClient {
  private allBeads: Bead[] = [];
  private allTopics: Topic[] = [];
  private allPosts: Post[] = [];
  private messages: Message[] = [];
  private allHistory = new Map<string, HistoryEntry[]>();
  private readCursor = new Map<string, string>();
  private subscribers = new Set<(e: MoteEvent) => void>();
  private timer: ReturnType<typeof setInterval> | null = null;

  constructor(private getActor: () => string) {
    this.seed();
  }

  /* ---------------- seed ---------------- */

  private seed() {
    const parser = seedBead("Surface reservation expiry warnings and explicit derived expiry events", "doing", 0,
      ["agent-ux", "events", "lease", "mote-dx"], 180, {
        body: "Reservations lapse silently. An agent holding one has no way to learn it is about to lose the path short of polling.",
      });
    parser.claim = { by: "alice", until: plus(24 * 60) };
    parser.notes = [{ actor: "alice", kind: "progress", op_id: opId(), text: "started on the derived event path", ts: ago(12) }];

    const cursor = seedBead("Make discussion unread state safely pageable and markable through a post", "open", 1,
      ["agent-ux", "cursor", "discussion", "mote-dx"], 240);
    const answering = seedBead("Let the answering actor close a request with provenance", "blocked", 1,
      ["agent-ux", "messaging", "request", "safety"], 300);
    const aliases = seedBead("Add safe high-frequency aliases and actionable near-miss diagnostics", "open", 2,
      ["agent-ux", "cli", "ergonomics", "mote-dx"], 400);
    const rfc = seedBead("RFC immutable op packs or snapshots for Git-scale Mote stores", "review", 2,
      ["git", "mote-dx", "rfc", "scaling", "storage"], 900);
    rfc.claim = { by: "codex-b", until: plus(4 * 60) };
    const identity = seedBead("Make concurrent actor identity fail-safe and diagnose inbox identity mismatches", "open", 1,
      ["identity", "messaging", "safety", "session"], 500);
    const help = seedBead("Add flat leaf-command discovery with mote help --all", "open", 1,
      ["cli", "discoverability", "help", "mote-dx"], 620);

    answering.deps = [{ parent: cursor.id, kind: "blocks" }];
    answering.ready = false;
    cursor.dependents = [{
      id: answering.id, title: answering.title, status: answering.status,
      priority: answering.priority, tags: answering.tags, assignee: answering.assignee,
      kind: "blocks",
    }];

    this.allBeads = [parser, cursor, answering, aliases, rfc, identity, help];
    for (const b of this.allBeads) {
      this.allHistory.set(b.id, [
        { accepted: true, actor: "alice", kind: "create", op_id: opId(), reason: null, ts: b.created_at },
      ]);
    }
    this.allHistory.get(parser.id)!.push(
      { accepted: true, actor: "alice", kind: "claim", op_id: opId(), reason: null, ts: ago(30) },
      { accepted: false, actor: "codex-b", kind: "patch", op_id: opId(), reason: "field `status` clock mismatch", ts: ago(28) },
      { accepted: true, actor: "alice", kind: "note", op_id: opId(), reason: null, ts: ago(12) },
    );

    this.allTopics = [
      this.mkTopic("planning", "Planning", "Coordination thread for the current milestone.", "alice", 180),
      this.mkTopic("lease-semantics", "Lease semantics", "What expiry should mean for reservations and claims.", "codex-b", 600),
      this.mkTopic("candidate-protocol", "Candidate protocol", "Landing rules and evidence.", "reviewer", 1500),
      this.mkTopic("general", "general", "", "alice", 4000),
    ];

    const root = this.mkPost("planning", "alice", 175,
      "Proposal: split parser and test work. Expiry is invisible until it bites — we should emit a derived warning event before a reservation lapses.", null);
    root.route_state = "routed";
    root.issues = [parser.id];
    parser.discussion_sources = {
      posts: [{ post_id: root.post_id, topic: root.topic, from: root.from }], topics: [],
    };

    const decision = this.mkPost("planning", "alice", 170,
      "Decision: split parser work from test work. Parser lands first; tests follow behind the same reservation.", null);
    decision.sticky = true;
    decision.post_kind = "decision";

    const reply1 = this.mkPost("planning", "codex-b", 165,
      "I can take tests. One question — does the warning need its own op kind, or is it derived at read time?", root.post_id);
    const reply2 = this.mkPost("planning", "alice", 160,
      "Derived. Nothing about a lapse is a decision anyone made, so there is nothing to record.", reply1.post_id);
    const orphan = this.mkPost("planning", "parser-session", 3,
      "Reservation adoption after an orphan should carry provenance in the note, not just the op id. Nobody is tracking this yet.", root.post_id);
    orphan.route_state = "needs_bead";

    this.mkPost("lease-semantics", "codex-b", 620,
      "A claim on closed work shows as orphaned. Should it be renewable? I think not — it should only be releasable.", null);
    const leaseNeeds = this.mkPost("lease-semantics", "reviewer", 45,
      "We never decided whether an expired reservation can be adopted. It cannot today, and that asymmetry needs a bead.", null);
    leaseNeeds.route_state = "needs_bead";

    void decision; void orphan;

    this.messages = [
      this.mkMsg("codex-b", "alice", "request", "Please take the parser work — I'm blocked on the lexer refactor and won't get to it today.", 60, {
        entity: parser.id, request_state: "open", ack_ts: ago(58),
      }),
      this.mkMsg("alice", "codex-b", "fyi", "Taking it. Reserving src/parser.rs for the next hour.", 55, {}),
      this.mkMsg("codex-b", "alice", "note", "Thanks. I'll pick up tests behind you.", 54, {}),
      this.mkMsg("parser-session", "alice", "blocked", "Cannot reserve src/watch.rs — reviewer holds it until 15:10.", 8, {}),
    ];

    this.readCursor.set("alice", reply2.post_id);
  }

  private mkTopic(topic: string, title: string, body: string, by: string, minutesAgo: number): Topic {
    return {
      topic, title, body, created_by: by, created_ts: ago(minutesAgo), created_op_id: opId(),
      last_activity_ts: ago(minutesAgo), last_activity_op_id: opId(),
      post_count: 0, sticky_count: 0, decision_count: 0, explicit: true,
      route_state: "open", issues: [], summary_post_id: null,
    };
  }

  private mkPost(topic: string, from: string, minutesAgo: number, body: string, replyTo: string | null): Post {
    const post: Post = {
      post_id: ulid("post"), topic, from, body, post_kind: "post",
      reply_to: replyTo, sent_ts: ago(minutesAgo), sticky: false, sticky_op_id: null,
      route_state: "open", issues: [],
    };
    this.allPosts.push(post);
    const t = this.allTopics.find((x) => x.topic === topic);
    if (t) {
      t.post_count += 1;
      if (post.sent_ts > t.last_activity_ts) t.last_activity_ts = post.sent_ts;
    }
    return post;
  }

  private mkMsg(from: string, to: string, kind: string, body: string, minutesAgo: number, extra: Partial<Message>): Message {
    return {
      msg_id: ulid("msg"), from, to, entity: null, reservation: null,
      msg_kind: kind as Message["msg_kind"], body, reply_to: null, correlation_id: null,
      idempotency_key: null, answers: [], request_state: null, response_msg_id: null,
      response_post_id: null, resolved_op_id: null, resolved_ts: null,
      sent_ts: ago(minutesAgo), ack_ts: null, direction: "in",
      ...extra,
    };
  }

  /* ---------------- events ---------------- */

  private emit(category: MoteEvent["category"], type: string, data: Record<string, unknown> = {}) {
    const event: MoteEvent = {
      schema: "mote.event.v1", event_id: opId(), store_id: "st-fixture",
      type, category, op_id: opId(), ts: now(), actor: this.getActor(),
      accepted: true, data,
    };
    for (const fn of this.subscribers) fn(event);
  }

  subscribe(onEvent: (e: MoteEvent) => void, onConnection?: (connected: boolean) => void): () => void {
    this.subscribers.add(onEvent);
    onConnection?.(true);
    if (!this.timer) this.timer = setInterval(() => this.simulateExternalPatch(), 45_000);
    return () => {
      onConnection?.(false);
      this.subscribers.delete(onEvent);
      if (this.subscribers.size === 0 && this.timer) {
        clearInterval(this.timer);
        this.timer = null;
      }
    };
  }

  /**
   * Another agent working in the same store. Without this the live-update and
   * conflict paths cannot be exercised until the server exists.
   *
   * `silent` models the real race field clocks exist for: the window between
   * another process publishing an op and this process's watcher noticing it.
   * A patch submitted in that window carries a stale clock and is rejected.
   */
  simulateExternalPatch(id?: string, opts: { silent?: boolean } = {}): string | null {
    const bead = id
      ? this.allBeads.find((b) => b.id === id)
      : this.allBeads.find((b) => b.status === "open");
    if (!bead) return null;
    const stamp = opId();
    bead.status = bead.status === "open" ? "doing" : "review";
    bead.clock.status = stamp;
    this.allHistory.get(bead.id)?.push({
      accepted: true, actor: "codex-b", kind: "patch", op_id: stamp, reason: null, ts: now(),
    });
    if (!opts.silent) this.emit("issue", "issue.patched", { id: bead.id, status: bead.status });
    return bead.id;
  }

  /* ---------------- reads ---------------- */

  private row = (b: Bead): BeadRow => ({
    id: b.id, title: b.title, status: b.status, priority: b.priority,
    tags: b.tags, assignee: b.assignee,
  });

  async board(): Promise<Board> {
    const actor = this.getActor();
    const counts: Partial<Record<Status, number>> = {};
    for (const b of this.allBeads) counts[b.status] = (counts[b.status] ?? 0) + 1;
    return {
      actor,
      status_counts: counts,
      active_claims: this.allBeads.filter((b) => b.claim).map((b) => ({
        id: b.id, title: b.title, status: b.status,
        claimed_by: b.claim!.by, lease_until_ts: b.claim!.until,
      })),
      active_reservations: [{
        reservation_id: ulid("rv"), actor: "alice", binding_kind: "bead",
        entity: this.allBeads[0].id, paths: ["src/watch.rs"], lease_until_ts: plus(50 * 60),
      }],
      orphaned_claims: [], orphaned_reservations: [],
      discussion_unread: this.unreadPosts(actor).length,
      inbox_unacked: this.messages.filter((m) => m.to === actor && !m.ack_ts).length,
    };
  }

  async beads(query: BeadQuery = {}): Promise<BeadRow[]> {
    return this.allBeads
      .filter((b) => (query.all ? true : b.status !== "closed"))
      .filter((b) => !query.status || b.status === query.status)
      .filter((b) => !query.ready || b.ready)
      .filter((b) => !query.tag?.length || query.tag.every((t) => b.tags.includes(t)))
      .sort((a, b) => a.priority - b.priority || a.title.localeCompare(b.title))
      .map(this.row);
  }

  async bead(id: string): Promise<BeadDetail> {
    const b = this.allBeads.find((x) => x.id === id);
    if (!b) throw new ValidationError(`no such bead ${id}`);
    const { claim: _claim, ...detail } = b;
    return structuredClone(detail);
  }

  async history(id: string): Promise<HistoryEntry[]> {
    return [...(this.allHistory.get(id) ?? [])].reverse();
  }

  async topics(): Promise<Topic[]> {
    const unread = this.unreadPosts(this.getActor());
    return this.allTopics
      .map((t) => ({ ...t, unread: unread.filter((p) => p.topic === t.topic).length }))
      .sort((a, b) => b.last_activity_ts.localeCompare(a.last_activity_ts));
  }

  async posts(topic: string): Promise<Post[]> {
    return this.allPosts.filter((p) => p.topic === topic).sort((a, b) => a.sent_ts.localeCompare(b.sent_ts));
  }

  async unread(): Promise<Post[]> {
    return structuredClone(this.unreadPosts(this.getActor()));
  }

  async thread(postId: string): Promise<Post[]> {
    const out: Post[] = [];
    const walk = (id: string, depth: number) => {
      const p = this.allPosts.find((x) => x.post_id === id);
      if (!p) return;
      out.push({ ...p, depth });
      for (const child of this.allPosts.filter((x) => x.reply_to === id)) walk(child.post_id, depth + 1);
    };
    walk(postId, 0);
    return out;
  }

  async unrouted(): Promise<Unrouted> {
    return {
      posts: this.allPosts.filter((p) => p.route_state === "needs_bead"),
      topics: this.allTopics.filter((t) => t.route_state === "needs_bead"),
    };
  }

  async actors(): Promise<Actor[]> {
    const me = this.getActor();
    const names = new Set<string>(["alice", "codex-b", "parser-session", "reviewer"]);
    for (const m of this.messages) { names.add(m.from); names.add(m.to); }
    names.delete(me);
    return [...names].map((actor) => {
      const convo = this.messages
        .filter((m) => (m.from === me && m.to === actor) || (m.from === actor && m.to === me))
        .sort((a, b) => a.sent_ts.localeCompare(b.sent_ts));
      const last = convo[convo.length - 1];
      return {
        actor, current: false,
        last_activity_ts: last?.sent_ts ?? null, last_activity_op_id: null,
        active_claims: this.allBeads.filter((b) => b.claim?.by === actor).length,
        active_reservations: 0, orphaned_claims: 0, orphaned_reservations: 0,
        inbox_unacked: this.messages.filter((m) => m.to === me && m.from === actor && !m.ack_ts).length,
        incoming_open_requests: this.messages.filter((m) => m.to === me && m.from === actor && m.request_state === "open").length,
        last_message: last ? { body: last.body, ts: last.sent_ts, direction: (last.from === me ? "out" : "in") as "in" | "out" } : null,
      };
    }).sort((a, b) => (b.last_activity_ts ?? "").localeCompare(a.last_activity_ts ?? ""));
  }

  async dm(peer: string): Promise<Message[]> {
    const me = this.getActor();
    return this.messages
      .filter((m) => (m.from === me && m.to === peer) || (m.from === peer && m.to === me))
      .sort((a, b) => a.sent_ts.localeCompare(b.sent_ts))
      .map((m) => ({ ...m, direction: m.from === me ? "out" : "in" } as Message));
  }

  private unreadPosts(actor: string): Post[] {
    const cursor = this.readCursor.get(actor) ?? "";
    const ordered = [...this.allPosts].sort((a, b) => a.sent_ts.localeCompare(b.sent_ts));
    const idx = ordered.findIndex((p) => p.post_id === cursor);
    return ordered.slice(idx + 1).filter((p) => p.from !== actor);
  }

  /* ---------------- writes ---------------- */

  async createBead(input: NewBeadInput): Promise<{ id: string }> {
    if (!input.title.trim()) throw new ValidationError("title is required");
    const b = seedBead(input.title, "open", input.priority ?? 2, input.tags ?? [], 0, {
      body: input.body ?? "",
      deps: (input.deps ?? []).map((parent) => ({ parent, kind: "blocks" })),
      ready: !(input.deps ?? []).length,
    });
    this.allBeads.unshift(b);
    this.allHistory.set(b.id, [{ accepted: true, actor: this.getActor(), kind: "create", op_id: opId(), reason: null, ts: now() }]);
    this.emit("issue", "issue.created", { id: b.id, title: b.title });
    return { id: b.id };
  }

  async patchBead(
    id: string,
    fields: Partial<Record<ScalarField, string | number>>,
    clock: Partial<Record<ScalarField, string>>,
  ): Promise<void> {
    const b = this.allBeads.find((x) => x.id === id);
    if (!b) throw new ValidationError(`no such bead ${id}`);
    for (const key of Object.keys(fields) as ScalarField[]) {
      if (clock[key] !== b.clock[key]) {
        const op = opId();
        this.allHistory.get(id)?.push({
          accepted: false, actor: this.getActor(), kind: "patch", op_id: op,
          reason: `field \`${key}\` clock mismatch`, ts: now(),
        });
        throw new ConflictError(op, `field \`${key}\` clock mismatch`, { [key]: b[key] });
      }
    }
    const stamp = opId();
    for (const key of Object.keys(fields) as ScalarField[]) {
      (b as unknown as Record<string, unknown>)[key] = fields[key];
      b.clock[key] = stamp;
    }
    this.allHistory.get(id)?.push({ accepted: true, actor: this.getActor(), kind: "patch", op_id: stamp, reason: null, ts: now() });
    this.emit("issue", "issue.patched", { id, ...fields });
  }

  async addNote(id: string, kind: NoteKind, text: string): Promise<void> {
    const b = this.allBeads.find((x) => x.id === id);
    if (!b) throw new ValidationError(`no such bead ${id}`);
    if (!text.trim()) throw new ValidationError("note text is required");
    b.notes.push({ actor: this.getActor(), kind, op_id: opId(), text, ts: now() });
    this.allHistory.get(id)?.push({ accepted: true, actor: this.getActor(), kind: "note", op_id: opId(), reason: null, ts: now() });
    this.emit("issue", "issue.noted", { id });
  }

  async claim(id: string, ttlSeconds: number): Promise<void> {
    const b = this.allBeads.find((x) => x.id === id);
    if (!b) throw new ValidationError(`no such bead ${id}`);
    if (b.claim && b.claim.by !== this.getActor() && b.claim.until > now()) {
      throw new ConflictError(opId(), `claim held by ${b.claim.by}`, { claimed_by: b.claim.by });
    }
    b.claim = { by: this.getActor(), until: plus(ttlSeconds) };
    this.emit("claim", "claim.acquired", { id });
  }

  async release(id: string): Promise<void> {
    const b = this.allBeads.find((x) => x.id === id);
    if (b) b.claim = null;
    this.emit("claim", "claim.released", { id });
  }

  async close(id: string, note?: string): Promise<void> {
    const b = this.allBeads.find((x) => x.id === id);
    if (!b) throw new ValidationError(`no such bead ${id}`);
    if (note) await this.addNote(id, "note", note);
    b.status = "closed";
    b.clock.status = opId();
    this.emit("issue", "issue.closed", { id });
  }

  async createTopic(topic: string, title: string, body?: string): Promise<void> {
    const slug = topic.trim().toLowerCase().replace(/\s+/g, "-");
    if (!slug) throw new ValidationError("topic name is required");
    if (this.allTopics.some((t) => t.topic === slug)) throw new ValidationError(`topic \`${slug}\` already exists`);
    this.allTopics.push(this.mkTopic(slug, title || slug, body ?? "", this.getActor(), 0));
    if (body?.trim()) this.mkPost(slug, this.getActor(), 0, body, null);
    this.emit("discussion", "discussion.topic_created", { topic: slug });
  }

  async post(topic: string, body: string, replyTo?: string | null): Promise<{ post_id: string }> {
    if (!body.trim()) throw new ValidationError("post body is required");
    const p = this.mkPost(topic, this.getActor(), 0, body, replyTo ?? null);
    this.emit("discussion", "discussion.posted", { topic, post_id: p.post_id });
    return { post_id: p.post_id };
  }

  async setSticky(postId: string, sticky: boolean): Promise<void> {
    const p = this.allPosts.find((x) => x.post_id === postId);
    if (p) { p.sticky = sticky; p.sticky_op_id = sticky ? opId() : null; }
    this.emit("discussion", sticky ? "discussion.post_stuck" : "discussion.post_unstuck", { post_id: postId });
  }

  async promote(postId: string, title: string, body: string, priority?: number, tags?: string[]): Promise<{ id: string }> {
    const p = this.allPosts.find((x) => x.post_id === postId);
    if (!p) throw new ValidationError(`no such post ${postId}`);
    const { id } = await this.createBead({ title, body, priority, tags });
    p.route_state = "routed";
    p.issues = [...p.issues, id];
    const b = this.allBeads.find((x) => x.id === id)!;
    b.discussion_sources = {
      posts: [{ post_id: p.post_id, topic: p.topic, from: p.from }], topics: [],
    };
    this.emit("discussion", "discussion.routed", { post_id: postId, issue: id });
    return { id };
  }

  async route(postId: string, issue: string): Promise<void> {
    const p = this.allPosts.find((x) => x.post_id === postId);
    if (!p) throw new ValidationError(`no such post ${postId}`);
    if (!this.allBeads.some((b) => b.id === issue)) throw new ValidationError(`no such bead ${issue}`);
    p.route_state = "routed";
    if (!p.issues.includes(issue)) p.issues = [...p.issues, issue];
    const b = this.allBeads.find((x) => x.id === issue)!;
    if (!b.discussion_sources.posts.some((source) => source.post_id === postId)) {
      b.discussion_sources = {
        ...b.discussion_sources,
        posts: [...b.discussion_sources.posts, { post_id: p.post_id, topic: p.topic, from: p.from }],
      };
    }
    this.emit("discussion", "discussion.routed", { post_id: postId, issue });
  }

  private setRoute(postId: string, state: RouteState, type: string) {
    const p = this.allPosts.find((x) => x.post_id === postId);
    if (p) p.route_state = state;
    this.emit("discussion", type, { post_id: postId });
  }
  async needsBead(postId: string) { this.setRoute(postId, "needs_bead", "discussion.needs_bead"); }
  async resolvePost(postId: string) { this.setRoute(postId, "resolved", "discussion.resolved"); }

  async markRead(topic?: string): Promise<void> {
    const scoped = topic ? this.allPosts.filter((p) => p.topic === topic) : this.allPosts;
    const ordered = [...scoped].sort((a, b) => a.sent_ts.localeCompare(b.sent_ts));
    const last = ordered[ordered.length - 1];
    if (last) this.readCursor.set(this.getActor(), last.post_id);
    this.emit("discussion", "discussion.read", { topic: topic ?? null });
  }

  async sendMessage(to: string, body: string, kind: string, entity?: string | null): Promise<void> {
    if (!body.trim()) throw new ValidationError("message body is required");
    const m = this.mkMsg(this.getActor(), to, kind, body, 0, { entity: entity ?? null });
    if (kind === "request") m.request_state = "open";
    this.messages.push(m);
    this.emit("message", "message.sent", { msg_id: m.msg_id, to });
  }

  async ackMessage(msgId: string): Promise<void> {
    const m = this.messages.find((x) => x.msg_id === msgId);
    if (!m) throw new ValidationError(`no such message ${msgId}`);
    if (m.from === this.getActor()) throw new ConflictError(opId(), "a sender cannot ack their own message", {});
    m.ack_ts = now();
    this.emit("message", "message.acknowledged", { msg_id: msgId });
  }

  async replyMessage(msgId: string, body: string, kind: "response" | "decline"): Promise<void> {
    const root = this.messages.find((x) => x.msg_id === msgId);
    if (!root) throw new ValidationError(`no such message ${msgId}`);
    const reply = this.mkMsg(this.getActor(), root.from, kind, body, 0, { reply_to: msgId, correlation_id: msgId });
    this.messages.push(reply);
    root.request_state = kind === "response" ? "responded" : "declined";
    root.response_msg_id = reply.msg_id;
    this.emit("message", kind === "response" ? "message.responded" : "message.declined", { msg_id: msgId });
  }

  async resolveRequest(msgId: string): Promise<void> {
    const m = this.messages.find((x) => x.msg_id === msgId);
    if (!m) throw new ValidationError(`no such message ${msgId}`);
    if (m.from !== this.getActor()) throw new ConflictError(opId(), "only the request sender can resolve it", {});
    m.request_state = "resolved";
    m.resolved_ts = now();
    this.emit("message", "message.resolved", { msg_id: msgId });
  }
}
