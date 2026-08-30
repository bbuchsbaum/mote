import type {
  Actor, Board, BeadDetail, BeadQuery, BeadRow, HistoryEntry, Message,
  DiscussionPostOptions, MessageSendResult, MoteEvent, NewBeadInput, NoteKind,
  Post, ScalarField, Topic, Unrouted,
} from "./types";

/**
 * The op was published and the reducer rejected it. An op file exists in
 * `ops/` recording the rejected intent; the store remembers the attempt.
 * Distinct from ValidationError, where nothing was written at all.
 */
export class ConflictError extends Error {
  constructor(
    readonly opId: string,
    readonly reason: string,
    readonly current: Record<string, unknown>,
  ) {
    super(reason);
    this.name = "ConflictError";
  }
}

/** Input failed validation before anything was published. The world is unchanged. */
export class ValidationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ValidationError";
  }
}

export interface MoteClient {
  // reads
  board(): Promise<Board>;
  beads(query?: BeadQuery): Promise<BeadRow[]>;
  bead(id: string): Promise<BeadDetail>;
  history(id: string): Promise<HistoryEntry[]>;
  topics(): Promise<Topic[]>;
  posts(topic: string): Promise<Post[]>;
  unread(): Promise<Post[]>;
  thread(postId: string): Promise<Post[]>;
  unrouted(): Promise<Unrouted>;
  actors(): Promise<Actor[]>;
  dm(peer: string): Promise<Message[]>;

  // issue writes
  createBead(input: NewBeadInput): Promise<{ id: string }>;
  patchBead(
    id: string,
    fields: Partial<Record<ScalarField, string | number>>,
    clock: Partial<Record<ScalarField, string>>,
  ): Promise<void>;
  addNote(id: string, kind: NoteKind, text: string): Promise<void>;
  claim(id: string, ttlSeconds: number): Promise<void>;
  release(id: string): Promise<void>;
  close(id: string, note?: string): Promise<void>;

  // discussion writes
  createTopic(topic: string, title: string, body?: string): Promise<void>;
  post(
    topic: string,
    body: string,
    replyTo?: string | null,
    options?: DiscussionPostOptions,
  ): Promise<{ post_id: string }>;
  setSticky(postId: string, sticky: boolean): Promise<void>;
  promote(postId: string, title: string, body: string, priority?: number, tags?: string[]): Promise<{ id: string }>;
  route(postId: string, issue: string): Promise<void>;
  needsBead(postId: string): Promise<void>;
  resolvePost(postId: string): Promise<void>;
  markRead(topic?: string): Promise<void>;

  // message writes
  sendMessage(
    to: string,
    body: string,
    kind: string,
    entity?: string | null,
    idempotencyKey?: string,
  ): Promise<MessageSendResult>;
  ackMessage(msgId: string): Promise<void>;
  replyMessage(msgId: string, body: string, kind: "response" | "decline"): Promise<void>;
  resolveRequest(msgId: string): Promise<void>;

  /** Returns an unsubscribe function. */
  subscribe(
    onEvent: (event: MoteEvent) => void,
    onConnection?: (connected: boolean) => void,
  ): () => void;
}

/* ------------------------------------------------------------------ */
/* HTTP implementation — talks to `mote serve`.                        */
/* ------------------------------------------------------------------ */

export class HttpClient implements MoteClient {
  constructor(private getActor: () => string, private base = "/api") {}

  private async req<T>(method: string, path: string, body?: unknown): Promise<T> {
    const headers: Record<string, string> = {
      "X-Mote-Actor": this.getActor(),
    };
    const isWrite = method !== "GET";
    if (isWrite) headers["Content-Type"] = "application/json";

    const res = await fetch(this.base + path, {
      method,
      headers,
      body: isWrite ? JSON.stringify(body ?? {}) : undefined,
    });

    if (res.status === 409) {
      const payload = await res.json();
      throw new ConflictError(payload.op_id, payload.reason, payload.current ?? {});
    }
    if (res.status === 422 || res.status === 400) {
      throw new ValidationError((await res.json()).message ?? res.statusText);
    }
    if (!res.ok) throw new Error(`${method} ${path} → ${res.status} ${res.statusText}`);
    if (res.status === 204) return undefined as T;
    return res.json() as Promise<T>;
  }

  private qs(query: BeadQuery = {}): string {
    const p = new URLSearchParams();
    if (query.status) p.set("status", query.status);
    if (query.assignee) p.set("assignee", query.assignee);
    if (query.ready) p.set("ready", "1");
    if (query.all) p.set("all", "1");
    for (const t of query.tag ?? []) p.append("tag", t);
    const s = p.toString();
    return s ? `?${s}` : "";
  }

  board() { return this.req<Board>("GET", "/board"); }
  beads(query?: BeadQuery) { return this.req<BeadRow[]>("GET", `/beads${this.qs(query)}`); }
  bead(id: string) { return this.req<BeadDetail>("GET", `/beads/${encodeURIComponent(id)}`); }
  history(id: string) { return this.req<HistoryEntry[]>("GET", `/beads/${encodeURIComponent(id)}/history?include_rejected=1`); }
  topics() { return this.req<Topic[]>("GET", "/topics"); }
  posts(topic: string) { return this.req<Post[]>("GET", `/topics/${encodeURIComponent(topic)}/posts`); }
  unread() { return this.req<Post[]>("GET", "/unread"); }
  thread(postId: string) { return this.req<Post[]>("GET", `/posts/${encodeURIComponent(postId)}/thread`); }
  unrouted() { return this.req<Unrouted>("GET", "/unrouted"); }
  actors() { return this.req<Actor[]>("GET", "/actors"); }
  dm(peer: string) { return this.req<Message[]>("GET", `/dm/${encodeURIComponent(peer)}`); }

  createBead(input: NewBeadInput) { return this.req<{ id: string }>("POST", "/beads", input); }
  patchBead(id: string, fields: Partial<Record<ScalarField, string | number>>, clock: Partial<Record<ScalarField, string>>) {
    return this.req<void>("PATCH", `/beads/${encodeURIComponent(id)}`, { fields, clock });
  }
  addNote(id: string, kind: NoteKind, text: string) {
    return this.req<void>("POST", `/beads/${encodeURIComponent(id)}/notes`, { kind, text });
  }
  claim(id: string, ttlSeconds: number) {
    return this.req<void>("POST", `/beads/${encodeURIComponent(id)}/claim`, { ttl: ttlSeconds });
  }
  release(id: string) { return this.req<void>("POST", `/beads/${encodeURIComponent(id)}/release`); }
  close(id: string, note?: string) { return this.req<void>("POST", `/beads/${encodeURIComponent(id)}/close`, { note }); }

  createTopic(topic: string, title: string, body?: string) {
    return this.req<void>("POST", "/topics", { topic, title, body });
  }
  post(topic: string, body: string, replyTo?: string | null, options: DiscussionPostOptions = {}) {
    return this.req<{ post_id: string }>("POST", `/topics/${encodeURIComponent(topic)}/posts`, {
      body,
      reply_to: replyTo ?? null,
      notify: options.notify ?? [],
      idempotency_key: options.idempotencyKey ?? null,
    });
  }
  setSticky(postId: string, sticky: boolean) {
    return this.req<void>("POST", `/posts/${encodeURIComponent(postId)}/sticky`, { sticky });
  }
  promote(postId: string, title: string, body: string, priority?: number, tags?: string[]) {
    return this.req<{ id: string }>("POST", `/posts/${encodeURIComponent(postId)}/promote`, { title, body, priority, tags });
  }
  route(postId: string, issue: string) {
    return this.req<void>("POST", `/posts/${encodeURIComponent(postId)}/route`, { issue });
  }
  needsBead(postId: string) { return this.req<void>("POST", `/posts/${encodeURIComponent(postId)}/needs-bead`); }
  resolvePost(postId: string) { return this.req<void>("POST", `/posts/${encodeURIComponent(postId)}/resolve`); }
  markRead(topic?: string) { return this.req<void>("POST", "/discussion/read", { topic: topic ?? null }); }

  sendMessage(to: string, body: string, kind: string, entity?: string | null, idempotencyKey?: string) {
    return this.req<MessageSendResult>("POST", "/messages", {
      to, body, kind, entity: entity ?? null,
      idempotency_key: idempotencyKey ?? `console-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    });
  }
  ackMessage(msgId: string) { return this.req<void>("POST", `/messages/${encodeURIComponent(msgId)}/ack`); }
  replyMessage(msgId: string, body: string, kind: "response" | "decline") {
    return this.req<void>("POST", `/messages/${encodeURIComponent(msgId)}/reply`, { body, kind });
  }
  resolveRequest(msgId: string) { return this.req<void>("POST", `/messages/${encodeURIComponent(msgId)}/resolve`); }

  subscribe(
    onEvent: (event: MoteEvent) => void,
    onConnection?: (connected: boolean) => void,
  ): () => void {
    // EventSource resends Last-Event-ID on reconnect, and every envelope's
    // event_id is the resume cursor, so reconnection resyncs for free.
    const src = new EventSource(`${this.base}/events`);
    src.onopen = () => onConnection?.(true);
    src.onerror = () => onConnection?.(false);
    src.onmessage = (e) => {
      try {
        onEvent(JSON.parse(e.data) as MoteEvent);
      } catch {
        /* a malformed frame must not kill the stream */
      }
    };
    return () => src.close();
  }
}
