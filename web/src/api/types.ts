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
  discussion_pulse: DiscussionPulse;
  discussion: DiscussionDecisionSummary;
  inbox_unacked: number;
}

export interface PulsePostRef {
  post_id: string;
  sent_ts: string;
  sent_op_id: string;
}

export interface DiscussionPulseParameters {
  short_window_s: number;
  burst_window_s: number;
  active_window_s: number;
  attention_window_s: number;
  burst_posts: number;
  burst_actors: number;
}

export interface TopicActivityPulse {
  topic: string;
  title: string;
  posts_5m: number;
  posts_15m: number;
  posts_60m: number;
  distinct_authors_60m: number;
  top_level_60m: number;
  replies_60m: number;
  raw_posts_60m: number;
  active_posts_60m: number;
  retracted_posts_60m: number;
  superseded_posts_60m: number;
  posts_short_window: number;
  posts_burst_window: number;
  distinct_authors_burst_window: number;
  posts_active_window: number;
  distinct_authors_active_window: number;
  burst: boolean;
  last_post_id: string;
  last_activity_ts: string;
  last_post: PulsePostRef;
  last_external_reply: PulsePostRef | null;
}

export interface TopicAttentionPulse {
  topic: string;
  title: string;
  unread_count: number;
  oldest_unread: PulsePostRef | null;
  newest_unread: PulsePostRef | null;
  notification_count: number;
  explicit_notification_count: number;
  watched_unread_count: number;
  unresolved_question_count: number;
  needs_bead_count: number;
  solitary_new_post_ids: string[];
  no_external_reply_post_ids: string[];
  explicit_attention: boolean;
}

export interface DiscussionPulse {
  schema: "mote.discussion-pulse.v1";
  actor: string | null;
  as_of_ts: string;
  parameters: DiscussionPulseParameters;
  definitions: {
    active: string;
    burst: string;
    solitary_new: string;
    unread: string;
    explicit_attention: string;
    no_external_reply: string;
  };
  active_now: TopicActivityPulse[];
  needs_eyes: TopicAttentionPulse[];
  totals: {
    active_topics: number;
    burst_topics: number;
    needs_eyes_topics: number;
    unread_posts: number;
    solitary_new_posts: number;
    no_external_reply_posts: number;
  };
  traversal: {
    topics_scanned: number;
    posts_scanned: number;
    reply_edges_scanned: number;
  };
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
  structured_decision_count: number;
  legacy_decision_count: number;
  question_count: number;
  open_question_count: number;
  deferred_question_count: number;
  superseded_question_count: number;
  closed_question_count: number;
  unresolved_question_count: number;
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
  decision: StructuredDecision | null;
  /** Present on `thread` only. */
  depth?: number;
}

export type DiscussionReference =
  | { kind: "topic"; topic: string }
  | { kind: "post"; post_id: string }
  | { kind: "issue"; issue_id: string }
  | { kind: "candidate"; candidate_id: string }
  | { kind: "url"; url: string };

export type DecisionQuestionStatus = "open" | "deferred" | "superseded" | "closed";
export type DecisionQuestionAction = "answer" | "defer" | "supersede" | "close";

export interface DecisionQuestionTransition {
  action: DecisionQuestionAction;
  actor: string;
  expect_question: string;
  references: DiscussionReference[];
  note: string | null;
  successor_question_id: string | null;
  idempotency_key: string | null;
  op_id: string;
  ts: string;
}

export interface DecisionQuestion {
  question_id: string;
  decision_id: string;
  topic: string;
  text: string;
  opened_by: string;
  opened_op_id: string;
  opened_ts: string;
  position: number;
  status: DecisionQuestionStatus;
  unresolved: boolean;
  clock_op_id: string;
  successor_question_id: string | null;
  answer_count: number;
  candidate_answers: DecisionQuestionTransition[];
  transitions: DecisionQuestionTransition[];
}

export interface AgreedPost {
  post_id: string;
  from: string;
  body: string;
  disposition: "active" | "superseded" | "retracted";
  sent_op_id: string;
}

export interface ResolvedDiscussionReference {
  reference: DiscussionReference;
  exists: boolean | null;
  disposition: string;
  title?: string | null;
  status?: string | null;
  route_state?: RouteState | null;
  topic?: string | null;
  from?: string | null;
  body?: string | null;
  issue?: string | null;
  commit_oid?: string | null;
  url?: string;
}

export interface StructuredDecision {
  decision_id: string;
  post: Omit<Post, "decision">;
  agreed: AgreedPost[];
  agreed_post_ids: string[];
  references: DiscussionReference[];
  resolved_references: ResolvedDiscussionReference[];
  questions: DecisionQuestion[];
  actor: string;
  op_id: string;
  ts: string;
}

export interface DiscussionDecisionSummary {
  structured_decision_count: number;
  legacy_decision_count: number;
  question_count: number;
  open_question_count: number;
  deferred_question_count: number;
  superseded_question_count: number;
  closed_question_count: number;
  unresolved_question_count: number;
  decisions: StructuredDecision[];
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
