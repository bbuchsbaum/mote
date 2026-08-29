# Actor presence and status contract

- **Contract:** `mote.actor-status.v1`
- **Tracker:** `bd-01M16WDT1D1ET0DB7ZNZA5XVY5`
- **Status:** implemented and verified

Mote records actor names on immutable operations. It also records optional,
TTL-bounded sessions. This contract combines those records with current work
and pending communication so a person or integration can answer five separate
questions:

1. Does this actor have a valid session lease now?
2. When did the actor last do work or interact through Mote?
3. What has each live session explicitly said it is doing?
4. What work does the actor currently hold?
5. What communication is waiting for the actor?

The answers form one actor-status projection. The projection is derived by
replaying the op log at an explicit `as_of_ts`. It does not create heartbeat,
end, acknowledgement, or read operations.

## Commands and consumers

The primary query is:

```sh
mote actor status <actor>
mote --json actor status <actor>
```

When `<actor>` is omitted, Mote uses the normal actor-resolution rules. A name
that has never appeared in the store still produces an `untracked` snapshot;
the human output must warn that the name is unknown. This behavior lets
message-send preflight use the same projection without registering recipients
as a side effect.

The same projection is embedded, without changing existing fields, in:

- `mote actor list`
- `mote session list`
- `mote in-flight`
- `mote watch`
- the TUI actor view
- presence-aware message output
- discussion watch and notification output

`mote events` continues to use the `mote.event.v1` envelope. Presence event
payloads carry the actor-status evidence fields defined here.

### Implementation matrix

This contract was checked against the current replay and CLI projections:

| Surface | Evidence | Status |
|---|---|---|
| `actor list` | Legacy fields plus embedded `mote.actor-status.v1` presence, categorized activity, sessions, intent, work, and attention; presence and activity-window filters | Implemented reference collection surface |
| `session list` | Start/end/heartbeat provenance, TTL deadline, derived live flag, and active session intent | Implemented in every embedded actor-status session |
| `inbox` and `msg requests` | Unacknowledged directed mail, request lifecycle, and send-time recipient evidence | Implemented as attention without treating receipt as activity |
| Discussion unread | Global and per-topic read cursors over public posts, explicit topic watches, and immutable notification recipients | Implemented as public attention routing without membership or privacy |
| `in-flight` | Actor aggregates plus live sessions, claims, reservations, doing beads, topics, and candidates | Implemented without removing detailed records |
| `watch` | Store-wide work counts, current-actor attention, and actor aggregates sampled at one timestamp | Implemented with periodic lease-expiry refresh |
| TUI | Agents tab with responsive actor list and per-session detail | Implemented as a read-only projection |
| `mote.event.v1` | Stable envelopes and cursors; distinct session events and actor-level time-derived presence transitions | Implemented with stable synthetic cursors |

Legacy repeated session starts remain backward-compatible renewal inputs and
now project as `session.heartbeat`. The legacy `last_activity` actor-list fields
still preserve their combined meaning, while the embedded status object
separates presence, substantive work, and interaction.

## What the status does not claim

Presence is coordination evidence, not process inspection or authentication.

- `live` means that at least one recorded session lease is valid at
  `as_of_ts`. It does not prove that a process is responsive.
- `expired` means that the store once had a session for the actor but no lease
  is valid now. It does not prove that the process exited.
- `recent` means that an actor with no session history authored a recent,
  accepted operation. It is not an online state.
- `untracked` means there is no current session evidence and no recent
  sessionless activity. The actor may still exist outside Mote.
- A direct message is addressed to an actor name, but it is readable by anyone
  who can read the shared store. Presence does not make messages private.
- Actor and session names are locally asserted. This contract does not add
  registration, credentials, access control, or remote process discovery.

The PID recorded on a session is diagnostic metadata. It must not decide
presence because stores can be shared across hosts and a PID can be reused.

## JSON shape

`mote --json actor status agent-parser` returns one object:

```json
{
  "schema": "mote.actor-status.v1",
  "actor": "agent-parser",
  "known": true,
  "current": false,
  "as_of_ts": "2026-08-29T14:30:00.000000Z",
  "recent_window_s": 600,
  "presence": {
    "state": "live",
    "source": "session_lease",
    "reason": "lease_valid",
    "live_session_count": 1,
    "latest_lease_until_ts": "2026-08-29T14:44:00.000000Z"
  },
  "activity": {
    "recent": true,
    "last_observed": {
      "ts": "2026-08-29T14:29:00.000000Z",
      "op_id": "20260829T142900.000000Z-p12-c0000-r1234-habcdef",
      "category": "presence",
      "type": "session.heartbeat"
    },
    "last_work": {
      "ts": "2026-08-29T14:27:00.000000Z",
      "op_id": "20260829T142700.000000Z-p12-c0000-r2345-habcdef",
      "category": "work",
      "type": "note.added"
    },
    "last_interaction": {
      "ts": "2026-08-29T14:28:00.000000Z",
      "op_id": "20260829T142800.000000Z-p12-c0000-r3456-habcdef",
      "category": "interaction",
      "type": "message.acknowledged"
    }
  },
  "sessions": [
    {
      "session_id": "sess-01...",
      "actor": "agent-parser",
      "label": "parser implementation",
      "pid": 1234,
      "ttl_s": 900,
      "started_ts": "2026-08-29T14:10:00.000000Z",
      "started_op_id": "20260829T141000.000000Z-p12-c0000-r4567-habcdef",
      "last_heartbeat_ts": "2026-08-29T14:29:00.000000Z",
      "last_heartbeat_op_id": "20260829T142900.000000Z-p12-c0000-r1234-habcdef",
      "lease_until_ts": "2026-08-29T14:44:00.000000Z",
      "ended_ts": null,
      "ended_op_id": null,
      "live": true,
      "intent": {
        "state": "working",
        "message": "implementing parser lowering",
        "issue": "bd-01...",
        "set_ts": "2026-08-29T14:20:00.000000Z",
        "set_op_id": "20260829T142000.000000Z-p12-c0000-r5678-habcdef"
      }
    }
  ],
  "intent": {
    "states": ["working"],
    "mixed": false
  },
  "work": {
    "active_claims": ["bd-01..."],
    "orphaned_claims": [],
    "active_reservations": ["rv-01..."],
    "orphaned_reservations": [],
    "doing_beads": ["bd-01..."],
    "candidates": [
      {
        "candidate_id": "cand-01...",
        "roles": ["reviewer"]
      }
    ]
  },
  "attention": {
    "inbox_unacked": 2,
    "incoming_open_requests": 1,
    "discussion_unread": 4,
    "topic_notifications_unread": 1,
    "watched_topics": ["architecture"]
  }
}
```

### Field rules

- `schema` is always `mote.actor-status.v1`.
- `actor` is the exact normalized actor name used for the query.
- `known` is true when the actor has authored an accepted operation, is named
  by accepted state such as a message recipient, claim, reservation, candidate
  role, or session, or is the currently resolved actor. Merely querying an
  unknown name does not make it known.
- `current` is true only when `actor` equals the actor resolved for the command.
- `as_of_ts` is the timestamp used for every time-dependent field in the
  object. One invocation must not sample the clock more than once.
- `recent_window_s` is the window used for activity recency and the
  sessionless `recent` presence state. The first implementation defaults to
  600 seconds and accepts an explicit override where the command exposes one.
- Optional scalar values are present as JSON `null`; they are not omitted.
- Collections are present as empty arrays when they have no members.
- Counts are non-negative JSON integers.
- Timestamps use Mote's normalized RFC 3339 UTC representation.
- Identifiers and enum values use their canonical lowercase representation.
- Actors, sessions, issue IDs, reservation IDs, candidate IDs, roles, states,
  and topic names are sorted deterministically. Activity evidence is selected
  by operation ID after filtering by `as_of_ts`.

Existing JSON consumers remain compatible:

- `mote actor list` keeps `actor`, `current`, `last_activity_ts`,
  `last_activity_op_id`, claim and reservation counts, `inbox_unacked`, and
  `incoming_open_requests`. It adds a `status` object containing this contract.
- `mote session list` keeps every existing session field and adds start, end,
  and heartbeat op provenance plus `intent`.
- `mote watch` and `mote in-flight` keep their current top-level fields and add
  an `actors` array of actor-status objects.
- New fields may be added compatibly within version 1. Removing a field,
  changing its type, or changing an enum meaning requires a new schema version.

## Presence derivation

The projection first discards evidence whose timestamp is later than
`as_of_ts`. A session is live exactly when:

```text
started_ts <= as_of_ts
and ended_ts is null or ended_ts > as_of_ts
and as_of_ts < lease_until_ts
```

The strict comparison at the lease boundary means a lease is expired when
`as_of_ts == lease_until_ts`.

Presence state is derived in this order:

1. `live`: at least one session is live.
2. `expired`: the actor has session history, but no session is live.
3. `recent`: the actor has no session history and authored an accepted work or
   interaction operation at or after `as_of_ts - recent_window_s`.
4. `untracked`: none of the rules above matched.

The `presence` evidence fields are:

| State | `source` | `reason` | Required details |
|---|---|---|---|
| `live` | `session_lease` | `lease_valid` | Positive `live_session_count`; maximum live `lease_until_ts` |
| `expired` | `session_history` | `ended`, `ttl_elapsed`, or `mixed` | Zero live sessions; most recent terminal evidence |
| `recent` | `accepted_activity` | `sessionless_recent_activity` | Zero sessions; recent work or interaction evidence |
| `untracked` | `none` | `no_presence_evidence` | Zero live sessions and no qualifying recent activity |

For `expired`, the reason is `ended` when every session visible at `as_of_ts`
ended explicitly, `ttl_elapsed` when none ended explicitly, and `mixed` when
both conditions occur across sessions.

Recent activity does not override session history. An actor whose session
expired and who then authored a new operation remains `expired`; its
`activity.recent` field becomes true. This makes the stale lease visible
instead of silently treating ordinary activity as a heartbeat.

### Presence truth table

| Session evidence at `as_of_ts` | Recent authored activity | State | Important consequence |
|---|---:|---|---|
| No session history | No | `untracked` | No online claim |
| No session history | Yes | `recent` | Recently observed, not live |
| One valid lease | No | `live` | Lease alone is enough |
| One valid lease | Yes | `live` | Activity remains a separate facet |
| All sessions explicitly ended | Either | `expired` / `ended` | End is terminal |
| All leases elapsed | Either | `expired` / `ttl_elapsed` | No fake end op is created |
| Ended and elapsed sessions | Either | `expired` / `mixed` | Session list retains each reason |
| Several sessions, at least one live | Either | `live` | Every live session remains visible |
| Known only as a message recipient | No | `untracked` | Mailbox exists without presence |
| Rejected operations only | No | `untracked` | Rejection is not activity evidence |

## Activity derivation

Activity uses accepted, actor-authored operations at or before `as_of_ts`.
Being named as a recipient, assignee, reviewer, or reservation holder by
another actor does not create activity for the named actor.

Each accepted operation contributes to one or more evidence classes:

| Class | Included operations | Excluded operations |
|---|---|---|
| `presence` | session start, heartbeat, status change, end | All tracker, message, and discussion work |
| `work` | issue creation and changes, notes, claims, reservations, candidate actions, discussion-to-work routing | Session operations, message receipt, passive queries |
| `interaction` | message send, reply, acknowledgement and resolution; discussion post, decision, summary, read marker, sticky, revision, retraction, routing and notification | Session heartbeat, passive queries |

An operation such as discussion routing may be both work and interaction. The
projection records it as the latest evidence for both classes. Event `type`
uses the same stable names as `mote.event.v1`.

`last_observed` is the latest accepted actor-authored operation of any class.
`last_work` and `last_interaction` exclude presence operations. The legacy
`last_activity_ts` and `last_activity_op_id` fields continue to mean
`last_observed` until a future schema version says otherwise.

`activity.recent` is true when either `last_work` or `last_interaction` is at
or after the inclusive cutoff. A heartbeat can therefore keep presence live
without making substantive activity recent.

Read-only commands never contribute evidence. In particular, `actor list`,
`session list`, `in-flight`, `watch`, `events`, inbox reads, discussion reads,
and the TUI must not publish activity merely because they observed the store.
Explicit `msg ack` and `discuss mark-read` commands do contribute interaction
evidence because they publish acknowledged user actions.

## Session heartbeat and intent

New clients publish an explicit `session_heartbeat` operation. Repeated legacy
`session_start` operations for an existing session continue to replay as
renewals and project as `session.heartbeat`, not `session.started`.

A heartbeat:

- names one existing, non-ended session;
- is published by that session's actor;
- records a new lease duration and resulting `lease_until_ts`;
- records `last_heartbeat_ts` and `last_heartbeat_op_id`;
- does not change session intent;
- cannot revive an ended session; and
- supports an idempotent retry key so a transport retry does not extend the
  lease twice or create two events.

An expired but non-ended session may heartbeat and become live again. The
accepted heartbeat is direct evidence that the session resumed; it produces a
new lease interval and an actor-level `presence.live` transition. An explicit
end remains terminal.

Declared intent is session-scoped. Valid states are:

- `available`: ready for directed work;
- `working`: actively advancing the named or described work;
- `waiting`: intentionally waiting for an event or reply;
- `blocked`: unable to advance the described work;
- `away`: not currently accepting interactive work.

The status operation requires a live session at its operation timestamp. It
may include a single-line message and an existing issue ID. Omitting either
produces `null`. A session with no status operation has `intent: null`. Ending
or expiring the session makes its intent inactive; the historical record
remains replayable.

Actor intent is an aggregation, not a winner. `intent.states` is the sorted
set of intent states on live sessions, and `intent.mixed` is true when that set
contains more than one value. Consumers that need a decision must inspect the
individual sessions. Mote does not impose a priority such as `blocked` over
`working` because two sessions may be doing unrelated work.

### Heartbeat write budget

Mote is append-only, so integrations must not publish a heartbeat on every
poll or every ordinary Mote command. The heartbeat command accepts a renewal
margin and publishes only when the remaining lease is within that margin,
unless the caller explicitly forces a heartbeat.

The documented general-purpose profile is a 15-minute lease with a five-minute
renewal margin. A harness may call heartbeat frequently, but this profile
produces at most about six accepted heartbeat operations per hour per live
session. Integrations that need faster failure detection must choose and report
their shorter TTL and accept the corresponding op volume. Ordinary work and
interaction operations do not implicitly extend a session lease.

## Work and attention

Work fields are derived at `as_of_ts` from the same disposition rules used by
`actor list`, `show`, `in-flight`, and preflight:

- Active and orphaned claims and reservations remain separate.
- `doing_beads` contains beads with a live claim held by the actor. A doing
  bead whose claim elapsed is not attributed to the former holder.
- Candidate roles are reported as a sorted set per candidate. Roles may
  include proposer, reviewer, authorizer, evidence producer, and currently
  authorized landing grantee.

Attention fields report durable work waiting for the actor:

- `inbox_unacked` counts messages addressed to the actor without an accepted
  acknowledgement.
- `incoming_open_requests` counts request roots addressed to the actor whose
  lifecycle is still open.
- `discussion_unread` uses the actor's global and per-topic board cursors.
- `topic_notifications_unread` counts unread public posts routed by an active
  watch at publish time or an explicit notification recipient. Delivery stays
  durable after unwatching and shares the ordinary discussion cursor.
- `watched_topics` lists the current deterministic subscription register.

Attention does not imply activity. Receiving mail, being mentioned, or
accumulating unread posts does not make an actor recent or live.

## Human rendering

Human output states the evidence instead of reducing status to a colored dot:

```text
agent-parser  live  lease until 14:40Z  2 sessions
  activity:    work 3m ago; interaction 2m ago; heartbeat 1m ago
  intent:      mixed (waiting, working)
  work:        1 claim, 1 reservation, 1 candidate
  attention:   2 mail, 1 open request, 4 discussion, 1 notification
```

Examples without a live lease are explicit:

```text
agent-reviewer  recent, not live  interaction 4m ago; no session history
agent-old       expired  TTL elapsed at 13:10Z; work 2m ago
agent-typo      untracked  actor has not been observed in this store
```

Human time abbreviations are presentation only. JSON always carries exact
timestamps, evidence, and `as_of_ts`.

## Session and presence events

Explicit session operations continue to use the existing `mote.event.v1`
envelope and `session` category. Their event types are:

- `session.started`
- `session.heartbeat`
- `session.status_changed`
- `session.ended`

Actor-level lease transitions use the same envelope with category `presence`:

- `presence.live`: the actor changes from no live sessions to at least one;
- `presence.expiring`: the lease that currently determines the actor's final
  live deadline enters its warning interval;
- `presence.ended`: an explicit end operation removes the actor's final live
  session; and
- `presence.expired`: wall time passes the actor's final live deadline.

The actor's final live deadline is the maximum `lease_until_ts` among its live
sessions. Its expiring boundary is the start of the final ten percent of the
lease interval that established that deadline. Integer rounding must leave at
least one second of warning for a lease longer than one second. A heartbeat
can establish a new final deadline and therefore a new warning boundary.

Every session payload contains `actor`, `session_id`, heartbeat and lease
timestamps, and active intent where applicable. Every presence payload
contains `actor`, `presence_state`, `source`, `reason`, and `as_of_ts`.
Explicit session events use their op ID as the event ID. An explicit operation
that changes actor-level presence may also yield a presence event. Every
presence event uses a distinct, stable synthetic ID tied to the triggering
operation or time boundary, so it cannot collide with the raw session event.

Expiry never creates a fake `session_end` operation. A follower may observe an
expiry only while it is polling because Mote has no daemon. Resuming from a
cursor must emit each later transition once for that observer. Session events
remain separately filterable so a consumer can subscribe to actor-level
presence transitions without heartbeat traffic. The sessionless `recent` and
`untracked` states are query projections in version 1; they do not create
presence events when the recency window opens or closes.

## Failure and compatibility rules

- Status projection is read-only. A projection failure cannot partially
  mutate the store.
- A command that cannot resolve its current actor may still inspect an
  explicitly named actor. Querying the implicit current actor fails with the
  existing identity diagnostic.
- Presence-aware message sending queues for every presence state by default.
  An opt-in live requirement is checked by the reducer at the message
  timestamp; failure leaves no accepted message. Accepted message records and
  delivery events retain the recipient state, source, reason, and as-of time.
  Presence never implies acknowledgement or request fulfillment, and direct
  messages remain readable to every reader of the shared store.
- Legacy stores without heartbeat or intent operations project existing
  sessions normally. `last_heartbeat_ts` defaults to `started_ts` and
  `last_heartbeat_op_id` defaults to `started_op_id` until a renewal is seen.
- Legacy actors with no sessions project as `recent` or `untracked` according
  to accepted activity. They are never silently registered.
- Unknown future operations remain governed by the existing replay policy.
  They do not become presence or activity evidence merely because they carry
  an actor-looking field.
- All time-dependent tests inject one `as_of_ts`; sleeping tests are not
  sufficient evidence for boundary behavior.

## Cross-surface conformance

For one store and one `as_of_ts`, every consumer must agree on:

- the set of known actors;
- presence state and evidence;
- live sessions and lease deadlines;
- last observed, work, and interaction evidence;
- active intent;
- work dispositions; and
- attention counts.

`actor status` is the reference projection. `actor list`, `session list`,
`in-flight`, `watch`, the TUI, inbox and request listings, discussion unread
and notifications, and presence events must call the same state-level
derivation or prove equivalent results in cross-surface tests. No consumer may
reconstruct time-based state by scanning raw operation names alone.
