# Mote — Design Notes

> Working name in user docs: "Pebble" / "Pebblelog". Repo dir: `mote`. Same metaphor.
> Repo path is `code/rust/mote` → Rust implementation assumed.

---

## Part 1 digest — "boring SQLite + event trail"  *(considered alternative)*

### The thesis (why not Beads)

- Beads' embedded Dolt is **single-writer**.
- "True multi-agent coordination" needs Dolt **server mode** (Beads FAQ).
- Recent Beads issues: stale LOCK cleanup, hooks re-entering `bd` while embedded flock is held → deadlock.
- That re-entrancy is the thing to *remove*, not recreate.
- Target: **one developer + a few local agents on one machine** doing tiny updates.

### Part 1's chosen substrate (not the final pick)

- SQLite WAL on a single local file.
- `synchronous=FULL`, `foreign_keys=ON`, `busy_timeout=5000`, every write `BEGIN IMMEDIATE`.
- STRICT tables.
- Requires SQLite **>= 3.51.3** (March 2026 WAL race fix).
- Local FS only (wal-index uses shared memory).

### Part 1's mutation rules (still useful — they survive into part 2)

1. Never store a bead as one giant JSON blob.
2. Relationships in their own tables.
3. Never hard-delete; tombstone with `deleted_at`.
4. Every write appends an event row.

### Part 1's hard rule (also survives)

> The core never re-enters itself. No daemon, no hook runner, no background sync, no CLI shelling out to itself.

---

## Part 2 digest — **immutable op-maildir**  *(adopted)*

### The pivot

> truth = immutable ops
> state = pure replay
> snapshots = disposable cache, never truth
> claims = leases, not locks

The shared-mutable thing on the write path is what keeps getting brittle in Beads/Dolt.
A single JSONL file still has one shared append head. **One file per op** removes the hotspot.
Writers only ever **create new files** — never edit, never truncate, never merge, never "clean a stale lock".

### Storage layout

```
.mote/
  FORMAT.json     # schema version, settings
  tmp/            # in-flight op writes (Maildir tmp)
  ops/            # published immutable ops
  snap/           # optional disposable cache (empty in v1)
```

### Write protocol (Maildir publish)

1. Serialize one op → canonical JSON bytes.
2. `tmp/<name>.json` with `O_CREAT|O_EXCL`.
3. Write bytes.
4. `fsync(tmpfile)`.
5. Close.
6. `link(tmp/<name>.json, ops/<name>.json)` — fails if dest exists, no silent stomp.
7. `fsync(ops_dir)` — directory entry durability (fsync(2) caveat).
8. `unlink(tmp/<name>.json)`.

Use `link`, not overwrite-rename, to keep "publish" create-only.

### Op naming

`20260420T182455.124583Z-p4312-c0007-r7f3a-h6b9d0.json`

- UTC ISO timestamp (microsecond precision)
- pid
- per-process counter
- short random suffix
- short content-hash suffix (cheap fsck + tamper-evidence)

Sortable. The filename **is** the op id.

### Op schema

```json
{
  "v": 1,
  "op": "20260420T182455.124583Z-p4312-c0007-r7f3a-h6b9d0",
  "ts": "2026-04-20T18:24:55.124583Z",
  "actor": "claude-plan",
  "entity": "bd-01JXYZ...",
  "kind": "patch",
  "expect": {
    "status": "<op-id-of-last-status-write>",
    "body":   "<op-id-of-last-body-write>"
  },
  "set":      { "status": "doing" },
  "add_tags": ["backend"],
  "del_tags": [],
  "add_deps": ["bd-01JABC..."],
  "del_deps": []
}
```

### Op kinds

`create | patch | comment | claim | release | delete`

### The merge rule (the elegant bit)

**Do not version the whole bead. Version touched scalar fields.**

Derived bead state carries per-field clocks:

```json
{
  "title": "...",
  "status": "doing",
  "body": "...",
  "_clock": { "title": "op-id-1", "status": "op-id-9", "body": "op-id-4" }
}
```

Consequences:

- two agents → different scalars → both win
- two agents → same scalar → deterministic conflict
- tags / deps are set ops → idempotent
- comments are append-only → never conflict

### Reducer semantics

Replay ops in **filename order**.

| kind         | rule                                                                  |
|--------------|-----------------------------------------------------------------------|
| `create`     | accepted only once per entity id                                      |
| `patch`      | accepted only if every `expect.<field>` matches current clock         |
| `add_tag`    | idempotent                                                            |
| `del_tag`    | idempotent                                                            |
| `add_dep`    | idempotent                                                            |
| `del_dep`    | idempotent                                                            |
| `comment`    | always appended                                                       |
| `delete`     | tombstone (never physical delete)                                     |
| *rejected*   | retained in history as rejected intent (audit trail, not silent loss) |

### Conflict UX

- exit `0` → accepted
- exit `2` → rejected as stale/conflicting

Agents get a clean retry loop.

### Claims = leases (not locks)

```json
{ "kind": "claim", "entity": "bd-01JXYZ...", "actor": "claude-fixer", "ttl_s": 1800 }
```

Derived state tracks `claimed_by`, `lease_until`, `claim_clock`.
A new claim is valid if the prior lease has expired by the new op's `ts`, OR `expect.claim_clock` matches.
**Abandoned agents do not leave permanent stale locks.** Worst case: a 30-minute timeout.

### Reading state (v1)

- scan `ops/`
- sort filenames
- replay into in-memory map
- answer `list`, `show`, `ready`, `history`

No snapshots day one. Add them only when profiling demands it. When added, snapshots live in `snap/`, are immutable, and are **derived caches only** — corrupted snapshot → delete + rebuild.

### Data model

| field      | shape          | semantics                          |
|------------|----------------|------------------------------------|
| `title`    | scalar         | clocked                            |
| `status`   | scalar         | clocked                            |
| `priority` | scalar         | clocked                            |
| `body`     | scalar         | clocked, **canonical summary only** |
| `assignee` | scalar         | clocked (separate from `claim`)    |
| `tags`     | set            | idempotent add/del                 |
| `deps`     | set            | idempotent add/del                 |
| `comments` | append-only    | never conflicts                    |
| `claim`    | soft lease     | ttl-bounded                        |

> **Hard rule**: `body` is canonical summary, **not** an agent conversation log. Chatter goes in `comment` ops. This is what keeps text conflicts rare.

### CLI surface (doc shows `pb`; we use `mote`)

```
mote init
mote new "Fix auth bug" -p 1
mote set bd-01J... status=doing
mote dep add bd-01J... bd-01K...
mote claim bd-01J... --by claude-fixer --ttl 1800
mote release bd-01J... --by claude-fixer
mote comment bd-01J... "Found root cause"
mote ls --ready
mote show bd-01J...
mote history bd-01J...
mote fsck
```

No daemon. No server. No background compactor. No hooks.

### fsck (v1)

- delete old `tmp/` junk
- verify hash suffixes
- (optionally write a snapshot later)

**No** lock healing. **No** "is this stale file safe to delete?" heuristics. That kind of guess is precisely the Beads failure mode.

### Why this dodges the Beads pain

- no embedded single-writer DB to contend on
- no external server mode
- no lock files on the main path
- no manual stale-lock cleanup logic
- no multi-step write tx that can deadlock against itself
- no mutable cache that matters for correctness

Correctness depends only on **create-only files + deterministic replay**.

### Implementation order (the doc's one-day plan)

1. `init`, path handling, canonical JSON, op naming
2. `publish_op()`
3. `replay()` + `apply()`
4. `new`, `set`, `ls`, `show`
5. `deps` + `ready`
6. `comments` + `claims`
7. three tests:
   - 20 concurrent creates
   - 2 concurrent status updates from the same base → **exactly one** accepted
   - crash before publish leaves only `tmp/` junk, no visible partial op

> "It is a very realistic one-day build in Python." We're in Rust. Multiplier ~2x but stronger guarantees.

---

## Decisions / disambiguations I'm baking into the PRD

- **Language**: Rust (repo path `code/rust/mote`).
- **Binary name**: `mote`.
- **Store dir**: `.mote/` (not `.pebble/`).
- **Bead id**: ULID with `bd-` prefix → `bd-01JXYZABCDEFGHJKMNPQRSTVWX`.
- **Op id**: filename sans `.json`.
- **Canonical JSON**: RFC 8785 (JCS) — sorted keys, UTF-8, no insignificant whitespace, canonical numbers.
- **Hash in filename**: first 6 hex chars (3 BLAKE3 bytes) over the canonical JSON bytes of the op **with the `op` field set to the empty string** (so the name is computable before it is also embedded). Verifier recomputes the same way.
- **Initial-state expect**: when patching a field that was last set by `create`, `expect.<field>` = the create op-id. For a field that has never been written, `expect.<field>` may be omitted or set to `null`; reducer treats both equivalently.
- **Status enum (v1)**: `open | doing | blocked | review | closed` (part 3 adds `review`). Extending requires bumping `FORMAT.json` schema version.
- **`comment` consolidates into `note`** (part 3): one append-only op with a `note_kind` field — `note | progress | decision | handoff | blocker`. Drops the standalone `comment` kind from part 2.
- **Reservation id**: `rv-<ULID>`. **Message id**: `msg-<ULID>`.
- **Actor identity**: resolved as `--actor` flag → `MOTE_ACTOR` env → `.mote/local/actor` file. No FORMAT.json or global-config fallback in v1.
- **Init creates** `.mote/`, `tmp/`, `ops/`, `local/`, `FORMAT.json`. Does **not** create `snap/` in v0.2 (snapshots deferred to v0.3).
- **Q4 (priority enum) — RESOLVED**: integer 0..3, 0 = highest. Removed from open questions in `PRD.json`.
- **Platform (v1)**: POSIX local FS (macOS, Linux). Windows is a non-goal in v1 — `link()` semantics differ and dir-fsync is not portable.
- **Concurrency target (v1)**: tens of agents on one machine, thousands of ops. Replay-from-scratch is fine.

## Open questions for the user

1. Confirm Rust over Python (repo path strongly implies Rust, but the doc estimates a Python build).
2. Confirm `mote` as binary name vs. the doc's `pb`.
3. Do `claim` and `assignee` *both* exist in v1, or does `claim` subsume `assignee`?
4. Should `priority` be an integer scale (`0..3`) or a named enum (`p0..p3`)?
5. Does `mote ls` default to "open + not blocked + not claimed-by-someone-else" (i.e. effectively `--ready`), or to "all open"?
6. Should `mote history <id>` include rejected intents by default, or only accepted ops?
7. (part 3) Default TTL for path reservations? Doc shows `1800` for claims; reasonable default `3600` for path leases?
8. (part 3) Should `mote begin` auto-extend an existing claim if the same actor re-enters, or always require explicit `release` first?

---

## Part 3 digest — coordination layer  *(adopted, extends part 2)*

The same immutable op log gains three logical **planes** layered on top:

1. **Issue plane** — small mutable scalars (status, priority, assignee), deterministic via field clocks (part 2). State enum gains `review`: `open | doing | blocked | review | closed`.
2. **Path plane** — repo-relative path **reservations** (advisory leases over directories or files), TTL-bounded, overlap-checked. Not a real FS lock; not git skip-worktree; explicitly a coordination primitive.
3. **Message plane** — direct agent-to-agent messages (`msg_send`) with single-step ack (`msg_ack`). Forms an inbox per actor.

### Why these three and not "git checkout"

- Worktrees remain the right escape hatch for *real* HEAD/index isolation.
- `skip-worktree` is for sparse checkout, and git may still write those files during merge/rebase → wrong primitive for coordination.
- Reservations are advisory by design; agents are expected to honor them. They're cheap, observable, and non-corrupting — exactly what shared, unprivileged coordination should look like.

### New op kinds (incremental over part 2)

| kind            | semantics                                                                       |
|-----------------|---------------------------------------------------------------------------------|
| `note`          | append-only progress/handoff/rationale entry on an issue (replaces `comment`)   |
| `reserve_open`  | open a TTL-bounded reservation over one or more repo-relative paths              |
| `reserve_close` | release one or more reservations (idempotent over already-closed paths)          |
| `msg_send`      | direct message from one actor to another, optionally about an issue              |
| `msg_ack`       | one-shot acknowledgment of a `msg_send` by the recipient                         |

### Path overlap rule

Two paths overlap iff one is a prefix of the other under directory semantics:

- `src/auth/` overlaps `src/auth/token.rs`
- `src/auth/` overlaps `src/auth/`
- `src/auth/` does **not** overlap `src/authn/` (no false-prefix on partial component names)
- `src/` overlaps everything under it

Implementation: normalize trailing-slash + split-on-`/`-component prefix match.

### `reserve_open` reducer

Accepted iff **every** listed path is currently free (no live reservation overlaps any of them) at the op's `ts`. **All-or-nothing** — no partial reservations. A live reservation = an open reservation whose `lease_until = open_op.ts + ttl_s` has not been exceeded by the candidate op's `ts`, and which has not been closed by a later `reserve_close`.

### `note` reducer

Always accepted. Categories (v1): `progress | handoff | rationale | blocker | resolution`. Free-text body. No clocks, no conflicts.

### Inbox / messages

- `msg_send` is always accepted (it's append-only intent).
- `msg_ack` is accepted iff the referenced `msg_send` has not already been acked by the same recipient. Self-ack of one's own send is rejected.
- `mote inbox` = list of `msg_send` ops where `to == self.actor` and no matching `msg_ack` from `self.actor` exists.

### New CLI surface

| command          | shape                                                                            |
|------------------|----------------------------------------------------------------------------------|
| `preflight`      | `mote preflight --issue bd-... --paths <p>...` — dry-run; reports overlaps        |
| `begin`          | `mote begin bd-... --paths <p>... [--note "..."]` — claim + reserve_open + note   |
| `note`           | `mote note bd-... --kind <note_kind> "<text>"` — append-only (`note_kind` ∈ note \| progress \| decision \| handoff \| blocker) |
| `close`          | `mote close bd-...` — set status=closed (own op kind, idempotent)                  |
| `done`           | `mote done bd-... [--note "..."]` — compound: completion note + close + release reservations + release claim |
| `ready`          | `mote ready` — open beads with no open blockers and not foreign-claimed (also `mote ls --ready`) |
| `tag`            | `mote tag add\|rm <id> <tag>` — single tag_add / tag_remove op                       |
| `handoff`        | `mote handoff bd-... --to <actor> [--note "..."] [--release]` — note + reassign + optional reservation close |
| `inbox`          | `mote inbox` — list unacked messages directed at me                                |
| `who-has`        | `mote who-has <path>` — list live reservations overlapping path                    |
| `board`          | `mote board` — high-level overview: issues × claims × reservations                 |
| `msg`            | `mote msg <actor> "..."` (alias `mote send`) — direct message                      |
| `ack`            | `mote ack <msg-id>` — ack a directed message                                       |

### Compound commands & atomicity

`begin` and `handoff` are convenience wrappers that emit a **sequence** of independent ops (still one mutation per op file). Failure handling:

- `begin` order: `reserve_open` → `claim` → `note`. If `reserve_open` is rejected (overlap), the command exits 2 and emits no further ops. If `claim` is rejected after a successful `reserve_open`, the CLI emits a compensating `reserve_close` and exits 2. The `note` is best-effort.
- `handoff` order: `note` (note_kind=handoff) → `claim` (reassign to new actor) → `reserve_close` (if `--release`). The note is the durable intent; the claim/release are the mechanical effects.

### What stays the same

- one file per op, `O_CREAT|O_EXCL` + `link()` + dir fsync
- canonical JSON, sortable filenames, content-hash suffix
- replay-from-scratch as the v1 read path
- no daemons, no hooks, no background tasks, no mutable cache
- rejected ops are retained in `ops/` as audit trail

