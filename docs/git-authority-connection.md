# Git authority connection and activation

Development contract for GA-20 and GA-23. First support is local machines.
Actual hosted qualification and project activation remain separate gates.

One project has one authority branch, independent of source branches and
worktrees. Existing local stores require the GA-22 migration path; an empty
store can bootstrap or connect without importing legacy history.

## Durable identity

The project descriptor `.mote/remote.json` is strict canonical JSON containing
its schema version, named remote, branch, store ID, generation, root OID, and
exact capability list. It contains no URL, credential, absolute machine path,
or copied authority journal. It can be shared with the project. A remote name
is a routing hint; the immutable root/store/generation identify the authority.

The machine's endpoint digest, confirmed high-water mark, retained requests,
and derived view stay below `.mote/local/git-authority/`. The existing client
binding pins the resolved single push endpoint. The machine route also pins the
exact capabilities, compared with the descriptor at open and with the validated
remote manifest before observations, new request intents, or submissions. Connecting an already bound
store must match the descriptor and existing endpoint binding. A different
remote alias is permissible only when it resolves to that same endpoint;
changing endpoint is an administrative migration, not a fresh connection.
Git commands must run from the selected store's project, including its linked
worktrees. An explicit store override from an unrelated Git repository refuses
before resolving its origin or publishing anything.
Inherited `GIT_DIR`, `GIT_COMMON_DIR`, `GIT_WORK_TREE`, and `GIT_NAMESPACE`
selectors refuse before repository probes, including for library transports.

Git object/index scratch is private and isolated outside the source checkout.
A command owns its scratch directory and removes it on ordinary completion.
Scratch is regenerable; durable requests, binding, and high-water evidence are
not stored there. Canonical paths (including symlink aliases) must be disjoint
in both ancestor directions; validation refuses overlap before cache writes.
Project caches have a distinct directory below store local/. Neither successful reads nor rendering append transactions.

## Setup and interruption

`authority init-git --remote origin --branch mote-state` and
`authority connect --remote origin --branch mote-state` are explicit. Setup
holds the existing local publication lock throughout validation and the local
mode transition. It refuses existing local operations (including unadmitted
files), unresolved publication/landing journals, and incompatible descriptors.
Nonempty migration is implemented separately and never approximated by task
recreation or an operation-set union.

Bootstrap durably retains the exact manifest blob, tree object, and raw commit
object, their Git object types and OIDs, and the selected identity before a
non-forced branch creation. This artifact lives in the setup record outside
disposable scratch. Re-importing those exact bytes must reproduce every OID,
including the commit's original author/committer timestamps; an OID plus a newly
generated commit is insufficient. An ambiguous push retains this artifact.
Retry observes the remote first and recognizes only that root and identity (or
a fully validated descendant of that root). It never mints a new root. A
competing root conflicts; a previously bound missing branch is damage.

Connect validates the complete remote chain and compares every descriptor
identity field before activation. It may adopt the remote store ID only for an
empty, never-bound local store. Bootstrap and connect preserve the source refs,
index, dirty files, and FETCH_HEAD.

Activation first durably publishes `.mote/local/git-setup.json`. Its existence
is itself a local-writer fence, checked before and after acquiring the common
publication lock. Setup/recovery owns that lock but does not call the normal
Writer's automatic publication recovery. It checks pending publication and
landing state explicitly and refuses unresolved work.

The strict setup record binds operation (`bootstrap` or `connect`), original
local FORMAT bytes/digest and store ID, an empty raw-operation-set fingerprint,
resolved endpoint digest, remote/branch, proposed descriptor and capabilities,
root artifact for bootstrap, and phase. These fields are immutable across
resume. Endpoint retargeting, changed arguments/profile, changed local history,
or an unrelated format/descriptor/binding fails closed and retains the record.

| Phase | Durable facts and resume behavior |
| --- | --- |
| prepared | Identity and bootstrap objects are durable; local writers are fenced. Reconstruct scratch from retained objects. Bootstrap observes before push and reuses the exact candidate; connect revalidates the selected remote root. Only a never-confirmed cache and record permit retrying a missing branch with the retained candidate; any retained binding, high-water mark or confirmed head prohibits recreation. A conflicting root refuses. |
| remote_confirmed | A validated observation has the selected root/store/generation/profile, and its binding and confirmed head are retained. Revalidate before finishing local activation; never create a replacement branch. |
| activated | The exact project descriptor, remote FORMAT, machine binding and high-water mark are durable. Remove the setup record only after verifying all four. |

Between remote confirmation and activated, publish the descriptor marker, then
the remote FORMAT, then complete/verify machine cache binding. Confirmation in the machine cache can precede the phase update; even a
`prepared` record must honor that evidence and refuse missing-branch recreation.
A phase update may lag a durable file write: recovery accepts only the original or proposed
FORMAT, the absent or exact proposed marker, and absent or matching cache
binding. Any other combination refuses. The intent fence remains throughout;
there is no local fallback window after a crash. Repeating setup verifies the
unchanged source cut and completes this same transition, never replacing its
identity. A descriptor in a fresh clone requires explicit machine connection.

Required interruption witnesses cover death after intent but before marker
(a local publish must refuse), around push with scratch loss (same root OID),
immediately after cache confirmation but before its phase update (deleting the
remote branch must never trigger recreation), and after marker but before
FORMAT/cache completion. Also cover competing
bootstrap roots and endpoint retargeting after an ambiguous push.

No automatic rollback is allowed after a confirmed remote transaction. A failed
pre-activation attempt leaves the local history intact. Deactivation or local
forking after activation needs an explicit administrative operation with custody
and outstanding-request checks; deleting a marker is not rollback.

The current binary fences all local publication/replay entry points. Binaries
that predate that fence are outside the supported client set; format metadata
alone does not prove those binaries refuse writes. Qualification must establish
the minimum supported binary and keep old clients away from activated stores.

## CLI submission and reads

Remote dispatch occurs before local identity/replay guards. Supported actions
use the remote client; every other stateful entry point refuses explicitly.
There is no path that retries through the local Writer after a remote failure.
Local identity configuration remains local and does not require publication.

Every logical mutation has one stable request ID and one canonical request.
Generated IDs and ambiguous outcomes are reported with a concrete recovery
command. Retrying a retained request uses its original bytes, including
preconditions and timestamp; reconstructing a new request under the same ID is
forbidden. Receipt lookup precedes session checks, including after end/recovery.
An absent receipt is not proof that an in-flight attempt failed.

Reads refresh by default and report head, sequence, observed time, and explicit
ownership policy. An explicit cached read never contacts the remote and always
reports `fresh:false`. Missing/unavailable authority is an error for fresh reads
and mutations; it never silently turns a cached view into a fresh decision.
Read projections use durable ownership without local TTL vacancy assumptions.
Cached inspection checks the saved project/machine identity without resolving
current Git remote configuration; removing an alias does not erase the saved
observation. Receipt lookup reports the same validated observation used for its
lookup, including when no receipt was found. Task history with rejected entries
includes ownership requests referenced indirectly by claim/reservation intent.

Remote lifecycle uses MOTE_ACTOR and MOTE_SESSION, with explicit enrollment.
Session-start shell output activates those values only after a confirmed
accepted receipt. A rejected or unconfirmed start must not emit shell activation.
Without `--json`, an accepted start writes only quoted shell exports to stdout
so `eval "$(mote session start --as actor)"` remains valid. Failure diagnostics
go to stderr; structured JSON output is available with `--json`.
Unsupported TTL/force/resume/announcement options refuse rather than changing
meaning silently. Legacy local command behavior stays unchanged.
