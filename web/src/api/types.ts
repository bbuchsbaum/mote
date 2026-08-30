// Wire types. Every shape here was read from `mote --json <cmd>` against a real
// store, not inferred from source. Field names and nullability match exactly;
// do not "tidy" them without re-checking the CLI output.

export type Status = "open" | "doing" | "blocked" | "review" | "closed";
export type RouteState = "open" | "needs_bead" | "routed" | "resolved";
export type RequestState = "open" | "responded" | "declined" | "resolved";
export type MsgKind = "note" | "request" | "handoff" | "blocked" | "fyi" | "response" | "decline";
export type NoteKind = "note" | "progress" | "decision" | "handoff" | "blocker";
export type ScalarField = "title" | "status" | "priority" | "body" | "assignee";

/** `mote --json ls` */
export interface BeadRow {
  id: string;
  title: string;
  status: Status;
  priority: number;
  tags: string[];
  assignee: string | null;
}

export interface Note {
  actor: string;
  kind: NoteKind;
  op_id: string;
  text: string;
  ts: string;
}

export interface DiscussionSources {
  posts: { post_id: string; topic: string; from: string }[];
  topics: { topic: string; title: string }[];
}

export interface BeadEdge extends BeadRow { kind: string }
export interface ParentEdge { parent: string; kind: string }

/** `mote --json show <id>` */
export interface BeadDetail extends BeadRow {
  body: string;
  created_at: string;
  deleted_at: string | null;
  ready: boolean;
  notes: Note[];
  deps: ParentEdge[];
  dependents: BeadEdge[];
  children: BeadEdge[];
  relations: ParentEdge[];
  discussion_sources: DiscussionSources;
  /** Per-field clocks. A patch must echo these back or it is rejected. */
  clock: Partial<Record<ScalarField, string>>;
}

/** `mote --json history <id> --include-rejected` */
export interface HistoryEntry {
  accepted: boolean;
  actor: string;
  kind: string;
  op_id: string;
  reason: string | null;
  ts: string;
}

export interface ClaimRow {
  id: string;
  title?: string;
  status: Status;
  claimed_by: string;
  lease_until_ts: string;
}

export interface ReservationRow {
  reservation_id: string;
  actor: string;
  binding_kind: string;
  entity: string;
  paths: string[];
  lease_until_ts: string;
}

/** `mote --json board` */
export interface Board {
  actor: string;
  status_counts: Partial<Record<Status, number>>;
  active_claims: ClaimRow[];
  active_reservations: ReservationRow[];
  orphaned_claims: ClaimRow[];
  orphaned_reservations: ReservationRow[];
  discussion_unread: number;
  inbox_unacked: number;
}

/** `mote --json discuss topics` */
export interface Topic {
  topic: string;
  title: string;
  body: string;
  created_by: string;
  created_ts: string;
  created_op_id: string;
  last_activity_ts: string;
  last_activity_op_id: string;
  post_count: number;
  sticky_count: number;
  decision_count: number;
  explicit: boolean;
  route_state: RouteState;
  issues: string[];
  summary_post_id: string | null;
  /** Only present on `in-flight`; the console computes it per actor elsewhere. */
  unread?: number;
}

/** `mote --json discuss list|thread` */
export interface Post {
  post_id: string;
  topic: string;
  from: string;
  body: string;
  post_kind: string;
  reply_to: string | null;
  sent_ts: string;
  sticky: boolean;
  sticky_op_id: string | null;
  route_state: RouteState;
  issues: string[];
  answers: string[];
  explicit_notify: string[];
  notification_recipients: string[];
  idempotency_key: string | null;
  /** Present on `thread` only. */
  depth?: number;
}

/** `mote --json actor list` */
export interface Actor {
  actor: string;
  current: boolean;
  last_activity_ts: string | null;
  last_activity_op_id: string | null;
  active_claims: number;
  active_reservations: number;
  orphaned_claims: number;
  orphaned_reservations: number;
  inbox_unacked: number;
  incoming_open_requests: number;
  status: ActorStatusSummary;
  /** Composed server-side from conversation_between; not a CLI field. */
  last_message?: { body: string; ts: string; direction: "in" | "out" } | null;
}

export interface PresenceEvidence {
  state: "live" | "recent" | "expired" | "untracked";
  source: string;
  reason: string;
  as_of_ts: string;
}

export interface ActorStatusSummary {
  as_of_ts: string;
  presence: Omit<PresenceEvidence, "as_of_ts"> & {
    live_session_count: number;
    latest_lease_until_ts: string | null;
  };
}

/** `mote --json msg thread <peer>` */
export interface Message {
  msg_id: string;
  from: string;
  to: string;
  entity: string | null;
  reservation: string | null;
  msg_kind: MsgKind;
  body: string;
  reply_to: string | null;
  correlation_id: string | null;
  idempotency_key: string | null;
  answers: string[];
  request_state: RequestState | null;
  response_msg_id: string | null;
  response_post_id: string | null;
  resolved_op_id: string | null;
  resolved_ts: string | null;
  sent_ts: string;
  ack_ts: string | null;
  direction: "in" | "out";
}

export interface MessageSendResult {
  accepted: true;
  msg_id: string;
  delivery: "queued";
  addressed: true;
  private: false;
  require_live: boolean;
  idempotent_replay: boolean;
  recipient: string;
  recipient_presence: PresenceEvidence;
}

export interface DiscussionPostOptions {
  notify?: string[];
  idempotencyKey?: string;
}

export type EventCategory =
  | "issue"
  | "claim"
  | "reservation"
  | "message"
  | "discussion"
  | "session"
  | "candidate";

/** One `mote.event.v1` envelope, verbatim off the SSE stream. */
export interface MoteEvent {
  schema: "mote.event.v1";
  event_id: string;
  store_id: string;
  type: string;
  category: EventCategory;
  op_id: string;
  ts: string;
  actor: string;
  accepted: boolean;
  data: Record<string, unknown>;
}

export interface Unrouted {
  posts: Post[];
  topics: Topic[];
}

export interface BeadQuery {
  status?: Status;
  tag?: string[];
  assignee?: string;
  ready?: boolean;
  all?: boolean;
}

export interface NewBeadInput {
  title: string;
  body?: string;
  priority?: number;
  tags?: string[];
  deps?: string[];
  assignee?: string;
}
