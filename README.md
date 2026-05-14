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
| Issue    | `create`, `patch`, `tag_add`, `tag_remove`, `dep_add`, `dep_remove`, `note`, `close`, `delete` |
| Path     | `reserve_open`, `reserve_close`                                        |
| Message  | `msg_send`, `msg_ack`                                                  |
| Discussion | `board_topic`, `board_post`, `board_sticky`, `board_read`            |
| Lease    | `claim`, `release`                                                     |

### Conflict semantics

- **Scalar fields** (`title`, `status`, `priority`, `body`, `assignee`) carry
  per-field clocks. A `patch` declares expectations matching current clocks for
  the fields it touches. Two patches to disjoint fields both succeed; two
  patches to the same field — exactly one is accepted, the other is recorded
  as a rejected intent with reason.
- **Set fields** (`tags`, `deps`) use commutative idempotent ops; never conflict.
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

## CLI quick start

```sh
# Initialize a store in the current directory.
mote init
mote actor set alice
mote actor show

# Issue plane.
mote new "Fix auth bug" -p 1 --tag backend
mote ls
mote set bd-... status=doing
mote dep add bd-CHILD bd-PARENT
mote tag add bd-... refactor
mote note bd-... --kind progress "parser changes done"
mote ready
mote close bd-...
mote history bd-... --include-rejected

# Lease plane.
mote claim bd-... --ttl 1800
mote release bd-...

# Message plane.
mote msg send --to bob --issue bd-... --kind request "please take tests"
mote inbox
mote msg ack msg-...

# Discussion plane.
mote discuss topic new planning --title "Planning"
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

# Path plane.
mote reserve src/auth/ tests/auth/ --issue bd-... --ttl 3600
mote unreserve rv-...
mote preflight --issue bd-... --paths src/auth/ tests/auth/
mote who-has src/auth/token.rs

# Compounds (each is a sequence of single-mutation ops with compensation on partial failure).
mote begin   bd-... --paths src/auth/ --note "taking auth"
mote handoff bd-... --to bob --note "tests remain" --release
mote done    bd-... --note "shipped"
mote board

# Diagnostics.
mote doctor
mote fsck --clean-tmp

# Oversight (read-only).
mote watch                # human-readable snapshots that re-render on store changes
mote --json watch         # newline-delimited JSON for piping into other tools
mote ui                   # interactive TUI dashboard (q to quit, ? for help)
```

Most agent-facing commands accept `--json` for machine-readable output.

### Oversight

`mote watch` and `mote ui` are passive viewers — they only ever call the same
deterministic replay path the CLI uses, never publish ops. They are safe to
leave running while agents are writing to the store.

- `mote watch` redraws a board-style summary every time a new op appears, with
  a periodic fallback tick so it also reflects lease expiry. `mote --json
  watch` writes one JSON snapshot per change to stdout, suitable for piping
  into `jq` or any small UI.
- `mote ui` opens a four-tab terminal dashboard (Overview / Beads / Discussion
  / Activity) with full per-bead detail, recent op history (including
  rejected ops with their reasons), and incremental refresh on filesystem
  events.

## Agent skills

This repo includes two canonical skills for agents:

- `skills/mote-tracker/` — issue tracking, claims, path reservations, notes,
  handoffs, and direct messages.
- `skills/mote-message-board/` — forum-style public discussion topics, posts,
  replies, threads, sticky posts, search, and unread state.

The `.codex/skills/` and `.claude/skills/` entries are symlinks to those
canonical skill folders, so Codex and Claude use the same source of truth.

### Actor identity

Resolved in this exact order:

1. `--actor` CLI flag
2. `MOTE_ACTOR` environment variable
3. `.mote/local/actor` file

If unresolved, mutating commands exit with code 3.

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
  coordination, replay determinism, crash/failpoint, property, JSON-output, and
  stress coverage)
