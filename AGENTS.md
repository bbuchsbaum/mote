# Agent Operating Guide

This repository uses `mote` for local issue tracking and path coordination.
The store is intentionally daemonless and advisory: agents must check and
respect reservations themselves.

## Before Work

1. Confirm a store exists:
   ```sh
   mote doctor
   ```
2. Confirm your actor identity:
   ```sh
   mote actor show
   ```
   If needed, set it once for this checkout:
   ```sh
   mote actor set <actor>
   ```
3. Inspect the current board:
   ```sh
   mote board
   mote ready
   ```

## Starting Work

Before editing files, check the exact paths you expect to touch:

```sh
mote preflight --issue <bd-id> --paths <path> [<path> ...]
```

If clear, begin work and reserve those paths:

```sh
mote begin <bd-id> --paths <path> [<path> ...] --note "starting"
```

If `preflight` or `begin` exits 2, another actor has an accepted overlapping
reservation. Choose a different scope, wait for release, or coordinate by
message or handoff.

## During Work

Use notes for material state changes:

```sh
mote note <bd-id> --kind progress "what changed"
mote note <bd-id> --kind blocker "what is blocked"
mote msg send --to <actor> --issue <bd-id> --kind request "short request"
```

Keep reservations narrow. Reserve directories only when the work truly needs a
directory-wide claim. Use git worktrees for broad or long-running changes that
would otherwise collide through the shared index or `HEAD`.

## Finishing Or Handoff

Complete work with:

```sh
mote done <bd-id> --note "done"
```

Hand off unfinished work with:

```sh
mote handoff <bd-id> --to <actor> --note "state and next step" --release
```

If you stop without using `done` or `handoff`, release claims and reservations
explicitly:

```sh
mote release <bd-id>
mote unreserve <rv-id>
```

## Exit Codes

- `0`: success
- `2`: reducer rejected the op; inspect the stderr reason and current state
- `3`: invalid command, validation problem, or unresolved actor
- `4`: repository or storage problem; run `mote doctor` and `mote fsck`

Never hand-edit `.mote/ops/*.json`. The op log is append-only source of truth.
