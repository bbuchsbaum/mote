# Git authority protocol version 1

This is the implementation contract for GA-02, GA-03, and GA-04. It describes
an optional authority for cooperating Mote clients with access to one Git
repository. It does not authorize adoption until the relevant transport and
acceptance gates pass.

## Authority identity and history

The default authority is `refs/heads/mote-state` in the project's selected Git
endpoint. Every source branch, clone, worktree, and machine participating in
that project connects to the same endpoint and authority branch. The metadata
branch has an independent root and is never merged into source history.

A local binding records the endpoint, ref, store ID, generation, root commit
OID, protocol version, and required capabilities. A remote name is a routing
alias, not identity. Connect resolves its single push endpoint and uses that
endpoint for reads and writes. Multiple push endpoints are refused. A fork
must explicitly select its upstream authority. Equal store IDs on two endpoints
do not create a shared authority. Endpoint changes require an explicit binding
transition. A previously bound branch disappearing is an error, not permission
to initialize it again.

The root commit has no parents. Its tree contains exactly `manifest.json` and,
for a migrated store, `legacy.pack`. The manifest is immutable. This example
uses the synthetic engine profile; current bootstrap selects the separate
[announcement-capable profile](git-authority-announcements.md):

```json
{
  "schema": "mote.git-authority.v1",
  "store_id": "st-example",
  "generation": 1,
  "ownership": "explicit-release-v1",
  "capabilities": ["canonical-transactions-v1", "explicit-release-v1", "synthetic-cas-v1"],
  "legacy_digest": null
}
```

Store IDs use Mote's `st-` prefix and a nonempty ASCII alphanumeric/hyphen
suffix. Generation is a positive integer. Capability arrays are sorted,
duplicate-free, and exact: unknown required capabilities fail closed. Root OID
is recorded in the local binding, not inside its own manifest. If present,
`legacy_digest` is the full lowercase domain-separated BLAKE3 digest of the
verified legacy export in the packed archive; the pack retains the original
inputs and replay evidence. See the [migration contract](git-authority-migration.md).
The initial engine implements the `synthetic-cas-v1` command profile. The
separately reviewed [coordination core profile](git-authority-coordination.md)
adds ordinary task ownership.
Its exact capability array is `["canonical-transactions-v1", "coordination-core-v1", "explicit-release-v1"]`.
The immutable manifest selects exactly one supported array; a synthetic root
never gains coordination semantics, and inactive-profile requests are refused
before planning and during replay. These two early profiles require roots with
`legacy_digest: null` and no legacy file. Later lifecycle, communications,
packed-migration, retirement and announcement profiles retain their own exact
capability sets. Migrated roots require the separately reviewed packed import
schema and exact retained replay profile; they never reinterpret an old root.

Each subsequent commit has exactly one parent and adds exactly one file:
`transactions/00000000000000000001.json`, then consecutive 20-digit decimal
sequence numbers. It cannot change root data or any earlier transaction. There
are no merge commits, mutable catalogs, snapshots, auxiliary user files, or
deletions in v1. Reject symlinks, submodules, executable blobs, unexpected paths,
invalid JSON, unknown fields, duplicate keys, and unsupported required versions.
One ordinary fast-forward push publishes one direct-parent proposal. Normal
writers must be prevented from deleting or rewriting the remote branch.

A validator checks the entire chain from its bound root before exposing a new
head. A new clone validates from root; a cache can accelerate this only when it
is tied to a previously validated commit. The cache is rebuildable and never
accepts decisions independently. A non-fast-forward observation, unknown root,
or invalid append stops authoritative operation. Clients never repair these
conditions by force-pushing or automatically recreating the branch.

## Requests and transactions

A request contains its complete normalized intent:

```json
{
  "version": 1,
  "request_id": "req-example",
  "actor": "alice",
  "session": "sess-example",
  "timestamp": "2026-10-20T14:00:00Z",
  "command": {"kind": "synthetic.cas", "changes": [{"key": "task-1", "expected": null, "value": "alice"}]}
}
```

The timestamp is descriptive client metadata, not a lease or acceptance clock.
It is supplied once and retained across retries. Request IDs contain 1–100
ASCII letters, digits, hyphens or underscores. Actor and session identifiers
are nonempty bounded strings. An operation that creates an entity records its
generated IDs in the request before submission. Entity IDs and explicitly
supplied expectations remain fixed across attempts.

Resolve stdin before journaling. Normalize project-relative paths with the
existing Mote path rules, sort/deduplicate path sets, and retain text exactly.
Preserve the distinction between an omitted option and one whose value matters
to semantics. Reject unknown fields and non-integer numeric input. Canonical
JSON uses the existing encoder. The request digest is full BLAKE3 over the
canonical request bytes. Changing an attempt's parent OID or transaction
sequence does not change this digest; those belong to the transaction.

Each transaction contains schema `mote.git-transaction.v1`, sequence, parent
OID, the complete request, request digest, ordered effects, and an outcome.
The containing Git commit is its receipt. No payload embeds its own commit OID.
Effect IDs are `eff-<request_id>-<index>`, where index is zero-based six-digit
decimal. Each effect separately carries canonical position `(sequence, index)`.
An effect's token is its identity, not a chronological cursor. Existing
reducers that compare op IDs lexically require the adapter below before reuse.
The command planner receives only the parent state and recorded request inputs;
it performs no network or filesystem writes, clock reads, or ID generation.

Ordinary task and communication effects use the current Mote operation
vocabulary. Explicit ownership/session transitions use the remote policy;
they must not be disguised as long-lived TTL operations. The remote state may
wrap the current reducer state with explicit ownership records. It must never
reinterpret imported local claims under that policy. Encoding remote effects
never calls the process/random-based local operation-name generator. The
adapter supplies the envelope explicitly. Its chronological op ID is
`gaop-<20-digit sequence>-<6-digit index>-<effect_id>`. These IDs are local to
one bound authority generation. Communication/read-cursor code must compare
canonical positions or these ordered adapter IDs, never request IDs or times.
Until audited and adapted, those commands are refused. The fixture with request
`z` published before request `a` and reversed timestamps pins this requirement.

The outcome is `accepted` with a structured command result, or `rejected` with
a stable code and structured reason. Rejections have no state-changing effects.
For either outcome, validators reconstruct the command result and complete
effect sequence against the parent state and compare canonical bytes. An
arbitrary client-supplied result, missing compound effect, extra effect, or
false rejection makes the append invalid. Command semantics are versioned
capabilities; unsupported commands cannot be replayed as generic failures.

Every logical command is atomic at this boundary. `begin` includes reservation,
claim, doing status, and requested note/announcement. `done` includes closure,
completion note, held-reservation release and claim release. A semantic failure
in any requested effect rejects the complete command. Explicit caller clocks
or tokens are never silently refreshed to obtain a successful retry. Commands
such as `next` may reselect work on contention because their recorded intent is
selection, but a recovered receipt always returns its original selection.

### Initial executable synthetic profile

The initial state is an empty ordered map of ASCII keys to `{value, token}`.
The sole accepted command shape is:

```json
{"kind":"synthetic.cas","changes":[{"key":"task-1","expected":null,"value":"alice"}]}
```

The array contains 1–64 changes with distinct keys. Keys contain 1–128 ASCII
letters, digits, hyphens or underscores. `expected` is a token string or null;
`value` is a string or null. Unknown fields and missing required fields are
syntax errors before submission. Every request includes a nonempty session
identifier for attribution; this synthetic profile does not implement session
liveness or real Mote ownership and must never be exposed as task coordination.

Evaluate changes in array order against the parent map. Null expected means the
key is absent; a string must equal the existing token. The first mismatch
rejects the entire command with exact outcome:

```json
{"status":"rejected","result":{"code":"compare_failed","index":0,"key":"task-1","expected":null,"actual":"eff-req-prior-000000"}}
```

There are no effects for rejection. For acceptance, each change produces one
effect, in array order:

```json
{"effect_id":"eff-req-example-000000","sequence":1,"index":0,"kind":"synthetic_set","key":"task-1","value":"alice"}
```

Its token is its `effect_id`. A null value removes the key; otherwise the map
stores the value and token. Deleting an absent key with null expected is an
accepted recorded effect. The exact accepted outcome is:

```json
{"status":"accepted","result":{"changes":[{"key":"task-1","token":"eff-req-example-000000"}]}}
```

For a deletion the result token is null. Result entries retain input order.
Digest/identity mismatch is a submission error and never replaces an existing
receipt. Parent sequence plus one supplies each effect's sequence; an empty
root has sequence zero. No other command is accepted under the synthetic
profile. Ordinary task/session ownership is defined by the separate coordination
core contract and its vectors; its state is never mixed with synthetic entries.

The transaction's exact required fields are `schema`, `sequence`, `parent`,
`request`, `request_digest`, `effects`, and `outcome`. OIDs are lowercase hex
strings of the repository's object format (40 or 64 characters), and parent
matches the containing commit's sole parent. Sequence is a positive integer.
Canonical position advances even for a rejected transaction with no effects.

### Retry and response contract

Before submitting, persist the complete request and any generated IDs in the
local request journal. A caller can provide a request ID so it survives an
ephemeral machine. Each authority generation has at most one receipt per ID.
The same ID and digest returns that receipt's result, including a rejection.
The same ID with another digest is a mismatch and has no additional effects.

Submission fetches and validates head H, checks for an existing receipt,
plans, constructs a single successor of H, and attempts an ordinary push.
Acceptance or rejection is terminal only after confirming the valid containing
commit and receipt on the authority chain. Successful push followed by failed
confirmation is unconfirmed. A timeout, disconnect, cancellation, or uncertain
Git result returns the request ID, attempted commit when known, and an
unconfirmed state. It never asserts that nothing committed.

On contention, fetch again, check receipt identity, and replan the original
request. Bound retries and use jitter. A receipt absent from one snapshot does
not prove an in-flight attempt failed. All additional attempts retain the ID
and compete against a particular parent. A recovered accepted result cannot
be replaced by a later rejected result for that request.

Human output and JSON distinguish accepted, rejected, unconfirmed, unavailable,
and incompatible. Preserve existing local exit codes; the remote CLI must
document the exact new status-to-exit mapping before exposure. Transport
diagnostics must not print endpoint credentials or raw authentication output.

## Explicit ownership policy

The v1 decision is explicit release/recovery for remote claims and reservations.
Client wall-clock elapsed time never creates vacancy. Local stores retain their
current TTL behavior. A remote `--ttl` is refused until a separately reviewed
automatic-expiration capability exists.

Each session belongs to one actor and has an explicit active/ended/revoked
state. Presence timestamps and heartbeats do not grant authority. Claims and
reservations bind actor, session, and an acquisition token. Acquisition requires
an active session. Same-actor sessions have distinct authority. Acquisitions
and transfers create fresh tokens; holder-sensitive edits name the current
session and exact expected token. Reservations normalize paths and conflict
across issues and all clones/source branches in the project.

Release requires the exact holder/session/token. Handoff requires exact old
ownership and a named active recipient session. Reservation movement must be
explicit in the request; it is never inferred from an actor name. Session end
releases exactly that session's holdings in one transaction. Completion or any
close/delete/status update that invalidates held work must pass the same holder
checks. Generic patch, bulk operations, and convenience aliases cannot bypass
those checks.

Recovery is a deliberately requested administrative command by a cooperating
writer with repository access. It requires a nonempty reason and the exact
observed resource tokens, names the recipient session or release-only action,
and records request actor and previous holder in its effects. It is never
triggered merely by a heartbeat timeout. Repository access is the trust
boundary; actor strings are attribution, not authentication or an independent
administrator role. Two recoveries of one token have one winner. Old-holder
commands submitted after a recovery or transfer reject their stale tokens.

No mechanism can prevent an old offline editor from changing private source
files. The ownership token fences subsequent Mote decisions, not arbitrary
filesystem side effects. Roles, candidate grants/reviews, and candidate landing
are refused until qualified against this explicit policy.

| Scenario | Required result |
| --- | --- |
| Alice and Bob acquire one task from H | One first publication; loser replans and records rejection. |
| Same actor, different sessions | No implicit cross-session renewal or release. |
| Two issues reserve overlapping paths | One ownership acceptance. |
| Client clock jumps or machine sleeps | Presence may look stale; ownership remains held. |
| Holder crashes | No time-based takeover; explicit recovery names its tokens. |
| Two clients recover the same holding | One replacement/release; the other request rejects stale expectations. |
| Old holder resumes after recovery | Its holder-sensitive commands reject. |
| Session ends while another session of that actor works | Only the ending session's holdings are released. |

## Migration and writer fencing

Migration first quiesces local writers, resolves publication/landing journals,
backs up the complete store, and selects one authoritative local history.
Independently divergent replicas are not unioned automatically. Resolve live
local holdings and sessions explicitly before export. Record the selected
legacy semantic profile, exact admitted operation order and bytes, and every
accepted/rejected decision. A v1 reader supports only declared legacy profiles;
unsupported historical behavior is an import error.

The legacy prefix replays under its pinned original semantics. In particular,
an old history in which A expired and B acquired without A releasing must retain
that accepted decision. The transition to explicit remote ownership follows
lifecycle cleanup. New timeout-only takeovers reject. A historically unknown
operation retains its original rejection interpretation even if a newer binary
understands that operation. Reproducing only final task values is insufficient;
decision history must also match.

The new local store marker uses an incompatible format fence for older Mote
writers. All public local writer entry points check mode before obtaining
publication authority. Unsupported remote paths refuse; they never write a
local accepted op. This includes library, server, batch/import, candidate and
role paths, as well as CLI commands and overrides. Losing connectivity never
changes mode. Read-only cached views report head, observation time and freshness.

Bootstrap uses explicit authorization and competes to create one root. A loser
does not overwrite another bootstrap. Before any canonical receipt, explicit
permanent retirement may restore the stopped backup into a new private local
store under the [rollback contract](git-authority-rollback.md). Post-use
conversion is deferred from the first local-machine release: it requires a
versioned continuation format preserving current canonical history. Source branch updates
and deployment are separate effects, outside this transaction guarantee.

## Contract fixtures and acceptance

`tests/fixtures/git_authority/contracts.json` contains valid/invalid root and
request examples and the required scenario inventory. The production parser
and engine tests must consume these fixtures rather than duplicating a second
validator in a script. GA-05 independently reviews this contract and its content
digest. The later tests additionally prove actual concurrent publication,
delayed responses, process death, replay equivalence, writer refusal, source
checkout preservation, and the accepted deployment/latency envelope.
