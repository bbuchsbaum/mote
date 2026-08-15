# mote

A lock-light, daemonless local task tracker for one developer running multiple
coding agents on the same machine.

The source of truth is not a database and not a single append-only file. It is
a directory of immutable operation files — Maildir-inspired, deterministic on
replay, with no mutable shared state on the write path.

> **Status:** v0.2 working draft. POSIX-only (Linux/macOS), local filesystem,
> single workstation. See `PRD.json` for the consolidated working spec.

## Why

Existing trackers solve a bigger problem than this product needs. For one
developer plus a few local agents doing tiny task updates:

- concurrent multi-process writes on one machine, with no daemon
- crash-safe publication: a process crash never produces a visible partial write
- deterministic state from immutable ops; no mutable cache that matters for
  correctness
- conflicts are field-scoped, not whole-record; unrelated edits never collide
- claims and reservations are TTL leases, not lock files
- the whole codebase is small enough to read in an afternoon

## Storage layout

```
.mote/
  FORMAT.json    # schema version, store id, default TTLs
  tmp/           # Maildir-style staging
  ops/           # immutable published ops, source of truth
  local/         # convenience state (e.g. actor identity); not source of truth
```

### Repository policy

For normal single-workstation use, keep `.mote/` out of git and treat it like
local coordination state. Add `.mote/` to the host repository's `.gitignore`
unless you have explicitly decided to version the op log.

If the task history matters, back up `.mote/ops/`; those immutable operation
files are the source of truth. Do not hand-edit op files. Use `mote fsck` or
`mote doctor` when you suspect storage damage.

Every mutation:

1. write `tmp/<name>.json` with `O_CREAT|O_EXCL`, fsync the file
2. `link()` from `tmp/` into `ops/` (fails on EEXIST — never silently overwrites)
3. fsync the `ops/` directory entry
4. unlink the tmp file

Op filenames are sortable: `YYYYMMDDTHHMMSS.UUUUUUZ-p<pid>-c<ctr>-r<rand>-h<hash6>.json`.
The 6-hex content hash is a corruption / debug aid, not the primary identity.

## Coordination planes

All planes share the same publication mechanism and reducer.

| Plane    | Op kinds                                                              |
|----------|-----------------------------------------------------------------------|
| Issue    | `create`, `patch`, `tag_add`, `tag_remove`, `dep_add`, `dep_remove`, `rel_add`, `rel_remove`, `note`, `close`, `delete` |
| Path     | `reserve_open`, `reserve_close`                                        |
| Message  | `msg_send`, `msg_ack`                                                  |
| Discussion | `board_topic`, `board_post`, `board_sticky`, `board_read`, `board_route` |
| Lease    | `claim`, `release`                                                     |
| Session  | `session_start`, `session_end`                                          |

### Conflict semantics

- **Scalar fields** (`title`, `status`, `priority`, `body`, `assignee`) carry
  per-field clocks. A `patch` declares expectations matching current clocks for
  the fields it touches. Two patches to disjoint fields both succeed; two
  patches to the same field — exactly one is accepted, the other is recorded
  as a rejected intent with reason.
- **Set fields** (`tags`, `deps`, `rels`) use commutative idempotent ops; never conflict.
- **Dependencies** are blocking edges. Every `dep` edge kind still blocks
  readiness for compatibility, including old `--kind parent` edges.
- **Relations** are non-blocking hierarchy edges. They are visible in `show`,
  `parents`, and `children`, but `ready` ignores them.
- **Append-only** (`note`, `msg_send`) never conflict.
- **Claims** are TTL leases. Expired leases auto-yield. Stale agents do not
  leave permanent locks. Self-actor renewal is auto-accepted.
- **Path reservations** are advisory leases over directory or exact-file
  paths. All-or-nothing accept. Two reservations from different actors over
  overlapping paths cannot both be live at once.
- **Discussion topics and posts** are append-only public board entries. Topics
  can be created before any posts exist, topic listings show creation and last
  activity times, and search covers both topics and posts. Posts have stable
  `post-...` identities, can optionally reply to another post, and can be
  marked sticky. Actors can mark the board read to poll for new external posts
  later, either globally or for a single topic. Thread views show a post with
  its nested reply history so agent reasoning remains reconstructable.
- **Discussion routing** links a post or topic to the beads it produced.
  `route_state` is `open` (nothing declared), `needs_bead` (actionable, not yet
  tracked), `routed` (linked to one or more beads), or `resolved` (no tracker
  action needed). Links accumulate — routing a post to a second bead does not
  erase the first. Because only an explicit `needs_bead` counts as unrouted,
  `mote discuss unrouted` reports declared state rather than guessing from
  prose.
- **Session leases** are TTL leases over an identity rather than a work item.
  Several sessions may legitimately share one actor name; a lease is what makes
  each of them individually visible to `mote in-flight` and `mote doctor`.

## CLI quick start

```sh
# Initialize a store in the current directory.
mote init
mote actor set alice
mote actor show
mote actor list

# Issue plane.
mote new "Fix auth bug" -p 1 --tag backend
mote ls
mote set bd-... status=doing
mote dep add bd-CHILD bd-PARENT
mote rel add bd-CHILD bd-PARENT --kind parent
mote children bd-PARENT
mote dependents bd-PARENT
mote tag add bd-... refactor reviewed
mote ls --tag m1 --tag task
mote note bd-... --kind progress "parser changes done"
mote ready
mote close bd-...
mote history bd-... --include-rejected

# Lease plane.
mote claim bd-... --ttl 1800
mote release bd-...

# Message plane.
mote msg send --to bob --issue bd-... --kind request \
  --idempotency-key tests-request-1 "please take tests"
mote inbox
mote --json inbox --wait --timeout 60  # pending mail, or one bounded wait
mote --json inbox --follow       # current unacked messages, then new deliveries
mote msg ack msg-...
mote msg reply msg-... --kind response "tests are passing"
mote msg requests --state responded
mote msg resolve msg-...

# Discussion plane.
mote discuss topic new planning --title "Planning" --description "Coordination thread"
mote discuss post --topic planning --body "proposal: split parser and test work"
mote discuss post --topic planning --reply-to post-... --body "I can take tests"
mote discuss sticky post-...
mote discuss list --topic planning
mote discuss search parser
mote discuss unread
mote discuss mark-read --topic planning
mote discuss replies post-...
mote discuss thread post-...
mote discuss topics

# Or create a topic and seed a visible first post:
mote discuss topic new planning-2 --title "Planning 2" --body "Initial proposal"

# Discussion routing: keep the argument on the board, the execution in beads.
mote discuss decision --topic planning --body "Consensus: split parser first"
mote discuss summary  --topic planning --body "Current state: 1 open question"
mote discuss summary  --topic planning          # read the pinned summary back
mote discuss needs-bead post-...                # actionable, not yet tracked
mote discuss route post-... --issue bd-...      # link an existing bead
mote discuss route --topic planning --issue bd-...
mote discuss promote post-... --title "Split the parser" --tag parser
mote discuss resolve post-...                   # no tracker action needed
mote discuss unrouted                           # what still needs a bead

# Session plane: one identity per session, not one per checkout.
eval "$(mote session start --as parser-session --label 'parser work')"
mote session list
mote session renew --ttl 7200
mote session end

# Path plane.
mote reserve src/auth/ tests/auth/ --issue bd-... --ttl 3600
mote unreserve rv-...
mote preflight --issue bd-... --paths src/auth/ tests/auth/
mote who-has src/auth/token.rs

# Compounds (each is a sequence of single-mutation ops with compensation on partial failure).
mote begin   bd-... --paths src/auth/ --note "taking auth"
mote begin   bd-... --paths src/auth/ --announce planning  # also post the claim
mote handoff bd-... --to bob --note "tests remain" --release
mote done    bd-... --note "shipped"
mote board
mote in-flight            # sessions, reservations, doing work, active topics

# Diagnostics.
mote doctor
mote fsck --clean-tmp

# Batch/import. Omitted input path means stdin.
mote batch < plan.jsonl
mote import plan.json

# Oversight (read-only).
mote watch                # human-readable snapshots that re-render on store changes
mote --json watch         # newline-delimited JSON for piping into other tools
mote --json events --kind message,reservation
mote --json events --kind message --for-actor bob --follow
mote ui                   # interactive TUI dashboard (q to quit, ? for help)
```

Most agent-facing commands accept `--json` for machine-readable output.

`mote batch` reads JSONL, one action per line, and publishes the corresponding
normal ops sequentially:

```jsonl
{"action":"create","id":"epic-1","title":"Epic","tags":["m1","epic"]}
{"action":"create","id":"task-1","title":"Task","relations":[{"parent":"epic-1","kind":"parent"}]}
{"action":"tag_add","id":"task-1","tags":["api","quick"]}
```

`mote import` reads a single JSON object with `beads`, `deps`, and `relations`
arrays. Both commands print accepted/rejected/skipped results; `--json` returns
the same report as structured data.

### Oversight

`mote watch`, `mote events`, follow-mode inboxes, and `mote ui` are passive
viewers — they only read immutable ops and derived state, never publish ops.
They are safe to leave running while agents are writing to the store.

- `mote watch` redraws a board-style summary every time a new op appears, with
  a periodic fallback tick so it also reflects lease expiry. `mote --json
  watch` writes one JSON snapshot per change to stdout, suitable for piping
  into `jq` or any small UI.
- `mote in-flight` answers "what is being touched right now?" in one shot:
  live session leases, path reservations, `doing` beads with their claim
  holder, and topics active inside `--minutes`. Everything but the commit list
  is replayed from the op log; recent commits are read from Git, labelled
  advisory, and suppressed by `--no-git`.
- `mote events` emits one accepted operation event per line. Filter categories
  with `--kind issue,claim,reservation,message,discussion,session`, and filter events
  authored by or directly related to an actor with `--for-actor`. An explicit
  global `--actor` is shorthand for `--for-actor` on this read-only command.
  `--follow` waits for new ops using filesystem notifications plus the
  `--interval` fallback scan. Without `--follow`, existing matching events are
  emitted and the command exits.
- `mote inbox --follow` first emits the actor's existing filtered unacked
  messages, then emits new matching message delivery events (`message.sent`,
  `message.responded`, or `message.declined`). Pass `--after <event-id>` to
  either follow surface to resume from a previously persisted cursor rather
  than replaying current inbox state. Human follow output is a compact message
  line; `--json` retains the stable `mote.event.v1` envelope.
- `mote inbox --wait` returns the current filtered inbox immediately when it is
  non-empty. Otherwise it waits for one matching delivery, replays the inbox,
  prints it in the same shape as ordinary `mote inbox`, and exits. The default
  timeout is 60 seconds; `--timeout 0` is a non-blocking check.
- Request messages have a lifecycle independent of acknowledgement. `msg ack`
  means only that the recipient saw the delivery. `msg reply` records a
  correlated `response` or `decline`; the original sender then closes the
  lifecycle with `msg resolve`. `msg requests --state
  open|responded|declined|resolved` lists request roots involving the current
  actor. Sender-scoped `--idempotency-key` values make identical send/reply
  retries return the original message id without creating a duplicate message.
- `mote ui` opens a four-tab terminal dashboard (Overview / Beads / Discussion
  / Activity) with full per-bead detail, recent op history (including
  rejected ops with their reasons), and incremental refresh on filesystem
  events.

Event JSON is newline-delimited and versioned independently from the durable
op schema. Every `mote.event.v1` envelope contains `event_id` (also the resume
cursor), `store_id`, `type`, `category`, `op_id`, `ts`, `actor`, `accepted`, and
the full kind-specific `data` payload. Consumers should persist `event_id` and
deduplicate by it; the periodic fallback may rescan, but a running stream emits
each filename at most once.

## Agent skills

This repo includes two canonical skills for agents:

- `skills/mote-tracker/` — issue tracking, claims, path reservations, notes,
  handoffs, and direct messages.
- `skills/mote-message-board/` — forum-style public discussion topics, posts,
  replies, threads, sticky posts, search, and unread state.

The `.codex/skills/` and `.claude/skills/` entries are symlinks to those
canonical skill folders, so Codex and Claude use the same source of truth in
this checkout.

### Install skills for your agents

The `mote` binary embeds both canonical skills, so installing them anywhere
else is a single command and does not require the source checkout.

List the skills bundled with this binary:

```sh
mote skills list
```

Install them for the current user (writes `~/.claude/skills/<skill>/` and
`~/.codex/skills/<skill>/`):

```sh
mote skills install --user
```

Install them into another repository (writes
`<repo>/.claude/skills/<skill>/` and `<repo>/.codex/skills/<skill>/`):

```sh
mote skills install --repo /path/to/other/repo
```

Restrict the target agents with `--agent claude` or `--agent codex` (the
default is both). Existing skill directories are left untouched; rerun with
`--force` to overwrite them with the version embedded in the current binary.

Re-running `mote skills install --user --force` after `cargo install ... --force`
is the supported way to refresh installed skills when mote is upgraded.

### Concurrent terminal setup

`.mote/local/actor` is shared by every process using the same store. For two
concurrent agent terminals, give each process its own environment identity
instead of repeatedly changing that shared convenience file:

```sh
# terminal A
export MOTE_ACTOR=agent-a

# terminal B
export MOTE_ACTOR=agent-b
```

`mote session start` does the same thing and leaves a lease behind, so the
other sessions can see you:

```sh
eval "$(mote session start --as agent-a --label 'auth refactor')"
```

A CLI cannot set its parent shell's environment, so `session start` prints the
`export` lines on stdout for you to `eval`; the diagnostics go to stderr. The
values are shell-quoted, so an actor name containing spaces or metacharacters
activates verbatim instead of truncating or executing.

`session renew` and `session end` publish under the invoking actor, so only the
owning identity can renew or end a lease. Ending is terminal.

Sharing one identity across concurrent sessions is not an error, but it is
lossy: same-actor reservations never conflict, so two sessions can hold the
same paths and neither `mote preflight` nor `mote who-has` will say so.
`mote doctor` reports that overlap, concurrent leases sharing an actor, and
generic actor names like `claude` or `agent`. It does not try to infer
concurrency from process ids — every mote invocation is its own process, so
that would flag ordinary sequential use.

When separate Git worktrees should coordinate through one store, also export
the same store root or its parent in both terminals:

```sh
export MOTE_STORE=/path/to/main-checkout/.mote
```

`mote actor list` derives known actors and their last accepted activity, active
claims/reservations, unacknowledged inbox count, and incoming open-request count
without adding registration or heartbeat state.

### Actor and store resolution

Actor identity is resolved in this exact order:

1. `--actor` CLI flag
2. `MOTE_ACTOR` environment variable
3. `.mote/local/actor` file

If unresolved, mutating commands exit with code 3.

Store location is resolved in this order:

1. `--store` CLI flag
2. `MOTE_STORE` environment variable
3. walk upward from the current directory looking for `.mote/`

### Exit codes

- `0` — success / op accepted
- `1` — internal failure
- `2` — op rejected by reducer (stale clock, path overlap, already-acked, etc.)
- `3` — invalid command, validation error, or actor identity unresolved
- `4` — repository / storage error

## Install / Update

Requires macOS or Linux and Rust 1.85+ with `cargo` on your `PATH`.
If `cargo` is not installed, install Rust first from <https://rustup.rs/>.

```sh
cargo install --git https://github.com/bbuchsbaum/mote --locked
mote --version
```

`cargo install` places binaries in `~/.cargo/bin` by default. If `mote` is not
found after install, make sure `~/.cargo/bin` is on your `PATH`.

First run inside a project:

```sh
mote init
mote actor set alice
mote doctor
```

Install the bundled Claude and Codex agent skills (optional but recommended
for agent users — see `Agent skills` below for details):

```sh
mote skills install --user
```

Update an existing install later:

```sh
cargo install --git https://github.com/bbuchsbaum/mote --force --locked
```

Install from a local checkout:

```sh
cargo install --path . --locked
```

## Build

For development builds from a checkout:

```sh
cargo build --release
target/release/mote --version
```

Requires Rust 1.85+ (edition 2024).

## Limitations (v0.2)

- POSIX local filesystems only. No Windows. No NFS.
- No glob-style reservation paths.
- No git integration; use `git worktree add` when two agents really need
  separate `HEAD`/index. Reservations are explicitly advisory, not enforced.
- No distributed sync protocol. Versioning `.mote/` in git is a manual policy
  decision, not the default operational mode.
- No snapshots; replay-from-scratch is the v0.2 read path.

## Project layout

- `PRD.json` — working spec; the source of truth for design decisions
- `mote_prd.md`, `mote_coordination_addendum.md` — historical design rationale
- `src/` — Rust crate
- `tests/` — integration tests (storage, issue plane, notes/ready, claims/msgs,
  event delivery, coordination, replay determinism, crash/failpoint, property,
  JSON-output, and stress coverage)
