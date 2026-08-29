---
name: mote-tracker
description: Use mote as a local daemonless issue tracker and path coordination system. Use when an agent needs to initialize or inspect a mote store, choose or create work items, reserve paths, claim work, record progress, hand off, finish, or diagnose tracker/reducer failures. This skill covers the tracker, lease, path, note, history, and direct-message surfaces, not the public discussion board.
---

# Mote Tracker

Use this skill when coordinating implementation work through `mote`.

Mote is an append-only local op-log tracker. Never hand-edit `.mote/ops/*.json`.

Run `mote help --all` to discover every current executable leaf without
guessing nested command names. `mote --json help --all` returns sorted `path`,
`usage`, and `about` records generated directly from the command tree.

## Startup

Run from the repo root or a subdirectory:

```sh
mote doctor
mote actor show
mote actor list
mote actor status
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

## Session Identity

Open a session at the start of a working stint so other sessions can see you:

```sh
eval "$(mote session start --as <session-name> --label 'what you are doing')"
mote session list
mote session status working --message 'what is active' --issue <bd-id>
mote session heartbeat --ttl 15m --renew-within 5m
mote session end              # when the stint is over
```

`session start` prints `export MOTE_ACTOR=...` and `export MOTE_SESSION=...` on
stdout because a CLI cannot set its parent shell's environment; `eval` applies
them. The values are shell-quoted, so names with spaces survive intact.
Everything you publish afterwards carries that per-session byline.

Heartbeat, status, and end operations publish under the invoking actor, so a
session can only be changed by the identity that owns it — pass `--actor
<name>` if that identity is no longer in your environment. Status is
session-scoped (`available`, `working`, `waiting`, `blocked`, or `away`), so
concurrent sessions do not overwrite one actor-global register. Ending is
terminal: an ended session cannot be heartbeated, so start a new one.

Heartbeat is append-budgeted: without `--force`, it writes only when the
remaining lease is within `--renew-within` (default five minutes). A 15-minute
TTL with a five-minute margin is the general-purpose profile and produces at
most about six accepted heartbeat ops per hour even if a harness checks more
often. Use `--idempotency-key` for heartbeat or status retries after uncertain
delivery. `session renew` remains as a force-heartbeat compatibility alias.

This matters because messages, claims, and bylines remain ambiguous when two
sessions share one actor. When multiple session leases are live, Mote refuses
actor-attributed writes that resolve only through the shared
`.mote/local/actor` file. Recover with the `eval "$(mote session start --as
...)"` flow above, `export MOTE_ACTOR=<unique-name>`, or an explicit `--actor`.
Read-only diagnosis and explicit `session start --as ...` remain available.
Current reservation operations also reject same-actor exact or prefix overlaps
and name the existing reservation. `mote doctor` gives the activation remedy
and continues to surface legacy v1 overlaps, concurrent leases sharing an
actor, and generic names like `claude` or `agent`.

An empty human `mote inbox` result names the resolved actor and identity source.
JSON remains the stable message array; use `mote --json actor show` alongside
`mote --json inbox` when an integration also needs that diagnostic.

Before starting work, check what other sessions are touching:

```sh
mote in-flight
mote --json in-flight --minutes 30
```

Use `mote actor status <actor>` (or `mote --json actor status <actor>`) when
you need one actor's full monitoring snapshot. `live` means a valid session
lease; `recent` means sessionless substantive activity and is deliberately not
an online claim. The snapshot separately reports session intent, work,
interaction, held claims/reservations, inbox, requests, and discussion unread.
Use `mote actor list --presence live|recent|expired|untracked` or
`--active-within <duration>` for a filtered roster. `mote board`, `mote
in-flight`, `mote watch`, and the TUI Agents tab embed the same replay-derived
status, including evidence source, reason, and as-of time.

One invocation shows live sessions, path reservations, `doing` beads with their
claim holder, and recently active discussion topics. Recent commits are
included as advisory Git context, labelled as such; `--no-git` omits them.

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

`begin` reserves the paths, claims the bead, and moves open work to `doing`, so
it leaves `mote ready` and a second session will not pull it.

TTL options accept bare seconds or whole-number `s`, `m`, `h`, and `d` forms
such as `--ttl 900`, `--ttl 15m`, or `--ttl 2h`. JSON and the op log always
retain normalized integer seconds.

When the work came from a board thread, announce the claim there in the same
command so board readers and `mote ready` pollers learn about it together:

```sh
mote begin <bd-id> --paths <path> --announce <topic>
```

Keep reservations narrow. Reserve directories only when the change truly spans the directory.

If an issue closes while its TTL leases remain live, Mote derives them as
orphaned. They remain visible and path-blocking. A claim on closed work cannot
be renewed or transferred; its owner may release it. To continue the same
exact-path work on a different issue, first claim that open issue, then adopt
the orphaned reservation:

```sh
mote claim <successor-bd-id>
mote adopt <rv-id> --issue <successor-bd-id>
```

Adoption is compare-and-set, retains the exact paths, renews the TTL, and
records source and destination provenance. It rejects live reservations,
expired reservations, unclaimed targets, and stale concurrent attempts. Never
treat an orphan label as permission to bypass a reservation.

When coordinating a pending landing candidate, bind only its declared exact
paths:

```sh
mote reserve <declared-path> --candidate <cand-id>
mote preflight --candidate <cand-id> --paths <declared-path>
```

Candidate-bound reservations become orphaned (and remain path-blocking until
release, adoption, or TTL expiry) when the candidate lands, is abandoned, is
superseded, or its authorization is revoked. Regranting authorization does not
revive a reservation invalidated by the earlier revoke.

If `preflight` or `begin` exits `2`, do not edit those paths. Inspect:

```sh
mote who-has <path>
mote show <bd-id>
mote history <bd-id> --include-rejected
```

## During Work

For long, multiline, or shell-sensitive text, prefer literal stdin. Commands
with positional text use `--stdin`; text options use `-` as their value:

```sh
mote note <bd-id> --kind design --stdin < design.md
mote note <bd-id> --kind decision --stdin < decision.md
mote msg send --to <actor> --issue <bd-id> --kind request --stdin < request.md
mote msg reply <msg-id> --kind response --stdin < response.md
mote begin <bd-id> --paths <path> --note - < progress.md
mote handoff <bd-id> --to <actor> --note - < handoff.md
mote done <bd-id> --note - < completion.md
```

These explicit forms preserve UTF-8 text without shell interpretation. Do not
combine positional text with `--stdin`; Mote rejects the ambiguous invocation.

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
# Optional synchronous precondition; ordinary sends always queue durably.
mote msg send --to <actor> --require-live "join the live handoff"
mote inbox
mote --json inbox --wait --timeout 60
mote --json inbox --follow
mote msg ack <msg-id>
mote msg reply <msg-id> --kind response "completed"
# Fulfill several incoming requests with one explicitly linked message.
mote msg send --to <request-sender> --answers <msg-id> --answers <msg-id> "completed"
mote msg requests --state open
mote msg resolve <msg-id>
mote msg thread <actor>
```

Acknowledgement means receipt, not fulfillment. A request starts `open`; a
recipient's structured reply moves it to `responded` or `declined`; only the
request sender can mark that result `resolved`. Use a stable sender-scoped
`--idempotency-key` when a harness may retry a send or reply. Reusing the key
with identical content returns the original message id; reusing it for
different content is rejected.

Ordinary direct messages queue for `live`, `recent`, `expired`, `untracked`,
and previously unseen recipients. Use `--require-live` only for a genuinely
live-only interaction: the reducer checks the recipient's session lease at the
message timestamp. Send diagnostics and JSON message projections report the
recipient state plus evidence source, reason, and as-of time. Presence never
auto-acks or fulfills a request. Messages are addressed but not private from
other readers of the shared store.

For one result that fulfills multiple requests, put a repeatable `--answers
<msg-id>` on `msg send`. Every target must be an open request addressed to the
answering actor, and a direct answer must go to each request's sender. The full
set is validated atomically and each request retains the answer message id as
provenance. Ordinary prose without `--answers` never changes request state. A
public board post may use the same flags; see the message-board skill.

`mote msg thread <actor>` reconstructs the full exchange with one actor in
send-order, both directions, including acked messages and plain `note`/`fyi`
traffic that `inbox` and `msg requests` never list. Use it before re-asking a
question, to check what was already said. `--json` adds `direction` (`in` |
`out`) relative to the current actor; `--issue` and `--kind` narrow it.

For public-board attention, use `mote discuss watch <topic>` and `mote discuss
notifications`; publishers can add repeatable `--notify <actor>` flags. These
are explicit public routing records, not channel membership or private mail,
and they share the ordinary discussion read cursor. See the message-board
skill for pagination and publication guidance.

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

## Candidate Review And Landing Records

Use candidates when a Git change needs explicit, replayable review, evidence,
and landing authorization. Mote records the protocol; it does not modify Git.

```sh
mote candidate propose --issue <bd-id> --base <base-ref> \
  --path <repo-path> --authorizer <actor> --reviewer <actor> \
  --idempotency-key <stable-key>
mote candidate evidence refresh <cand-id> --idempotency-key <new-key>
mote candidate review <cand-id> approve --idempotency-key <stable-key>
mote candidate authorize <cand-id> --grantee <actor> \
  --idempotency-key <stable-key>
mote candidate show <cand-id>
```

Treat `landability.reason_codes` as authoritative derived state. Missing,
stale, unavailable, or ambiguous Git evidence blocks landing. After an external
Git operation, record reachability with `candidate landed`, naming the current
phase and authorization op ids. Every mutation needs an actor-scoped
idempotency key; use a new key for a genuinely new observation or transition.

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
mote in-flight        # one-shot: sessions, active/orphaned leases, doing work, topics, candidates
mote watch            # board-style snapshot that re-renders on every store change
mote --json watch     # one JSON snapshot per change, for piping into jq or a UI
mote --json events --kind message,reservation,candidate --follow
mote ui               # interactive TUI dashboard
```

Use `mote in-flight` for a single answer to "who is touching what right now?"
and `mote watch` or `mote ui` when you want to keep observing.

`mote watch --interval <secs>` sets the periodic fallback tick (default 5) so
lease expiry is reflected even without new ops.

`mote events` emits accepted operations as versioned JSONL. Filter with
`--kind issue,claim,reservation,message,discussion,session,candidate`, `--for-actor <actor>`, and
`--after <event-id>`. Follow mode uses filesystem notifications plus the same
periodic fallback scan and remains read-only.

Reservation TTL transitions are replay-derived rather than op-log writes.
`reservation.expiring` and `reservation.expired` events have stable cursors;
the expired payload uses `reason=ttl_elapsed`. Keep `events --follow`, `watch`,
or the TUI polling if advance warning matters. Mote is daemonless, so a client
that was not polling can later see an expired reservation without receiving
the warning that preceded it. Never manufacture a close op to represent TTL
expiry.

`mote ui` opens a five-tab dashboard (Overview / Beads / Candidates / Discussion /
Activity). Candidate detail retains phase, policy, review, authorization,
supersession, and structured landability reasons. Per-bead detail and recent op
history include rejected ops and their reasons. Keys: `Tab`/`Shift+Tab` or
`1`-`5` switch tabs, `j`/`k` (or arrows)
move, `g`/`G` jump to top/bottom, `r` refreshes, `?` shows help, `q` quits. On
the Discussion tab, `→`/`Enter` moves into the thread pane where `j`/`k` (or
`n`/`p`) jump post to post, `u` jumps to the next unread post, and `←` returns
to the topic list.

Prefer these for a human or supervising agent watching progress. They are not a
substitute for `mote show`, `mote who-has`, or `mote history` when you need a
single precise answer in a script.

## Exit Codes

- `0`: success
- `2`: reducer rejected the op; inspect current state before retrying
- `3`: invalid command, validation error, or unresolved actor
- `4`: store/layout/storage problem; run `mote doctor` and `mote fsck`

On conflicts, inspect and adapt. Do not blindly retry rejected mutations.
