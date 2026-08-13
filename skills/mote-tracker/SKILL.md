---
name: mote-tracker
description: Use mote as a local daemonless issue tracker and path coordination system. Use when an agent needs to initialize or inspect a mote store, choose or create work items, reserve paths, claim work, record progress, hand off, finish, or diagnose tracker/reducer failures. This skill covers the tracker, lease, path, note, history, and direct-message surfaces, not the public discussion board.
---

# Mote Tracker

Use this skill when coordinating implementation work through `mote`.

Mote is an append-only local op-log tracker. Never hand-edit `.mote/ops/*.json`.

## Startup

Run from the repo root or a subdirectory:

```sh
mote doctor
mote actor show
mote actor list
mote board
mote ready
mote inbox
mote msg requests --state open
```

If no store exists:

```sh
mote init
mote actor set <stable-actor-name>
mote doctor
```

Use one stable actor name for a workstream, not a new actor per command. When
multiple agent terminals share a store, set a distinct `MOTE_ACTOR` in each
terminal; do not make the processes compete over the shared
`.mote/local/actor` file. When agents in separate worktrees share one
coordination store, set the same `MOTE_STORE` path in each terminal.

## Pick Or Create Work

Inspect ready work:

```sh
mote ready
mote show <bd-id>
```

Create work only when no suitable issue exists:

```sh
mote new "Short concrete title" -p 1 --tag <area>
```

Use `--id <external-id>` only for migration/import workflows. External ids must not start with `bd-`.

## Reserve Before Editing

Before touching files, check exact paths:

```sh
mote preflight --issue <bd-id> --paths <path> [<path> ...]
```

If clear, begin and reserve:

```sh
mote begin <bd-id> --paths <path> [<path> ...] --note "starting"
```

Keep reservations narrow. Reserve directories only when the change truly spans the directory.

If `preflight` or `begin` exits `2`, do not edit those paths. Inspect:

```sh
mote who-has <path>
mote show <bd-id>
mote history <bd-id> --include-rejected
```

## During Work

Record meaningful state changes:

```sh
mote note <bd-id> --kind progress "what changed"
mote note <bd-id> --kind decision "decision and why"
mote note <bd-id> --kind blocker "what is blocked"
```

For directed coordination:

```sh
mote msg send --to <actor> --issue <bd-id> --kind request \
  --idempotency-key <stable-key> "short request"
mote inbox
mote --json inbox --wait --timeout 60
mote --json inbox --follow
mote msg ack <msg-id>
mote msg reply <msg-id> --kind response "completed"
mote msg requests --state open
mote msg resolve <msg-id>
```

Acknowledgement means receipt, not fulfillment. A request starts `open`; a
recipient's structured reply moves it to `responded` or `declined`; only the
request sender can mark that result `resolved`. Use a stable sender-scoped
`--idempotency-key` when a harness may retry a send or reply. Reusing the key
with identical content returns the original message id; reusing it for
different content is rejected.

Use `inbox --wait` at a coordination boundary when the agent should return
pending messages immediately or wait once for a reply. It exits after one
delivery or the timeout and uses the same output shape as ordinary `inbox`.

`--json inbox --follow` emits current unacknowledged messages as
`mote.event.v1` JSONL, then waits for new deliveries. Persist each `event_id`
and resume with `mote --json inbox --follow --after <event-id>` after restarting
a harness. Without `--json`, follow mode prints compact human message lines.

Check `mote inbox` at startup, before a long wait, and before completion or
handoff. Acknowledge a message only after its content has actually been
incorporated into the agent's work; stream delivery alone is not receipt.

Use the public forum-style board via the separate `$mote-message-board` skill.

## Finish Or Hand Off

Complete finished work:

```sh
mote done <bd-id> --note "finished"
```

Hand off unfinished work:

```sh
mote handoff <bd-id> --to <actor> --note "state and next step" --release
```

If stopping without `done` or `handoff`, release claims/reservations:

```sh
mote release <bd-id>
mote unreserve <rv-id>
```

## Oversight (Read-Only)

To observe store state without mutating it, use the passive viewers. They only
replay the op log and never publish ops, so they are safe to leave running while
other agents write.

```sh
mote watch            # board-style snapshot that re-renders on every store change
mote --json watch     # one JSON snapshot per change, for piping into jq or a UI
mote --json events --kind message,reservation --follow
mote ui               # interactive TUI dashboard
```

`mote watch --interval <secs>` sets the periodic fallback tick (default 5) so
lease expiry is reflected even without new ops.

`mote events` emits accepted operations as versioned JSONL. Filter with
`--kind issue,claim,reservation,message,discussion`, `--for-actor <actor>`, and
`--after <event-id>`. Follow mode uses filesystem notifications plus the same
periodic fallback scan and remains read-only.

`mote ui` opens a four-tab dashboard (Overview / Beads / Discussion / Activity)
with per-bead detail and recent op history, including rejected ops and their
reasons. Keys: `Tab`/`Shift+Tab` or `1`-`4` switch tabs, `j`/`k` (or arrows)
move, `g`/`G` jump to top/bottom, `r` refreshes, `?` shows help, `q` quits.

Prefer these for a human or supervising agent watching progress. They are not a
substitute for `mote show`, `mote who-has`, or `mote history` when you need a
single precise answer in a script.

## Exit Codes

- `0`: success
- `2`: reducer rejected the op; inspect current state before retrying
- `3`: invalid command, validation error, or unresolved actor
- `4`: store/layout/storage problem; run `mote doctor` and `mote fsck`

On conflicts, inspect and adapt. Do not blindly retry rejected mutations.
