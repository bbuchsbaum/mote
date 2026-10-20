# Git authority workflow for local machines

This is an opt-in development backend. It has local-clone and fault-injection
qualification, not a completed physical two-machine or hosted-policy pilot.
Cloud clients and conversion back to local mode after shared work are deferred.
Ordinary local stores retain their existing behavior.

Each project uses one dedicated `mote-state` branch on its selected Git remote.
The same branch name can be used independently in every repository. All source
branches, worktrees and machines for that project consult the same authority.
The metadata branch is never checked out or merged into source history. No
workstation must accept SSH connections; each client needs Git access to the
remote. A fork must explicitly select the intended authority endpoint.

## New authority and another client

Use the same retained executable on the participating local machines. First
inspect the executable without opening a store:

```sh
mote --json authority capabilities
```

In an existing Git checkout with an empty local Mote history:

```sh
mote init
mote authority init-git --remote origin --branch mote-state
mote authority status
eval "$(mote session start --as alice-laptop --label 'local development')"
mote new 'Shared task'
```

On another checkout with an empty local store, connect to that existing branch:

```sh
mote init
mote authority connect --remote origin --branch mote-state
eval "$(mote session start --as alice-desktop --label 'local development')"
mote ready
```

Bootstrap refuses an existing authority rather than overwriting it. `connect`
validates its root, store, generation, endpoint and immutable capabilities.
It creates this machine's local binding, not another authority. Keep `.mote/local`
and the local Git scratch/cache private and out of source commits.

A checkout that inherited old local operations is not an empty client. Preserve
that history; do not delete operations to make `connect` pass. One explicit
option is a separate client store inside the same checkout, with a shell-local
selector. From the project root, choose a new directory name:

```sh
mkdir .mote-client
(cd .mote-client && mote --store "$PWD/.mote" init --separate)
export MOTE_STORE="$PWD/.mote-client/.mote"
mote authority connect --remote origin --branch mote-state
```

Use that selector consistently in every participating terminal. The inherited
local store is retained historical custody, not an active coordination replica.
Do not commit the client directory or let another terminal use the old store
as this project's active tracker. `--store` overrides `MOTE_STORE`; actor and
session identity are also local choices, not evidence of process liveness.

## Ownership, reads and recovery

```sh
mote preflight --issue bd-ID --paths src/example.rs
mote begin bd-ID --paths src/example.rs --note 'starting'
mote note bd-ID --kind progress 'implementation and check evidence'
mote done bd-ID --note 'verified result'
mote session end
```

Replace `bd-ID` with the ID returned by `new`. Begin/next, done and handoff are
whole canonical transactions. Ownership uses explicit release and exact tokens.
On an announcement-capable authority, adding `--announce TOPIC` to `begin`
commits the claim, reservation, status, note and public post together. Earlier
profiles refuse the option; connecting never upgrades their contract.
A sleeping or crashed machine does not lose ownership through a TTL. A session
end cleans only its own claims and reservations, including when another session
uses the same actor. Recovery is an explicit audited action by an active session:

```sh
mote session list --all
mote authority recover-session sess-SOURCE --expect-session TOKEN --reason 'holder lost; reviewed current ownership'
```

Use the source session's observed `token`, not its presence timestamp. The CLI
collects observed claim and reservation tokens from a fresh parent. A competing
recovery or write can make those expectations stale and cause rejection.
`--to-session sess-RECIPIENT` deliberately transfers holdings to an active
recipient; omitting it releases them and revokes the abandoned source session.

Ordinary reads fetch and validate fresh shared history. `authority cached` is
the explicit offline view and always says `fresh:false`; it cannot authorize
work. Offline mutations fail without an isolated local write. An uncertain
response includes a retained request ID and recovery commands:

```sh
mote authority receipt REQUEST_ID
mote authority retry REQUEST_ID
```

Retain the submitting machine's request journal until resolved. Retry reuses
the exact request bytes, timestamp and IDs; retyping the original command makes
a new intent. A fresh client can inspect an existing receipt by ID; if the
submitting machine and its journal were lost before confirmation, absence of a
receipt does not prove the request failed. Reusing a communication idempotency
key in a new request rejects rather than replacing receipt recovery.

`events --follow --interval 5`, `watch --interval 5`, and `inbox --follow` are
read-only polls. Event cursors bind root and canonical position, survive client
replacement, and never sort remote/legacy history by timestamp. Total cache loss
requires `authority connect` to restore routing/binding custody first. A fetch
failure stops a fresh stream. Poll intervals are provisional, not a measured
production capacity promise; no bulk batching is supported. An earlier small
local nine-transaction transport probe showed about 1.2 seconds for a fresh
read after optimization, but does not qualify current large histories or a
hosted remote. Avoid many redundant followers until representative costs are
measured.

## Existing local history and operational limits

Migration requires one explicitly selected, stopped local history with no
outstanding publication journals, claims, open reservations or active sessions.
Expiry alone does not satisfy cleanup. Resolve source replicas deliberately;
Mote does not union divergent local histories. Use the closed-build development
recipe when replaying migrated roots:

```sh
python3 scripts/build_legacy_profile.py /absolute/new/path/mote-retained
/absolute/new/path/mote-retained --json authority legacy-profile
/absolute/new/path/mote-retained authority migrate-git --remote origin --branch mote-state --backup /absolute/new/private/backup
```

The backup must be a new private destination outside the source project and
scratch/cache paths. It retains the complete stopped store, not just operations.
Setup journals preserve the selected root across interruption:

```sh
mote authority resume --remote origin --branch mote-state
```

Use the same retained binary for migrated replay, resume and rollback. Its
profile includes executable bytes and closed build inputs. A rebuild—even from
the same Git revision—or another architecture is not automatically compatible.
Cross-build and cross-architecture migration are not yet qualified.

Before any canonical receipt (including enrollment or rejection), an unused
migrated authority can be permanently retired and its verified backup restored
to a new private local store. It does not reactivate the original store:

```sh
mote authority rollback-git --output /absolute/new-parent/.mote --reason 'cancel unused migration'
```

The output's parent must already exist outside the project and custody/cache
paths. Retirement must win the first transaction; if enrollment or another
request wins, restoration refuses. After shared work, returning to local mode
requires a future continuation format and is deferred for this release scope.

The [command matrix](git-authority-commands.md) lists supported and refused
routes. Candidate/role workflows, distributed source landing, batch/import,
web console and TUI are unsupported for a Git store. Do not combine local
candidate acceptance with a claim of distributed source publication.
See [compatibility](git-authority-compatibility.md) for executable diagnostics,
and [communications](git-authority-communications.md) for retry/ordering rules.

Metadata has the visibility and write permissions of its remote; actor names
are not authenticated human identities. Normal writers must be allowed direct
appends while branch policy rejects rewrites and deletion. Invalid appended
history stops clients; they never force-push a repair. Hosting policy must be
qualified before adoption. This repository's CI pushes run only on `main`, so
direct `mote-state` appends do not trigger its source-build workflow; other
repositories must inspect their own workflow filters. No hosted policy is
changed by these instructions.
