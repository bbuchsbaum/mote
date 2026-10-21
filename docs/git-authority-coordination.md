# Coordination core development profile

Status: contract draft for GA-13 and GA-14. Implementation and qualification
remain gated on GA-12. This profile is for disposable development authorities;
it is not an adoption or migration format. It does not widen the meaning of
`synthetic-cas-v1`. Local stores retain their existing semantics.

The manifest capabilities are exactly, in this order:
`canonical-transactions-v1`, `coordination-core-v1`, `explicit-release-v1`.
An authority uses either this list or the synthetic profile's existing list.
Commands from the inactive profile are invalid. Adding lifecycle, messaging,
or import semantics requires a separately reviewed profile; an immutable core
manifest cannot silently acquire those meanings.

## State and replay boundary

The authority state retains its canonical sequence and a coordination wrapper:

- Canonical task operation entries, using only create, patch, tag add/remove,
  dependency add/remove, relation add/remove, and note.
- Sessions indexed by immutable session ID, with actor and descriptive presence.
- Claims indexed by task ID, with actor, session, and acquisition token.
- Reservations indexed by immutable reservation ID, with task, normalized paths,
  actor, session, acquisition token, and explicit open/closed status.

Session records have exactly actor, token, label, status (`active`),
presence_timestamp, and presence_text. Claim records have exactly actor, session,
and token. Reservation records have exactly task, actor, session, token, paths,
and status (`open` or `closed`). All fields are required and nonnull. Maps and
task entries are initially empty. Map keys supply the corresponding IDs.

Task entries replay through the existing reducer in canonical adapter-ID order.
Their envelopes are constructed by the planner from the recorded request and
canonical position. They never pass through local publication or ID generation.
Legacy ownership, sessions, reservations, roles, and candidates remain empty in
the reused task state. Existing TTL-based ownership and readiness helpers must
never inspect this wrapper. Public projections obtain ownership exclusively
from its explicit maps. A task-only projection may reuse task field rendering.

Planning clones the complete wrapper, validates all preconditions, and derives
every effect and result. Any semantic failure discards the entire provisional
wrapper, including provisional task history. Rejected transactions advance the
sequence but retain no effects or task-history entries. Replay reproduces the
same plan rather than trusting the recorded result. Arbitrary op passthrough,
generic batches, and client-supplied effects are invalid.

## Request schema

The existing request envelope is unchanged. Its command is
`{"kind":"coordination","action":ACTION}`. ACTION is a strict object tagged
by `kind`; the table lists all other required fields. Missing fields, unknown
fields, float syntax, and malformed identifiers are syntax errors. A nullable
field must be present, including when its value is null.

| Action kind | Other fields |
| --- | --- |
| `session.start` | `label`: string |
| `session.presence` | `text`: string |
| `task.create` | `id`: string, `title`: string, `body`: string, `priority`: integer, `assignee`: nullable string |
| `task.patch` | `id`: string, `set`: field-to-value object, `expect`: field-to-nullable-clock object |
| `task.tag` | `id`: string, `tag`: string, `add`: boolean |
| `task.dependency` | `id`: string, `parent`: string, `add`: boolean |
| `task.relation` | `id`: string, `parent`: string, `relation`: string, `add`: boolean |
| `task.note` | `id`: string, `note_kind`: string, `text`: string |
| `claim.acquire` | `id`: string |
| `claim.release` | `id`: string, `token`: string |
| `reservation.acquire` | `id`: string, `reservation`: string, `paths`: array of strings |
| `reservation.release` | `reservation`: string, `token`: string |

Task and reservation identifiers have the `bd-` and `rv-` prefixes respectively,
followed by 1–100 ASCII letters, digits, hyphens, or underscores. Actor and
session retain the request-envelope bounds. Label and presence text have at
most 4096 UTF-8 bytes. Task title has 1–4096 bytes; body and note text at most
1 MiB. Priority is 0–3. An assignee is null or a nonblank string of at most 128
bytes without control characters. A tag is nonblank, has at most 128 bytes,
and contains no control characters. Relation is `parent` or `related`.
Note kinds are exactly note, progress, decision, design, handoff, and blocker,
matching the current local allowlist. A future local addition does not silently
widen this immutable profile.

A patch writes one or more of title, body, priority, and assignee. Values follow
the create constraints; assignee clearing is unsupported in this core profile.
The `expect` keys must exactly equal the written fields. Null means that field
has never been written; a string must equal its current canonical adapter clock.
Expectations remain fixed across retries. An empty patch is invalid.
After checking every remote expectation, the generated legacy patch op omits
null expectations because PatchOp accepts only string clocks. If none remain,
it omits expect entirely, matching typed PatchOp serialization. The canonical
request always retains its nullable expectations; replay verifies absence again.

Paths contain 1–64 entries, must already equal existing Mote normalization,
and must be sorted and duplicate-free. A caller normalizes once before
journaling. NUL/control characters, backslashes, absolute paths, dot and dot-dot
components are invalid. Overlap uses existing component-prefix semantics.

## Preconditions and deterministic outcomes

All actions except session.start require an existing active session whose actor
equals the request actor. Session.start requires a never-used session ID;
a fresh request cannot renew or replace an existing session. Its timestamp is
descriptive. Presence records the request timestamp and text; earlier timestamps
are permitted and never expire, acquire, release, or transfer ownership.

Task.create requires a never-used task ID and always creates status open.
Its generated create op explicitly includes status open, title, body, and
priority in set; null assignee is omitted from set, while a string is included.
The existing ScalarSet serialization determines this omission. Defaults and
status therefore receive the create adapter clock; omitted assignee has no clock.
Task mutations require a known nondeleted task. Patch compares all clocks before
applying any field. Dependency endpoints must exist and be nondeleted. Adding
a dependency that would create a directed cycle is rejected. Adding a dependency
to a task with any open claim/reservation is refused until lifecycle semantics
qualify held-work changes. Relation endpoints must exist; self edges are refused.
The `parent` relation is acyclic; `related` is nonblocking and may form cycles.
Removal of a known absent tag/edge is accepted and recorded, as in local mode.

Claim.acquire requires a task whose status is open, doing, blocked, or review,
and an absent claim. It does not renew an existing holder, even for the same
actor/session. Claim.release requires the exact current actor/session/token.
No timestamp creates vacancy. Reservation.acquire requires an eligible task,
a never-used reservation ID, and no overlapping open reservation anywhere in
the project, including within the same session and across distinct tasks.
Reservation.release requires its exact actor/session/token and open status.
Released reservation IDs remain consumed. Claim tokens and reservation tokens
are their respective acquisition effect IDs. Releasing a claim does not release
reservations; compound lifecycle actions are unavailable in this core profile.

An eligible task for both acquisition actions is a known nondeleted task with
status open, doing, blocked, or review. Closed or missing tasks produce
task_unavailable, regardless of whether an acquisition would otherwise conflict.

Precondition order is session, task/reference existence, explicit clocks/tokens,
graph/ownership conflict, reducer acceptance. Conflicts are chosen deterministically:
field names, reservation IDs, and paths use lexical order. A semantic rejection
has outcome `{"status":"rejected","result":{"code":CODE,"subject":SUBJECT}}`.
Codes are `session_exists`, `session_unavailable`, `task_exists`, `task_unavailable`,
`stale_field`, `cycle`, `held_work`, `claim_occupied`, `claim_mismatch`,
`reservation_exists`, `path_conflict`, `reservation_mismatch`, and
`self_edge`, `task_effect_rejected`. Subject is the offending session/task/field/reservation
identifier, or the first conflicting reservation ID for path_conflict.

The following precedence examples are normative:

- If the session is unavailable and the task is absent, reject session_unavailable
  with the request session ID. Never inspect task conflicts first.
- For an edge action with both endpoints absent, reject task_unavailable for id.
  With id present and parent absent, use parent as subject. After both exist,
  a self edge rejects self_edge with id, including a related self-relation.
- With stale title and body clocks, body sorts first: stale_field subject body.
- For dependency insertion on held work that would also create a cycle, reject
  held_work with id before inspecting the prospective cycle. A cycle otherwise
  uses cycle with id; parent relation cycles use the same result.
- For reservation acquisition, check task eligibility, then reused reservation ID,
  then overlap. A reused ID that also overlaps rejects reservation_exists with
  that requested reservation ID. Multiple overlaps reject path_conflict with
  the lexically first open reservation ID, regardless of path iteration order.
- Release of an absent or closed reservation rejects reservation_mismatch with
  its reservation ID. Any wrong actor, session, or token uses the same result.
  A missing task claim or mismatched holder/token uses claim_mismatch with id.

Syntax validation precedes all semantic checks and creates no transaction.
Each action then follows the applicable checks above. Only after these checks
does the task adapter run; an unexpected reducer rejection uses
task_effect_rejected with id and discards its provisional history.

An accepted outcome is
`{"status":"accepted","result":{"code":"ok","subject":SUBJECT,"effect_ids":[IDS]}}`.
Subject is session ID for session actions, task ID for task and claim actions,
and reservation ID for reservation actions. IDs are in canonical effect order.
Each core action produces exactly one effect. Receipt recovery always returns
that original result, including its IDs and rejection reason.

## Effect schema

All effects retain effect_id, sequence, and index. The existing synthetic_set
wire object and semantics remain unchanged. The coordination profile accepts
only these additional strict effect objects:

- `kind: "task_op"`, with `op`: the complete generated typed task operation.
  Its op ID equals the canonical adapter ID, and its actor/timestamp equal
  the request. No caller-supplied legacy envelope is accepted.
- `kind: "ownership"`, with `transition`: the strict tagged ownership event.
  Its `kind` and complete other fields are listed below. All are required.

| Transition kind | Other fields |
| --- | --- |
| `session_started` | `session`, `actor`, `token`, `label`, `timestamp`: strings |
| `session_presence` | `session`, `actor`, `text`, `timestamp`: strings |
| `claim_acquired` | `task`, `actor`, `session`, `token`: strings |
| `claim_released` | `task`, `actor`, `session`, `token`: strings |
| `reservation_acquired` | `reservation`, `task`, `actor`, `session`, `token`: strings; `paths`: array of strings |
| `reservation_released` | `reservation`, `task`, `actor`, `session`, `token`: strings |

Acquisition and session enrollment tokens equal the enclosing effect ID.
Release tokens equal the current acquisition token, not the release effect ID.
Actor/session fields equal the request. Session enrollment creates active status
and initial presence with empty text and the request timestamp; a presence event
updates only its timestamp/text. Core has no ended/revoked session transitions.
Claims disappear on release; reservation records retain their paths and token
with closed status. Event fields and exact values are pinned by independent
golden vectors before implementation acceptance. No event can carry TTL fields.

Complete transition examples follow (normal JSON formatting is shown; the
existing canonical encoder determines stored bytes). They cover every event
shape; the enclosing effect supplies its canonical position and effect ID.

```json
[
  {"kind":"session_started","session":"sess-a","actor":"alice","token":"eff-enroll-000000","label":"laptop","timestamp":"2026-10-20T00:00:00Z"},
  {"kind":"session_presence","session":"sess-a","actor":"alice","text":"working","timestamp":"2026-10-19T00:00:00Z"},
  {"kind":"claim_acquired","task":"bd-one","actor":"alice","session":"sess-a","token":"eff-acquire-000000"},
  {"kind":"claim_released","task":"bd-one","actor":"alice","session":"sess-a","token":"eff-acquire-000000"},
  {"kind":"reservation_acquired","reservation":"rv-one","task":"bd-one","actor":"alice","session":"sess-a","token":"eff-reserve-000000","paths":["src/a.rs"]},
  {"kind":"reservation_released","reservation":"rv-one","task":"bd-one","actor":"alice","session":"sess-a","token":"eff-reserve-000000"}
]
```

Unknown transition fields or kinds fail closed. Validators regenerate these
objects from the parent wrapper and complete request. Task adapter op IDs
provide chronological field clocks; ownership tokens provide identity only.

## Required witnesses before connecting real commands

Use independent expected states for disjoint versus same-field edits, notes and
create retries, dependency and hierarchy cycles, same actor in separate sessions,
cross-task overlapping paths, backwards timestamps, wrong-token and wrong-session
release. Denied task and ownership actions must leave task values, task history,
and ownership maps unchanged. Reserve the task-effect-then-ownership-failure
witness for GA-15/16's actual compound commands; this profile has no batches. Require
inactive-profile, unknown-op passthrough, and local TTL regression tests.
This contract does not qualify begin/next/done, session end/recovery, communication,
roles, candidates, read dispatch, setup, migration, or adoption; their tickets
remain explicit prerequisites for the final user-facing release.
