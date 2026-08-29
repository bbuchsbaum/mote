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
| Candidate | `candidate_propose`, `candidate_evidence`, `candidate_review`, `candidate_authorize`, `candidate_revoke`, `candidate_supersede`, `candidate_abandon`, `candidate_landed` |

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
  overlapping paths cannot both be live at once. Closing their issue derives
  an `orphaned` disposition while the TTL is still live: the paths continue to
  block conflicts, but an actor holding a live claim on another open issue may
  adopt them with a compare-and-set transition and recorded provenance. Live
  or expired reservations cannot be adopted. A claim left on closed work is
  likewise shown as orphaned; it cannot be renewed or transferred, only
  released by its owner or allowed to expire.
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
- **Candidates** bind an issue to immutable full Git object ids, declared
  paths, reviewers, evidence producers, and one landing authorizer. Git is
  inspected only by explicit CLI commands, which publish receipts; replay is
  repository-independent and landability fails closed with stable reason
  codes. Every candidate mutation requires an actor-scoped idempotency key.
  Reservations may bind to a pending candidate, but only for paths in its
  immutable declared path set. Landing, abandonment, supersession, or
  authorization revocation makes a still-live candidate binding orphaned and
  path-blocking. Regranting does not silently revive a lease invalidated by an
  earlier revoke.

## CLI quick start

```sh
# Initialize a store in the current directory.
mote init
mote actor set alice
mote actor show
mote actor list
mote actor list --presence live
mote actor list --active-within 10m
mote actor status               # current actor
mote actor status bob           # explicit actor, even without presence evidence

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
mote inbox                       # an empty result names actor and identity source
mote --json inbox --wait --timeout 60  # pending mail, or one bounded wait
mote --json inbox --follow       # current unacked messages, then new deliveries
mote msg ack msg-...
mote msg reply msg-... --kind response "tests are passing"
# One ordinary message can explicitly fulfill several incoming requests.
mote msg send --to alice --answers msg-... --answers msg-... "both are done"
mote msg requests --state responded
mote msg resolve msg-...
mote msg thread bob              # full two-sided history with one actor
mote msg thread bob --kind fyi --issue bd-...

# Discussion plane.
mote discuss topic new planning --title "Planning" --description "Coordination thread"
mote discuss post --topic planning --body "proposal: split parser and test work"
mote discuss post --topic planning --reply-to post-... --body "I can take tests"
# A public post can explicitly fulfill an incoming request too.
mote discuss post --topic planning --answers msg-... --body "public result"
mote discuss sticky post-...
mote discuss supersede post-OLD post-NEW
mote discuss retract post-... --reason "premise disproved"
mote discuss list --topic planning
mote discuss search parser
mote --json discuss unread --page --limit 100
mote --json discuss unread --page --before post-... --limit 100
mote discuss mark-read --through post-...
mote discuss watch planning
mote discuss notifications --topic planning
mote discuss unwatch planning
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

# Explicit public-post attention; repeat --notify for multiple actors.
mote discuss post --topic planning --notify bob --notify carol \
  --idempotency-key planning-update-1 --body "Review requested"

# Session plane: one identity per session, not one per checkout.
eval "$(mote session start --as parser-session --label 'parser work')"
mote session list
mote session status working --message 'implementing parser' --issue bd-...
mote session heartbeat --ttl 15m --renew-within 5m
mote session end

# Candidate plane: Mote records review and exact Git evidence; it never lands.
mote candidate propose --issue bd-... --base origin/main \
  --path src/lib.rs --authorizer release-owner --reviewer reviewer \
  --idempotency-key proposal-1
mote candidate evidence refresh cand-... --idempotency-key ancestry-2
mote candidate review cand-... approve --idempotency-key review-1
mote candidate authorize cand-... --grantee landing-agent \
  --idempotency-key grant-1
mote candidate show cand-...
# After an external Git operation makes the commit reachable from the target:
mote candidate landed cand-... --target origin/main \
  --expect-phase OP_ID --expect-authorization OP_ID \
  --idempotency-key landed-1

# Path plane.
mote reserve src/auth/ tests/auth/ --issue bd-... --ttl 3600
mote reserve src/auth/token.rs --candidate cand-...  # declared candidate path only
mote unreserve rv-...
mote adopt rv-... --issue bd-successor  # adopter must claim the open successor first
mote preflight --issue bd-... --paths src/auth/ tests/auth/
mote who-has src/auth/token.rs

# TTLs accept bare seconds for compatibility or one human suffix.
mote claim bd-... --ttl 15m
mote session renew --ttl 2h

# Compounds (each is a sequence of single-mutation ops with compensation on partial failure).
mote begin   bd-... --paths src/auth/ --note "taking auth"
mote begin   bd-... --paths src/auth/ --announce planning  # also post the claim
mote handoff bd-... --to bob --note "tests remain" --release
mote done    bd-... --note "shipped"
mote board
mote in-flight            # sessions, reservations, doing work, topics, candidates

# Diagnostics.
mote doctor
mote fsck --clean-tmp

# Batch/import. Omitted input path means stdin.
mote batch < plan.jsonl
mote import plan.json

# Oversight (read-only).
mote watch                # human-readable snapshots that re-render on store changes
mote --json watch         # newline-delimited JSON for piping into other tools
mote --json events --kind message,reservation,candidate
mote --json events --kind message --for-actor bob --follow
mote ui                   # interactive TUI dashboard (q to quit, ? for help)
mote serve                # local console on 127.0.0.1:7717
mote serve --port 0       # same loopback-only server on an available port
```

Most agent-facing commands accept `--json` for machine-readable output.
Run `mote help --all` for one sorted list of every executable leaf command and
its concise usage; `mote --json help --all` returns the same introspected
surface as records with `path`, `usage`, and `about`. Because this view is
generated from the Clap command tree, newly added nested commands appear
without a separately maintained catalog.
TTL-bearing claim, reservation, adoption, begin, and session commands accept
bare seconds plus whole-number `s`, `m`, `h`, and `d` forms. The op log and
JSON continue to record normalized integer seconds.

### Literal text from stdin

For multiline or shell-sensitive technical prose, prefer explicit stdin input
over an argv string. Options that already carry text use `-` as the value:

```sh
mote new "Parser follow-up" --body - < issue-body.md
mote discuss post --topic planning --body - < proposal.md
mote begin bd-... --paths src/parser.rs --note - < progress.md
```

Commands whose body is positional use `--stdin` instead:

```sh
mote note bd-... --kind decision --stdin < decision.md
mote msg send --to bob --issue bd-... --kind request --stdin < request.md
mote msg reply msg-... --kind response --stdin < response.md
```

The explicit forms preserve UTF-8 text, including newlines, backticks, quotes,
angle brackets, dollar-parentheses, Unicode, and leading dashes, without shell
interpretation. Mote never reads stdin for these commands unless `--body -`,
`--note -`, or `--stdin` is present. Supplying both positional text and its
stdin form is rejected.

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
  actor presence summaries, live session leases, active and orphaned path
  reservations, `doing` beads with their claim holder, topics active inside
  `--minutes`, and candidate phases with structured landability reasons.
  Everything but the commit list is replayed from the op log; recent commits
  are read from Git, labelled advisory, and suppressed by `--no-git`.
- `mote events` emits one accepted operation event per line. Filter categories
  with `--kind issue,claim,reservation,message,discussion,session,presence,candidate`, and filter events
  authored by or directly related to an actor with `--for-actor`. An explicit
  global `--actor` is shorthand for `--for-actor` on this read-only command.
  `--follow` waits for new ops using filesystem notifications plus the
  `--interval` fallback scan. Without `--follow`, existing matching events are
  emitted and the command exits.
  Reservation TTL observations are the deliberate exception to the
  operation-backed source: `reservation.expiring` and `reservation.expired`
  are synthetic read-only projections with stable cursorable ids, the original
  open op in `op_id`, and `reason=ttl_elapsed` after the deadline. They never
  mint a fake close operation. The warning begins in the final 10% of the TTL
  (at least one second and at most five minutes). Because Mote is daemonless,
  warnings and expiry events exist only when `events --follow`, `watch`, the
  TUI, or another polling view is running; a process that was not polling may
  observe the expired state later without receiving the earlier warning.
  Actor presence uses the same mechanism. Raw `session.started`,
  `session.heartbeat`, `session.status_changed`, and `session.ended` traffic
  remains in the `session` category. The quieter `presence` category emits
  actor-level `presence.live` and `presence.ended` transitions plus synthetic
  `presence.expiring` and `presence.expired` lease boundaries. Synthetic IDs
  are stable resume cursors and never represent fake session-end operations.
  An explicit end of the actor's final live session suppresses later TTL
  expiry. If polling resumes only after a deadline, it emits `expired` and does
  not manufacture the missed `expiring` warning.
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
- `mote msg thread <peer>` prints every message exchanged with one actor in
  send-order, in both directions, including acked messages and plain
  `note`/`fyi` traffic. `mote inbox` is inbound and unacked-only and `mote msg
  requests` is limited to request roots, so neither can reconstruct a
  conversation. `--json` adds a `direction` field (`in` | `out`) relative to
  the current actor; `--issue` and `--kind` filter the thread.
- Direct sends are durable mailbox delivery for every recipient state,
  including unseen and expired actors. Send diagnostics, request/thread JSON,
  inbox JSON, and message events record the recipient's state, evidence source,
  reason, and send-time timestamp. Use `msg send --require-live` only when the
  action is meaningful for a recipient with a valid session lease; the reducer
  rejects the send at the exact message timestamp otherwise. This guard does
  not acknowledge or fulfill anything. Direct messages are addressed, not
  private: anyone who can read the shared store can read them. Human sends keep
  the bare message id on stdout for shell compatibility and print delivery
  evidence on stderr; `--json` returns one structured delivery result.
- Request messages have a lifecycle independent of acknowledgement. `msg ack`
  means only that the recipient saw the delivery. `msg reply` records a
  correlated `response` or `decline`; the original sender then closes the
  lifecycle with `msg resolve`. `msg requests --state
  open|responded|declined|resolved` lists request roots involving the current
  actor. Sender-scoped `--idempotency-key` values make identical send/reply
  retries return the original message id without creating a duplicate message.
  A request recipient can instead put repeatable `--answers <msg-id>` flags on
  one direct message or public discussion post. Mote validates the complete
  answer set before applying any part of it and records the answering message
  or post on every fulfilled request. Ordinary prose never changes request
  state; the explicit flag is required.
- Discussion unread pages are always chronological; sticky status never
  reorders this cursor stream. `unread --limit N` selects the newest N unread
  posts in the selected range. Ordinary JSON remains the historical post
  array; add `--page` to receive an object containing `posts` plus `page`
  boundary metadata, including the exact first, last, and snapshot-last post/op
  ids and `has_older`/`has_newer`. To inspect a large backlog safely, save the
  first page's `snapshot_last_post_id`, then request older pages with `--before
  <first_post_id> --limit N` until `has_older` is false. Finally run
  `mark-read --through <saved-snapshot-last-post-id>` (and the same `--topic`,
  if used). Posts appended during paging remain unread. `--through` rejects an
  unknown post, a topic mismatch, or a boundary older than the effective
  cursor; omitting it retains the convenience behavior of marking through the
  current head.
- Topic watches route future external posts into durable attention without
  creating channel membership. `discuss watch <topic>`, `unwatch`, and
  `watches` maintain the explicit subscription register; `discuss
  notifications` lists only unread posts routed by a watch or repeatable
  `--notify <actor>` flags. It uses the same `--topic`, `--limit`, `--before`,
  and `mark-read --through` cursor rules as ordinary unread discussion.
  Publishers are excluded from their own notifications, named recipients need
  not be online, and words resembling at-mentions in the body have no routing
  effect. Notification metadata never changes public board visibility or adds
  access control. Author-scoped post `--idempotency-key` retries return the
  original post and do not duplicate attention.
- Discussion corrections are immutable links. `discuss supersede OLD NEW`
  leaves both bodies visible and marks the old post `superseded-by:NEW`; the
  replacement lists what it supersedes. `discuss retract POST --reason ...`
  likewise retains the body and concise single-line reason. Only the author may retract, and the
  same actor must author both sides of a same-topic supersession. Self-links,
  cross-topic links, cycles, unknown posts, and competing changes to a stale
  post are rejected in deterministic replay order. List, thread, search,
  unread, JSON, events, and TUI surfaces expose the disposition and provenance
  rather than hiding obsolete history.
- `mote ui` opens a six-tab terminal dashboard (Overview / Beads / Candidates
  / Discussion / Activity / Agents). Candidate detail retains immutable Git anchors,
  policy, reviews, authorization, supersession, and structured landability
  reasons, so unavailable evidence is visibly blocked. It also has full
  per-bead detail, recent op history (including rejected ops with their reasons),
  and incremental refresh on filesystem events. The Discussion tab reads like a forum: threads are indented under
  their parent post, `→`/`Enter` focuses the thread pane, `j`/`k` (or `n`/`p`)
  jump post to post, and `u` jumps to the next unread post.

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

`session heartbeat`, `session status`, and `session end` publish under the
invoking actor, so only the owning identity can mutate that session. Status is
session-scoped and may be `available`, `working`, `waiting`, `blocked`, or
`away`; concurrent sessions sharing an actor keep independent intents. Ending
is terminal. The compatibility command `session renew` now publishes the same
explicit heartbeat operation as `session heartbeat --force`; old stores whose
clients renewed with repeated `session_start` operations still replay those
records as heartbeats.

Heartbeat is write-budgeted for the append-only log. Without `--force`, it
publishes only once the remaining lease is within `--renew-within` (five
minutes by default). A general-purpose integration can poll freely while using
a 15-minute TTL and five-minute margin, producing at most about six accepted
heartbeats per hour. Use `--idempotency-key` when retrying a heartbeat or
status write after an uncertain transport result; an identical retry returns
the first result without another op or event.

Sharing one identity across concurrent sessions is lossy: messages, claims,
and bylines cannot distinguish the sessions. More importantly, when multiple
session leases are live, Mote refuses actor-attributed writes whose identity
comes only from `.mote/local/actor`. Activate a session, export `MOTE_ACTOR`, or
pass `--actor`; read-only diagnosis and `session start --as ...` remain
available. This guard prevents a concurrent `mote actor set` from silently
retagging another live process. Explicit flag and environment identities retain
their normal precedence, including for existing single-user scripts.

New reservation operations reject a same-actor exact or prefix overlap and
name the existing reservation; `preflight` reports it as
`same_actor_duplicate` so a release cannot appear successful while a hidden
duplicate remains. `mote doctor` reports the exact session-activation remedy,
overlaps replayed from older v1 stores, concurrent leases sharing an actor, and
generic actor names like `claude` or `agent`. It does not infer concurrency from
process ids — every mote invocation is its own process, so that would flag
ordinary sequential use.

When separate Git worktrees should coordinate through one store, also export
the same store root or its parent in both terminals:

```sh
export MOTE_STORE=/path/to/main-checkout/.mote
```

`mote actor status` returns the stable `mote.actor-status.v1` projection. It
keeps valid session presence, substantive work, interaction, held work, and
pending attention separate. A valid lease is the only `live` assertion;
sessionless activity may be `recent`, but never silently upgrades to online.
An actor known only because it received a message is `untracked` until it
authors qualifying activity. Heartbeats count as observed presence evidence,
not work or interaction.

`mote actor list` preserves its legacy last-activity, active/orphaned lease,
inbox, and request-count fields and embeds the same projection under `status`.
Use `--presence live|recent|expired|untracked` or `--active-within <duration>`
to filter that projection. `board`, `in-flight`, and `watch` expose actor arrays
sampled at the same `as_of_ts` as the rest of their snapshot. The TUI's Agents
tab shows the same source, reason, lease, intent, work, and attention evidence,
including separate rows for concurrent sessions. Board, preflight, who-has,
in-flight, watch, events, and the TUI preserve the same active/orphaned
distinction; orphaned reservations remain conflict-producing.

### Actor and store resolution

Actor identity is resolved in this exact order:

1. `--actor` CLI flag
2. `MOTE_ACTOR` environment variable
3. `.mote/local/actor` file

If unresolved, mutating commands exit with code 3.

For a human-readable empty inbox, Mote prints the resolved actor and source so
silence cannot be mistaken for checking somebody else's mailbox. JSON inbox
output remains the backward-compatible message array (`[]` when empty); pair it
with `mote --json actor show` when a machine consumer also needs the actor and
source diagnostic.

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
- `candidate_protocol.md` — normative candidate, review, evidence, and landing protocol
- `src/` — Rust crate
- `tests/` — integration tests (storage, issue plane, notes/ready, claims/msgs,
  event delivery, coordination, replay determinism, crash/failpoint, property,
  JSON-output, and stress coverage)
