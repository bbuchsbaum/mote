# Mote PRD Addendum — Agent Coordination Layer

**Status:** Draft v0.2 addendum  
**Applies to:** `mote_prd.md` v0.1  
**Theme:** low-tech coordination for multiple coding agents in one repo without defaulting to worktrees

## 1. Why this addendum exists

The base PRD solves durable concurrent task updates.

This addendum extends Mote so that multiple coding agents can also coordinate **where they are working**, **what state an issue is in**, and **what they need from each other**, while preserving the original design goals:

- no daemon
- no central mutable DB
- no fragile lock manager
- no hidden background coordination
- tiny codebase
- human-inspectable storage

The new requirement is not “full collaborative editing.”

It is:

- two or more agents in the same repository
- usually one shared working tree
- avoid stepping on the same files
- provide lightweight handoff and notes
- allow a small amount of interactive coordination
- still remain dead simple

## 2. Core design stance

Mote should have **three planes**, all built on the same immutable op log.

### 2.1 Issue plane

This is the original bead/task system.

It handles:

- create/update/close issue
- dependency edges
- issue claim lease
- notes and history

### 2.2 Path plane

This is the new coordination plane.

It handles:

- advisory reservations over repo-relative paths
- overlap checks before an agent starts editing
- handoff of a path area from one agent to another
- visibility into active work areas

This is **not** OS-enforced file locking.
It is an advisory reservation system.

### 2.3 Message plane

This is the lightweight direct message layer.

It handles:

- agent-to-agent messages
- handoff requests
- acknowledgements
- small interactive coordination

All three planes share the same publication mechanism and reducer.
There is still only one storage model: immutable op files.

## 3. Design rules for the coordination layer

1. **Reservations are leases, not locks.**
   They expire naturally and never require lock healing.

2. **Reservations are advisory, not enforced by the filesystem.**
   Agents are expected to check Mote before editing and to respect active reservations.

3. **Issue state stays small and mutable; progress stays append-only.**
   A single mutable `progress_text` field would create unnecessary write conflicts. Status should be scalar; progress should be note-like.

4. **Messages are tiny and explicit.**
   This is not chat. It is a structured mailbox for coordination.

5. **Git is context, not the coordinator.**
   Mote may read Git metadata when useful, but it must not depend on Git index flags or branch machinery for correctness.

6. **Worktrees remain the escape hatch.**
   If two agents need different branches, different HEADs, or truly overlapping edits, use worktrees. Mote is for the common case where that would be overkill.

## 4. New first-class concepts

## 4.1 Actor

An actor is a stable agent identity such as:

- `agent-alpha`
- `claude-fixer`
- `human`

Every op already has an `actor` field. This now becomes user-visible and operationally important.

Recommended sources, in order:

1. `--actor`
2. `MOTE_ACTOR`
3. `.mote/local/actor`

`.mote/local/*` is convenience state only, not source of truth.

## 4.2 Reservation

A reservation is an advisory exclusive lease over one or more **repo-relative path roots**.

Examples:

- `src/auth/`
- `src/parser/mod.rs`
- `tests/auth/`

A reservation belongs to:

- an actor
- optionally an issue
- a lease TTL
- an optional short note

A reservation says:

> “I intend to work in this area for a while. Please check here before editing overlapping paths.”

## 4.3 Message

A message is a tiny directed coordination event.

A message has:

- sender
- recipient
- body
- optional issue reference
- optional reservation reference
- optional kind

Kinds in v0.2:

- `note`
- `request`
- `handoff`
- `blocked`
- `fyi`

## 4.4 Note

The old `comment` concept should become `note`.

A note is append-only and belongs to an issue.

Kinds:

- `note`
- `progress`
- `decision`
- `handoff`
- `blocker`

This is where issue progress lives.

## 5. Path reservation model

## 5.1 Why reservations instead of file locking

Kernel file locking is the wrong level here.

It is too low-level, too easy to bypass, and too awkward for directory- or issue-scoped coordination. What we need is not “prevent writes to this inode.” We need “signal that this part of the repo is currently being changed by agent X for issue Y.”

Reservations express intent, not enforcement.

That is the right level for multi-agent coding.

## 5.2 Reservation scope

To keep overlap semantics simple, v0.2 should support only two path forms:

1. **Exact file path**
   - `src/http/router.rs`

2. **Directory prefix**
   - `src/http/`

No glob patterns in v0.2.

Why:

- easier overlap rules
- easier normalization
- easier human understanding
- fewer weird edge cases

Globs can be added later if there is real demand.

## 5.3 Normalization rules

Reservation paths must be:

- repo-relative
- normalized lexically
- not absolute
- not allowed to contain `..`
- use `/` separators internally

Symlink resolution is out of scope for v0.2.
Treat paths as lexical repo paths.

## 5.4 Overlap rules

Two reservations overlap if any claimed root intersects another.

Examples:

- `src/auth/` overlaps `src/auth/token.rs`
- `src/auth/token.rs` overlaps `src/auth/token.rs`
- `src/auth/` overlaps `src/auth/`
- `src/auth/` does **not** overlap `src/http/`

## 5.5 Reservation mode

v0.2 should support only:

- `exclusive`

No read/shared mode yet.

If shared mode ever becomes necessary, it can be added later. Starting with one mode keeps the mental model clean.

## 5.6 Lease model

A reservation has:

- `reservation_id`
- `actor`
- `paths[]`
- optional `issue_id`
- `ttl_s`
- optional `base_rev`
- optional `note`

Reservations expire naturally.

Expired reservations are ignored by queries and by future overlap checks.
No cleanup is required for correctness.

## 5.7 Accepted vs rejected reservation ops

A `reserve_open` op is accepted if, at its op timestamp, no overlapping **unexpired accepted** reservation exists for a different actor.

Otherwise it is rejected as a conflict.

A `reserve_close` op is accepted if it references an existing accepted reservation and is issued by the same actor.

## 5.8 Reservation philosophy

Reservations are:

- strong enough to be useful
- weak enough not to become a lock bureaucracy

An agent can still ignore Mote and edit anyway.
That is acceptable.
The system exists to make the cooperative path easy and visible.

## 6. Issue state and progress

## 6.1 Keep scalar state small

Issue scalar fields should remain small and conflict-resistant:

- `title`
- `status`
- `priority`
- `summary`
- `assignee` (optional)

Suggested `status` set for v0.2:

- `open`
- `doing`
- `blocked`
- `review`
- `closed`

## 6.2 Progress should be append-only

Do **not** add a large mutable “work log” field.

Instead, progress should be captured as append-only `note` ops, typically with kind `progress`.

Example:

```json
{
  "kind": "note",
  "note_kind": "progress",
  "entity": "bd-01...",
  "actor": "agent-alpha",
  "text": "Parser changes done. Tests still failing in auth middleware.",
  "ts": "..."
}
```

This lets multiple agents add useful progress without conflicting.

## 6.3 Handoff should be a first-class note kind

Handoffs are common in multi-agent work.

A handoff should:

- append a `note` with `note_kind=handoff`
- optionally send a direct message to another actor
- optionally release the issue claim and reservation(s)

## 7. Message layer

## 7.1 Why messages belong inside Mote

Without a message layer, coordination escapes into ad hoc terminal output, random scratch files, or hidden model context.

A tiny message layer gives:

- explicit requests
- explicit handoffs
- visible inbox
- auditable coordination

without introducing a daemon or chat server.

## 7.2 Message schema

```json
{
  "v": 1,
  "kind": "msg_send",
  "msg": "msg-01...",
  "from": "agent-alpha",
  "to": "agent-beta",
  "issue": "bd-01...",
  "reservation": "rv-01...",
  "msg_kind": "handoff",
  "body": "Auth middleware is done. Please take tests and cleanup.",
  "ts": "..."
}
```

Acknowledgement:

```json
{
  "v": 1,
  "kind": "msg_ack",
  "msg": "msg-01...",
  "actor": "agent-beta",
  "ts": "..."
}
```

## 7.3 Inbox semantics

An actor’s inbox is derived state:

- all messages addressed to that actor
- minus messages acked by that actor

Messages are immutable.
Read/unread is derived from send + ack ops.

## 7.4 Message scope

v0.2 should support:

- one recipient
- optional issue reference
- optional reservation reference
- plain text body

No threads, no reactions, no edits, no attachments.

## 8. New query surface

## 8.1 `mote preflight`

This is the most important new command.

Purpose:

> “Before I start editing these paths for this issue, what should I know?”

Inputs:

- optional issue id
- one or more paths
- inferred or explicit actor

Output should include:

- overlapping active reservations
- current issue status and claimant
- recent handoff/progress notes on the issue
- unread messages to this actor related to the issue
- open blocking dependencies

Exit code:

- `0` clear enough to start
- `2` contested or stale/problematic

This is the main coordination query agents should run before beginning work.

## 8.2 `mote who-has <path>`

Shows active reservation holder(s) for a path and its nearest overlapping scope.

## 8.3 `mote board`

One-shot overview of:

- issues in `doing` / `blocked` / `review`
- active issue claims
- active reservations
- unread messages for current actor
- ready issues

This is the lightweight interactive dashboard.
No daemon. No live UI required.

## 8.4 `mote inbox`

Shows unread messages for the current actor.

Optional filters:

- by issue
- by sender
- by kind

## 9. New command surface

## 9.1 Low-level commands

```bash
mote reserve src/auth/ src/http/router.rs --issue bd-01... --ttl 45m
mote unreserve rv-01...
mote who-has src/auth/token.rs
mote preflight --issue bd-01... --paths src/auth/ tests/auth/
mote msg send --to agent-beta --issue bd-01... --kind request "Please take tests"
mote inbox
mote msg ack msg-01...
mote note bd-01... --kind progress "Middleware done; tests remain"
```

## 9.2 High-level helper commands

These are CLI sugar over the low-level ops.

### `mote begin`

```bash
mote begin bd-01... --paths src/auth/ tests/auth/ --ttl 45m --note "Taking auth layer"
```

Semantics:

1. claim issue lease
2. open reservation(s)
3. append start/progress note

If reservations conflict, the command fails cleanly and reports holders.

### `mote handoff`

```bash
mote handoff bd-01... --to agent-beta --note "Parser done; tests and docs remain" --release
```

Semantics:

1. append handoff note
2. send direct message
3. optionally release issue claim
4. optionally release active reservations owned by current actor for that issue

### `mote done`

```bash
mote done bd-01... --note "Merged and tested"
```

Semantics:

1. set status closed
2. append completion note
3. release active reservations for current actor on that issue
4. release claim

## 10. Op additions

Add the following op kinds:

- `note`
- `reserve_open`
- `reserve_close`
- `msg_send`
- `msg_ack`

The original `comment` op should be renamed or aliased to `note`.

## 11. Reducer extensions

## 11.1 Derived state additions

Add derived maps for:

- `active_reservations: reservation_id -> ReservationState`
- `path_index: normalized_path_root -> reservation_id`
- `messages: msg_id -> MessageState`
- `issue_notes: issue_id -> Vec<Note>`

## 11.2 Reservation reduction

When replaying `reserve_open`:

1. normalize paths
2. find active reservations by other actors that overlap
3. if any exist and are unexpired at op time, reject
4. otherwise accept and materialize reservation

When replaying `reserve_close`:

1. locate reservation
2. verify actor matches owner
3. mark inactive

## 11.3 Message reduction

`msg_send` always accepts if schema-valid.

`msg_ack` accepts if:

- message exists
- actor matches recipient

Duplicate ack is a no-op success.

## 11.4 Note reduction

`note` always accepts if the target issue exists.

This makes notes a safe channel for concurrent progress reporting.

## 12. Git integration policy

Mote may integrate with Git in a **read-only contextual way**.

Allowed in v0.2:

- record `base_rev` at reservation time if inside a Git repo
- show `git diff --name-only -- <paths...>` style path-local status in queries
- show current branch / HEAD in diagnostic output

Disallowed in v0.2:

- mutating Git index bits for coordination
- manipulating sparse checkout flags
- automatically creating branches/worktrees
- staging or committing implicitly

The point is to help agents coordinate work, not to become a Git porcelain.

## 13. When to use Mote reservations vs worktrees

Use Mote reservations when:

- agents are working in mostly disjoint paths
- they can share one HEAD
- they need lightweight coordination, not isolation
- the goal is “who is touching what?”

Use worktrees when:

- agents need different branches or different HEADs
- changes are broad and overlapping
- generated files or refactors will touch large shared areas
- one agent needs strong isolation from another

Worktrees become the exception, not the default.

## 14. Failure model additions

Acceptable:

- reservation conflict on overlap
- expired reservation naturally disappearing from active view
- unacked message remaining visible
- multiple progress notes from different agents

Unacceptable:

- silent double-allocation of the same exact file scope to different actors when both reservations should have conflicted
- reservation requiring manual stale-lock cleanup
- message loss after publication
- issue history depending on mutable side files

## 15. Acceptance criteria for the coordination layer

1. Two concurrent reservations on disjoint paths both succeed.
2. Two concurrent exclusive reservations on overlapping paths result in one success and one conflict.
3. `mote preflight` surfaces overlapping active reservations.
4. `mote begin` fails cleanly if requested paths are already reserved by another actor.
5. A reservation expires naturally and stops blocking new work without cleanup.
6. `mote handoff` produces both an issue note and a direct message.
7. `mote inbox` hides acked messages and shows unacked ones.
8. Two agents can append progress notes to the same issue with no conflict.

## 16. Implementation recommendation

This coordination layer is still very implementable in Rust in roughly another day because it does **not** add a second storage engine or background service.

It is mostly:

- a few new op structs
- a path-overlap helper
- an inbox/materialization query
- two or three compound CLI commands

That is the right kind of extra power: modest, explicit, inspectable, and still boring in the best sense.

## 17. Final design decision

Mote should evolve from a simple immutable-op issue tracker into a **small local coordination fabric** for coding agents.

Not by adding daemons, sockets, locks, or a database.

But by adding three small things on top of the existing op log:

- append-only issue notes for progress and handoff
- advisory path reservations with TTLs
- direct messages with ack

That gives you:

- issue state
- file-area coordination
- handoff
- inbox-like interaction
- low-tech collaboration in one repo

without recreating the brittleness you are trying to escape.

## 18. Session, in-flight, and discussion routing roadmap

Recent multi-agent use exposed three follow-on needs that should share one
design surface instead of becoming separate ad hoc commands.

### 18.1 Session identity

Actor resolution should keep the current precedence:

1. `--actor`
2. `MOTE_ACTOR`
3. `.mote/local/actor`

The missing capability is a deliberate session affordance. A future
`mote session start --as <actor> [--ttl <seconds>]` should:

- print a shell-safe `export MOTE_ACTOR=<actor>` line for the caller to eval or copy
- publish a TTL-bounded session lease op so other agents can see active sessions
- warn when the actor falls back to `.mote/local/actor` in a multi-session repo

The CLI cannot set environment variables in its parent shell, so session start
must be explicit about how the identity is activated.

### 18.2 In-flight dashboard

`mote in-flight` should be a one-shot, read-only derived view. It should not
replace `mote watch` or `mote ui`; it should answer the narrower question:

> what work is actively being touched right now?

The first version should combine:

- live session leases, grouped by actor
- live path reservations
- live issue claims
- `doing` beads, including whether they have a live claim
- recent discussion topics with unread or unrouted activity

This remains a replay-only command. It must not infer hidden state from Git or
process tables unless such context is explicitly marked as advisory.

### 18.3 Discussion routing

Discussion threads are useful for exploration, but decisions and implementation
must route back into issue state. The board should gain structured routing ops
rather than relying only on prose.

Candidate commands:

```sh
mote discuss route post-... --issue bd-...
mote discuss route topic <topic> --issue bd-...
mote discuss decision --topic <topic> --body "decision text"
mote discuss resolve --topic <topic>
```

The derived routing state should support:

- `needs-bead`: discussion has actionable content but no linked issue
- `routed`: topic or post is linked to one or more beads
- `resolved`: discussion no longer needs tracker action

Promotion to a bead should be explicit. A future helper may create a bead from a
post, but it should record the post/topic link at the same time so the board is
not a second, competing task tracker.

### 18.4 Acceptance criteria

1. `mote begin` moves open work to `doing` so same-actor sessions do not keep
   seeing begun work in `mote ready`.
2. `mote session start --as <actor>` gives the caller a clear activation path
   and leaves a visible session lease.
3. `mote in-flight` shows claims, reservations, `doing` work, and active
   sessions from replayed state only.
4. Discussion routing can answer "which posts still need tracker action?"
   without scraping prose.
5. Board-to-bead promotion creates or links issue work without turning topic
   bodies into the source of truth.
