# PRD — Mote

**Working title:** Mote  
**Tagline:** A lock-light, daemonless local task graph for concurrent coding agents  
**Status:** Draft v0.1  
**Implementation language:** Rust (recommended)  
**Target build:** 1–2 days for a solid CLI MVP

## 1. Summary

Mote is a local-first, immutable-op task tracker for one developer running multiple coding agents on the same machine. It replaces a shared mutable database with a Maildir-inspired directory of immutable operation files. Each write creates a brand-new file in `tmp/`, fsyncs it, publishes it into `ops/` with a create-only step, fsyncs the directory, and never mutates it again. State is derived by deterministic replay.

The design goal is not to be distributed or infinitely scalable. The goal is to **stop breaking** under ordinary local concurrent use while staying tiny, inspectable, elegant, and easy to implement.

## 2. Problem

The current Beads/Dolt stack is solving a broader problem than this product needs. Beads’s FAQ explicitly recommends Dolt server mode for true concurrent multi-agent writes, which adds exactly the kind of moving parts this replacement is intended to remove.

For the actual use case — one developer, one local repo, several local agents, small task updates — the desired properties are:

- concurrent access by multiple processes
- no daemon
- no central mutable DB file on the write path
- no fragile lock healing
- simple crash model
- small codebase
- predictable behavior under contention

## 3. Vision

A developer should be able to point several agents at the same workspace and say:

- create tasks
- update status
- add/remove dependencies
- leave comments
- claim work temporarily
- list ready work

…and trust that the system will either accept a write or reject it cleanly, but never corrupt itself and never require babysitting.

## 4. Goals

### 4.1 Product goals

1. **Concurrent multi-process writes on one machine** without a daemon or server.
2. **Crash-safe publication**: a process crash must not produce a visible partial write.
3. **Deterministic materialization** from immutable ops.
4. **Minimal command surface** that agents can use easily.
5. **Human-inspectable storage** that can be read with normal shell tools.
6. **Implementation small enough** that a sharp MVP fits in roughly a day.

### 4.2 Technical goals

1. Source of truth is a directory of immutable JSON op files.
2. No operation ever edits an existing op file.
3. Conflict handling is field-scoped, not whole-record scoped.
4. Claims are leases, not locks.
5. The hot path uses no background service, no watcher, no sqlite, and no network.

## 5. Non-goals

These are deliberately excluded from v0.1:

- distributed multi-machine sync
- branch-aware merge semantics
- background indexing service
- rich text collaborative editing
- SQL query language
- real-time subscriptions
- snapshots/compaction on day one
- Windows-perfect semantics on day one

## 6. Users and use cases

### Primary user

A single developer working locally with 2–10 coding agents that may issue commands concurrently.

### Core use cases

- An agent creates a new bead/task while another agent changes status on a different task.
- Two agents try to update the same field on the same task at nearly the same time.
- One agent claims work and disappears; another agent should recover naturally when the lease expires.
- A human wants to inspect history directly from the filesystem or via CLI.

## 7. Product principles

1. **Create-only writes.** Writers should add files, not edit shared state.
2. **Deterministic replay.** Any machine replaying the same ops should derive the same state.
3. **Narrow conflicts.** Unrelated updates should not collide.
4. **Explicit failure beats hidden magic.** Conflicts should return a clear stale-write error.
5. **Filesystem first.** Let the local filesystem provide the hard guarantees, not a custom coordinator.
6. **No async runtime.** This is a small CLI, not a service.

## 8. Why Rust

### Recommendation

Use **Rust**.

This tool is mostly filesystem I/O, not CPU-bound compute, so raw runtime speed is not the deciding factor. The real decision is between **delivery speed**, **correctness**, and **operational simplicity**.

Rust is a good fit because the core primitives we need are already available in the standard library: hard links, renames, file syncing, and ordered collections. Rust’s standard library exposes `std::fs::hard_link`, `std::fs::rename`, and `File::sync_all`, and it includes ordered collections such as `BTreeMap` and `BTreeSet`.

Rust also has mature library support for:

- typed JSON via Serde / `serde_json`
- ergonomic CLI parsing via `clap` derive
- sortable IDs via `ulid` if desired
- hashing via the official `blake3` Rust implementation
- date/time handling via crates like `jiff` or `time`; `jiff` is explicitly positioned as a high-level datetime library designed to be hard to misuse.

### Serious alternative

If the implementer is materially faster in Go, Go is the only serious alternative worth considering. Go’s `os` package provides the same basic file primitives needed here, including hard links, renames, and file sync.

### Final language call

- If you already write Rust comfortably: **use Rust**.
- If you do not write Rust comfortably and want the fastest path to a working version: **use Go**.

For this PRD, the implementation target is **Rust 2024 edition**. Rust 2024 is the current edition documented by the official Rust Edition Guide and the official book assumes `edition = "2024"`.

## 9. Scope of v0.1

### Included commands

- `mote init`
- `mote new`
- `mote show`
- `mote ls`
- `mote set`
- `mote dep add`
- `mote dep remove`
- `mote comment`
- `mote claim`
- `mote release`
- `mote close`
- `mote history`
- `mote ready`
- `mote fsck`

### Included entity capabilities

- create bead
- update scalar fields
- maintain dependency edges
- append comments
- lease-based claims
- tombstone delete/close semantics
- replay-based history

### Deferred to v0.2+

- snapshots for faster cold start
- export/import
- TUI
- richer query language
- agent-tailored JSON output modes

## 10. Filesystem layout

```text
.mote/
  FORMAT.json
  tmp/
  ops/
```

### Notes

- `FORMAT.json` stores schema/version metadata.
- `tmp/` contains incomplete writes only.
- `ops/` contains the immutable source of truth.
- All directories **must** live on the same filesystem, matching the Maildir assumption and the requirement for hard-link publication. Maildir documents the `tmp/new/cur` directories as being on the same filesystem, and Rust’s `hard_link` docs note that both paths often need to be on the same filesystem.

## 11. Write model

### Core write protocol

For each mutation:

1. Build a typed op struct.
2. Serialize it to canonical JSON bytes.
3. Create `tmp/<opname>.json` with create-new semantics.
4. Write all bytes.
5. `sync_all()` the temp file.
6. Close the file.
7. Publish by hard-linking `tmp/<opname>.json` to `ops/<opname>.json`.
8. `fsync` the `ops/` directory.
9. Remove the temp file.

### Durability requirement

Linux `fsync(2)` notes that syncing the file alone does not guarantee the directory entry is durable; the directory itself must also be synced.

### Why hard-link publication

Hard-link publication keeps the visible step create-only. If the destination name already exists, publication fails rather than replacing or mutating an existing op.

## 12. Op naming

Each op file name must be:

- lexicographically sortable by creation time
- unique across concurrent writers
- cheap to generate
- somewhat human-readable

### Proposed format

```text
20260420T182455.124583Z-p4312-c0007-r7f3a-h6b9d0.json
```

Where:

- timestamp = UTC RFC3339-like sortable prefix
- `p4312` = process id
- `c0007` = per-process counter
- `r7f3a` = random suffix
- `h6b9d0` = short content hash

The content hash is a corruption/debug aid, not the primary identity.

## 13. Identity model

### Bead IDs

Use ULIDs for bead IDs:

```text
bd-01JV5YQ4P0QF5X1W1Y6T2V8H9A
```

The `ulid` crate documents ULIDs as unique 128-bit lexicographically sortable identifiers with a 48-bit timestamp prefix and 80 bits of randomness.

### Op IDs

The file name is the op ID.

## 14. Data model

A bead is derived state, not stored as a mutable row.

### 14.1 Scalar fields

- `title`
- `status` (`open | doing | blocked | closed`)
- `priority` (`0..3`)
- `body`
- `assignee` (optional human-oriented field)
- `deleted_at` (derived tombstone)

### 14.2 Set-like fields

- `tags`
- `deps` (IDs of blocking beads)

### 14.3 Append-only fields

- `comments`

### 14.4 Lease fields

- `claimed_by`
- `lease_until`
- `claim_clock`

### 14.5 Field clocks

Each scalar field carries its own latest op clock in derived state:

```json
{
  "_clock": {
    "title": "op-a",
    "status": "op-b",
    "body": "op-c"
  }
}
```

This is the key to allowing unrelated edits to succeed concurrently.

## 15. Operation types

### `create`
Creates a bead.

### `patch`
Updates one or more scalar fields with optional per-field expectations.

### `tag_add` / `tag_remove`
Idempotent set operations.

### `dep_add` / `dep_remove`
Idempotent dependency edge operations.

### `comment`
Append-only text note.

### `claim`
Acquire or renew a lease.

### `release`
Release a current lease.

### `close`
Set status to closed.

### `delete`
Tombstone an item.

## 16. Canonical op schema

```json
{
  "v": 1,
  "op": "20260420T182455.124583Z-p4312-c0007-r7f3a-h6b9d0",
  "ts": "2026-04-20T18:24:55.124583Z",
  "actor": "agent-alpha",
  "entity": "bd-01JV5YQ4P0QF5X1W1Y6T2V8H9A",
  "kind": "patch",
  "expect": {
    "status": "20260420T181900.000001Z-p4100-c0002-r2ab1-h91aa22"
  },
  "set": {
    "status": "doing"
  }
}
```

### Canonicalization rule

To keep content hashing deterministic, ops must be serialized from typed structs with fixed field ordering. Arbitrary hash-map-shaped JSON objects are disallowed in source ops unless they are backed by deterministic ordering such as `BTreeMap`.

## 17. Replay and conflict semantics

Ops are replayed in lexicographic filename order.

### Replay rules

- `create`: accepted only once per bead id
- `patch`: accepted only if every expected field clock still matches
- `tag_add` / `tag_remove`: idempotent
- `dep_add` / `dep_remove`: idempotent
- `comment`: always append
- `claim`: accepted if lease is expired or expected claim clock matches
- `release`: accepted only by current holder or with expected claim clock
- `delete`: tombstones bead

### Conflict behavior

A stale same-field update is **not** corruption. It becomes a rejected op during replay. The CLI should report this as a conflict/stale write.

Example:

- Agent A sets `status=open -> doing` with expectation clock `x`
- Agent B sets `status=open -> blocked` with the same expectation clock `x`
- whichever op replays first wins
- the second op is rejected as stale

### Important consequence

Two agents changing different fields can both succeed:

- Agent A changes `status`
- Agent B changes `priority`

No conflict, because the field clocks are independent.

## 18. Claim model

Claims are leases, not locks.

Example claim op:

```json
{
  "v": 1,
  "kind": "claim",
  "entity": "bd-01JV5YQ4P0QF5X1W1Y6T2V8H9A",
  "actor": "agent-alpha",
  "ttl_s": 1800,
  "expect_claim": "20260420T180000.000001Z-p4000-c0001-r1111-haaaa11"
}
```

### Rules

- claims expire automatically by timestamp + ttl
- expired claims do not require cleanup to be ignored
- abandoned agents therefore do not create permanent stale locks

## 19. Query model

### `mote ls`
List beads with optional filters:

- status
- claimed/unclaimed
- tag
- priority

### `mote ready`
List beads that are:

- not deleted
- not closed
- have no open blocking deps
- optionally unclaimed, or claimed by caller

### `mote show <id>`
Materialized current state + derived clocks + lease state.

### `mote history <id>`
All ops touching a bead, annotated as accepted or rejected under current replay.

## 20. CLI UX

### Example commands

```bash
mote init
mote new "Fix auth bug" --priority 1 --tag backend
mote set bd-01JV... status=doing
mote dep add bd-01JV... bd-01JW...
mote comment bd-01JV... "Root cause is missing token refresh"
mote claim bd-01JV... --by agent-alpha --ttl 1800
mote release bd-01JV... --by agent-alpha
mote ready
mote history bd-01JV...
mote fsck
```

### Output rules

- human-readable by default
- `--json` for agent consumption
- stable exit codes

### Exit codes

- `0` success
- `2` stale/conflict
- `3` invalid command or validation error
- `4` repository/storage error

## 21. Validation rules

- title required on create
- bead IDs immutable
- self-dependency forbidden
- duplicate dependency edges collapse harmlessly
- closing an already closed bead is a no-op success
- deleting an already deleted bead is a no-op success
- comments cannot mutate prior comments

## 22. Failure model

### Acceptable failures

- visible conflict/stale write
- duplicate create rejected cleanly
- stale tmp files after crash

### Unacceptable failures

- partial op visible in `ops/`
- silent overwrite of an existing op
- corrupted replay state from ordinary concurrent writes
- permanent lock file that blocks future work

## 23. `fsck`

`mote fsck` must stay boring.

### Responsibilities

- validate op filename format
- validate content hash suffix
- verify JSON parseability and schema version
- remove stale files in `tmp/` older than threshold
- report orphan or malformed ops

### Explicit non-responsibilities

- no lock healing
- no speculative repair of accepted/rejected semantics
- no rewriting of history

## 24. Performance expectations

The system is primarily I/O bound.

### v0.1 targets

- repo with 1,000 ops should feel instant for common commands
- repo with 10,000 ops should remain acceptable without snapshots
- cold replay may be linear in op count
- write path should be a single short critical section around one file write and one publish step

Snapshots are intentionally deferred until there is evidence that replay time matters.

## 25. Platform assumptions

### v0.1 support target

- Linux: yes
- macOS: yes
- Windows: later / best effort only

Rationale: the durability story around directory fsync and the Maildir-inspired publication protocol is cleanest and best understood on POSIX-style local filesystems. Linux `fsync(2)` documents the need to sync the directory entry separately.

## 26. Rust implementation plan

### 26.1 Crate settings

- Edition: `2024`
- Binary crate only for v0.1
- No async runtime

### 26.2 Minimal dependency set

Recommended:

```toml
[package]
name = "mote"
version = "0.1.0"
edition = "2024"

[dependencies]
anyhow = "1"
blake3 = "1"
clap = { version = "4", features = ["derive"] }
jiff = "0.2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
ulid = "1"
```

### Why this dependency set

- `clap` gives a very fast CLI implementation path with derive support.
- `serde` / `serde_json` give typed JSON parsing and emission.
- `blake3` gives fast content hashing with an official Rust implementation.
- `jiff` gives high-level, hard-to-misuse datetime handling.
- `ulid` provides sortable IDs.

If dependency minimization beats developer speed, `anyhow` can be removed and `ulid` can be replaced with a custom ID format.

### 26.3 Suggested module layout

```text
src/
  main.rs
  cli.rs
  repo.rs        # discover/open .mote
  op.rs          # op structs + serde
  ids.rs         # bead ids and op names
  publish.rs     # tmp write + sync + hardlink publish
  reducer.rs     # replay and apply rules
  state.rs       # derived bead state
  query.rs       # ls/show/ready/history
  fsck.rs        # validation and cleanup
  errors.rs
```

### 26.4 Design rules for the Rust codebase

- keep core logic synchronous
- separate pure reducer logic from filesystem effects
- keep `serde_json::Value` out of the core; use typed structs
- avoid trait-heavy abstraction layers in v0.1
- integration tests should spawn real processes where possible

## 27. Acceptance criteria

### Correctness

1. 20 concurrent `new` commands create 20 distinct ops and 20 distinct beads.
2. Two concurrent same-field patches from the same expected clock result in one accepted and one conflict.
3. Two concurrent different-field patches can both succeed.
4. Crash before publication leaves only `tmp/` residue, never a partial visible op.
5. Replay of the same `ops/` directory always yields identical derived state.

### UX

1. `mote ready` correctly excludes blocked items.
2. `mote claim` expires naturally after TTL.
3. `mote history` shows accepted vs rejected ops.
4. `mote fsck` is safe to run repeatedly.

## 28. Test plan

### Unit tests

- op canonicalization
- field-clock conflict logic
- dependency reduction
- lease expiration logic
- ready-query filtering

### Integration tests

- concurrent create via spawned processes
- concurrent patch on same field
- concurrent patch on different fields
- crash-like interruption between temp write and publish
- fsck cleanup of stale tmp

### Golden tests

- fixed `ops/` fixture -> exact expected derived state JSON

## 29. Delivery plan

### Day 1 MVP

1. `init`
2. op publication protocol
3. replay engine
4. `new`, `set`, `show`, `ls`
5. `dep add/remove`
6. `ready`
7. integration tests for concurrency

### Day 2 polish

1. comments
2. claims / release
3. `history`
4. `fsck`
5. better JSON output

## 30. Risks and mitigations

### Risk: replay becomes slow
Mitigation: defer snapshots until profiling shows this actually matters.

### Risk: too much schema flexibility causes non-deterministic serialization
Mitigation: typed op structs only; no arbitrary JSON maps in source ops.

### Risk: Windows filesystem semantics complicate durability story
Mitigation: explicitly target Linux/macOS first.

### Risk: implementer spends too long “rustifying” abstractions
Mitigation: keep the design concrete, synchronous, and binary-first.

## 31. Final decision

Build **Mote** as a **Rust 2024 CLI** using a **Maildir-inspired immutable op log** as the source of truth.

The core innovation is not a fancy merge engine. It is this combination:

- immutable create-only writes
- field-scoped optimistic conflict handling
- lease-based claims instead of lock files
- replay-derived state instead of mutable shared state

That is the smallest design here that still feels robust, elegant, and powerful.
