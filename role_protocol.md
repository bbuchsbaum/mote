# Role Definitions and Assignment Leases

**Status:** v1 role core and candidate quorum amendment implemented
**Applies to:** Mote v0.2 immutable-op storage
**Implementation beads:** role design, role core, role-based candidate quorum

## 1. Purpose

Mote currently records actor names, session presence, claims, reservations, and
candidate-specific reviewers, but an operational role such as `reviewer`,
`release`, `chief`, or `deputy` exists only in prose. Prose has no current
holder, term, expiry, capacity, or machine-readable vacancy. Work can therefore
queue behind a role whose former holder has ended their session without any
surface reporting the outage.

Role protocol v1 adds two separate first-class facts:

1. an immutable role definition says what the role is, who may staff it, its
   capacity and standing coverage requirement, and its typed exclusions;
2. a bounded role assignment explicitly grants that role to one actor through
   one named live session for a finite term.

A role assignment is not a bead assignee, claim, path reservation, candidate
review, Git permission, or cryptographic credential. Other protocols gain
authority from it only when they explicitly name the exact role and validate a
live eligible assignment.

## 2. Identity and definitions

### 2.1 Role identity

A role id is `role-` followed by a ULID. A role also has an immutable,
store-unique lowercase name containing only `[a-z0-9-]`, beginning and ending
with an alphanumeric character. CLI commands accept either the id or its exact
name and always return the id.

Names are never reused. This prevents a historical requirement for
`role:reviewer` from silently acquiring a different remit or authority policy.
Candidate policies introduced by the quorum amendment bind the exact role id,
not a label or current name lookup.

### 2.2 `role_define`

`role_define` records:

- role id and name;
- non-empty remit;
- sorted typed exclusions;
- sorted non-empty assignment-authority actors;
- `capacity`, the maximum number of simultaneously active assignments;
- `minimum_active`, the standing coverage demand, with
  `0 <= minimum_active <= capacity`;
- definition actor, op id, timestamp, and actor-scoped idempotency key.

The defining actor must be one of the assignment-authority actors. This makes
the bootstrap grant explicit without pretending that an actor string proves a
real-world identity. Definitions are immutable in v1. A materially different
remit, exclusion policy, capacity, or authority set requires a new role id and
name; the old role may then be retired.

CLI defaults both `capacity` and `minimum_active` to `1`, matching the common
single-holder role while making an unstaffed definition visibly short of
coverage. A deliberately on-demand role uses `--minimum-active 0`.

At least two assignment-authority actors are recommended for operationally
critical roles, because v1 has no ambient liveness-based takeover. A vacant or
unstaffable role is surfaced as a blocker, not repaired by inferring a chief,
repository owner, recent contributor, or session holder.

### 2.3 Typed exclusions

Each exclusion has a stable code and optional exact target:

| Code | Target | Enforced when |
|---|---|---|
| `concurrent_role` | role id | assigning either role; exclusion is symmetric if declared by either definition |
| `candidate_proposer` | none | consuming the role for a candidate review |
| `candidate_authorizer` | none | consuming the role for a candidate review |
| `candidate_named_reviewer` | none | consuming a separate role-based review slot |
| `candidate_evidence_producer` | none | consuming the role for a candidate review |
| `candidate_path_reservation` | none | consuming the role for a candidate whose declared paths overlap an active reservation held by the actor |

Unknown codes are rejected. The last code is deliberately named for the fact
Mote can prove: an active overlapping reservation. It does not claim to know
who authored code. Remit prose may explain broader human expectations, but
only typed exclusions are machine-enforced and JSON distinguishes them from
the remit.

The role core enforces `concurrent_role`. Candidate role reviews enforce all
candidate-context exclusions at review publication and recheck them at every
explicit landability snapshot. A later evidence receipt or overlapping active
reservation can therefore make an earlier role approval ineligible without
rewriting it.

## 3. Session-bounded assignment leases

### 3.1 `role_assign`

An assignment id is `ra-` followed by a ULID. `role_assign` records:

- exact role id and definition op id;
- assignment id;
- holder actor and holder session id;
- exact holder-session lease clock (`last_heartbeat_op_id`);
- assigning actor;
- requested TTL seconds, op timestamp, and derived `lease_until_ts`;
- exact sorted active-assignment snapshot as `(assignment_id, clock_op_id)`;
- actor-scoped idempotency key.

The operation is accepted only when:

1. the role exists and is not retired;
2. the operation actor is named in its assignment-authority set;
3. the holder session exists, belongs to the holder actor, has not ended, and
   is live at the operation timestamp;
4. the named session lease clock is current and the requested role lease ends
   no later than that session lease;
5. the exact active-assignment snapshot matches replay at the operation
   timestamp;
6. capacity remains available and the holder has no other active assignment to
   the same role;
7. neither side of any active concurrent-role pair excludes the other.

Binding the term to the session lease prevents a later heartbeat from reviving
an assignment that already lapsed. To grant a longer role term, the holder must
first heartbeat the same session and an assignment authority must publish a
new assignment or renewal against that visible lease.

The CLI requires an explicit session id when assigning another actor. When the
holder equals the resolved actor, `MOTE_SESSION` may supply it. Mote never picks
one of an actor's sessions by recency.

### 3.2 Disposition

At an injected `as_of_ts`, every assignment has exactly one disposition:

- `active`: not released, before its role lease deadline, and the bound
  session has not ended before `as_of_ts`;
- `released`: explicitly relinquished or revoked;
- `expired`: the role lease deadline has passed;
- `session_ended`: the bound session ended before the role deadline;
- `session_missing`: the immutable assignment names no accepted session
  record; this is defensive replay output and a rejected assignment never
  creates it.

An assignment cannot become active again after any terminal disposition.
Session intent (`working`, `waiting`, and related values), recent activity, PID,
and actor-wide presence from another session do not affect role validity.

### 3.3 `role_renew`

Only an assignment-authority actor may renew. Renewal names the exact current
assignment clock and current holder session lease op, requires the assignment
to still be active, and requires the new deadline to fit within that current
session lease. It updates the assignment clock, TTL, and deadline. It cannot
change role, holder, or session and cannot revive an expired, ended, or
released assignment.

### 3.4 `role_release`

The holder actor or any assignment-authority actor may release an assignment.
The operation names the current assignment clock and an optional reason. The
holder may release even after their session ends; release is cleanup, not a new
authority grant. Concurrent release and renewal operations from one clock have
one replay winner and the other is rejected as stale.

### 3.5 `role_retire`

An assignment-authority actor may retire a role only when it has no active
assignments. The operation names the immutable definition op and the exact
sorted assignment-clock snapshot. Retirement is terminal; the name and id are
never reused, and later assignments fail. A candidate that still requires the
retired role remains explicitly blocked.

## 4. Capacity, demand, and vacancy

Capacity is the number of actors who may hold the role concurrently. It is not
a work queue and one holder may normally serve multiple work items.

`minimum_active` is standing demand. Later consumers may add contextual demand;
for candidate quorum this is the largest unsatisfied distinct-holder threshold
among pending candidates requiring the role. Demands are reported with their
source ids. They do not reserve a holder or mutate an assignment.

For a role at `as_of_ts`, pending candidates contribute contextual demand only
while their exact role requirement is unsatisfied. A terminal candidate or a
satisfied role quorum contributes no queued demand.

```text
candidate demand = required distinct approval count for each unsatisfied role requirement
```

For the combined projection:

```text
active_count = count(active assignments)
demanded_count = max(minimum_active, contextual demand counts, default 0)
available_capacity = capacity - active_count
coverage_shortfall = max(demanded_count - active_count, 0)
vacant = active_count == 0
```

`mote role list` always shows vacancy, even with zero demand. `mote audit`
emits `role_coverage_shortfall` as a warning only when
`coverage_shortfall > 0`; the finding lists the exact demand sources, active
assignments, nearest deadline, and whether the last holder expired, ended, or
released. Audit never assigns a holder, extends a term, or infers authority.

## 5. Compare-and-set and idempotency

Assignment publication includes the exact active assignment clocks observed by
the CLI. Two assignments produced from one capacity snapshot cannot both
silently overfill a role: replay accepts at most the first whose snapshot and
capacity remain valid. Renewal, release, and retirement likewise carry exact
register expectations.

Every role mutation has an actor-scoped idempotency key. Role and assignment
ids created by CLI retry are deterministically derived from store id, actor,
operation kind, and key. Reusing a key for the same canonical action returns
the accepted result; different content is rejected. Failed reducer operations
remain in history and do not consume capacity.

Natural passage of time changes derived disposition without publishing an op.
All commands and tests inject one `as_of_ts`; reducer replay never reads the
wall clock.

## 6. CLI and JSON

```text
mote role define NAME --remit TEXT --assigner ACTOR [--assigner ACTOR ...]
  [--capacity N] [--minimum-active N] [--exclude CODE[:ROLE]]
  --idempotency-key KEY
mote role assign ROLE ACTOR --session SESSION --expect-session OP --ttl DURATION
  --expect-active ASSIGNMENT:CLOCK ... --idempotency-key KEY
mote role renew ASSIGNMENT --ttl DURATION --expect CLOCK
  --expect-session OP --idempotency-key KEY
mote role release ASSIGNMENT --expect CLOCK [--reason TEXT]
  --idempotency-key KEY
mote role retire ROLE --expect-definition OP
  --expect-assignment ASSIGNMENT:CLOCK ... --idempotency-key KEY
mote role show ROLE
mote role list [--vacant] [--holder ACTOR]
```

For ordinary interactive use, `role assign` derives `--expect-active` from one
coherent replay snapshot; the explicit flag exists for automation and tests.
Human output states actor, session, assignment deadline, session deadline,
disposition, capacity, demand, and shortfall. JSON retains the exact role id,
definition op, assignment clocks, authority actors, exclusions, demand sources,
`as_of_ts`, and typed dispositions.

Events use category `role` and types `role.defined`, `role.assigned`,
`role.renewed`, `role.released`, and `role.retired`. Actor status lists active
role assignments separately from actor presence and recent activity. Watch,
TUI, and the HTTP snapshot expose the same reducer projection.

## 7. Reducer invariants

1. The same accepted op set and injected `as_of_ts` yields identical roles,
   assignment dispositions, capacity, and shortfalls on every machine.
2. No session, activity, Git, network, filesystem, or wall-clock lookup occurs
   during replay.
3. Role identity, name, remit, exclusions, capacity, minimum coverage, and
   assignment-authority set never mutate.
4. At most `capacity` assignments are active at one replay position.
5. One actor has at most one active assignment to one role.
6. Assignment terms never outlive the session lease observed by their accepted
   assignment or renewal operation.
7. Expired, ended, released, and retired state never revives.
8. A role assignment grants no authority outside a consumer that explicitly
   names that role.
9. Candidate labels, bead assignee, claim holder, reservation holder, recent
   activity, repository ownership, and remote access never imply a role.
10. Unknown roles, sessions, exclusions, stale clocks, over-capacity snapshots,
    and missing identity anchors fail closed.

## 8. Adversarial cases

- **Two assigners race for the last slot:** both name the same active snapshot;
  one is accepted and the other is stale.
- **Session ends early:** the assignment becomes `session_ended` immediately
  and standing or contextual demand reports a shortfall.
- **Session later heartbeats:** a lapsed assignment does not revive; an
  authority must publish a new assignment.
- **Role TTL exceeds session TTL:** assignment or renewal is rejected rather
  than silently presenting a longer grant.
- **Another session for the same actor is live:** it does not keep an
  assignment bound to the ended or expired session alive.
- **Holder and authority race release/renew:** exact assignment-clock CAS gives
  one winner.
- **Mutually excluded roles:** assignment is rejected if either definition
  excludes the other and the holder has an active conflicting assignment.
- **Reviewer reserves candidate paths after approval:** candidate quorum
  rechecks role eligibility when deriving landability; a previously counted
  role approval stops counting rather than becoming grandfathered.
- **All assignment authorities disappear:** Mote reports the coverage outage;
  it does not select a replacement from presence or activity.
- **Legacy store:** replay produces empty role maps and unchanged issue,
  session, reservation, message, discussion, and candidate behavior.

## 9. Compatibility and rollout

Role operations are additive tagged op variants. Older binaries may leave them
as malformed history and must not write role-aware candidate operations. A
role-aware candidate proposal uses protocol version 2, stores its complete
policy in `review_policy`, and deliberately leaves the legacy reviewer array
empty so an older reducer rejects it instead of accepting a weakened subset.
Role-capable binaries replay legacy stores without migration or rewritten ops.
Role names never substitute for candidate reviewer strings automatically;
role-based quorum is a separate additive candidate-policy form.

The role core owns types, reducer state, TTL/CAS behavior, CLI, JSON, events,
actor status, watch, TUI, HTTP snapshots, audit findings, and focused tests.
The quorum slice owns role eligibility for candidate reviews and contextual
demand. No implementation step assigns real project roles merely because a
definition or example appears in this document.
