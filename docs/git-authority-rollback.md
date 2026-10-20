# Unused Git authority rollback (development contract)

Rollback restores the stopped backup into one explicitly selected **new private
local store path ending in `.mote`**. Its parent directory must already exist;
incompatible output names refuse before any retirement attempt. It leaves the original attached store, cache, setup evidence
and publication lock inode intact. Use the resulting path through `MOTE_STORE`.
This development path requires Unix filesystem identities and refuses other
platforms before retirement. This operation does not copy a source-code checkout or restore the original
pathname. Other machines remain attached to a permanently retired authority.

The immutable `authority-retirement-v1` capability is a distinct profile; the
previous five-capability migrated profile retains its meaning and refuses this
action. Both the six-capability recoverable profile and the new seven-capability
communication profile support the same retirement rule. Retirement requires matching verified legacy export identity and prior
canonical sequence zero. Every existing receipt, including rejected requests
and session enrollment, disqualifies retirement. A single CAS append records a
permanent effect. All subsequent ordinary actions, including session enrollment
and recovery, reject with `authority_retired`; receipt recovery still runs first.
A concurrent enrollment can win instead, in which case restoration is forbidden.
This is the existing Git-writer trust boundary, not an additional authorization
system. Precedence after request validation is retired, already-used authority,
then export-digest mismatch.

Before retirement, retain one canonical request, backup custody, endpoint,
branch, root, store/generation, destination and nonce in a private journal in the
original store. Retrying cannot change these or create additional writable
copies. Rejection, timeout, missing branch, missing receipt or incompatible
profile leaves the original fenced and creates no usable destination. Only an
accepted canonical retirement receipt, validated against that exact request and
identity, authorizes restoration. The authoritative root is retained, never
deleted or recreated. Rollback of a root that has never been published is not
approximated by treating a missing branch as proof of failure.

The output must be canonically disjoint from the source project, complete
backup, authority cache and disposable scratch in both ancestor directions.
Create it exclusively, private to its owner. Persist its filesystem identity
before copying; ambiguous ownership or a competing destination creator refuses.
Copy original files/modes and empty directories, excluding FORMAT until final
publication. Restore directory modes after children; retain private permissions
on the output root even if the original root was more permissive. The complete
copy and mode inventory must be verified before publication authorization.

Restoration progress and retirement proof remain in the original store, outside
the copied contents. Hold the original publication lock throughout. Hold the
destination publication lock through final verification and publication; never
replace or unlink either lock inode. Write/sync complete FORMAT bytes in a
private file on the destination filesystem. Persist publication authorization,
reverify the complete unpublished copy and staged FORMAT bytes/mode under the
destination lock, then durably record `publishing` before attempting the link.
The original process may then atomically hard-link those bytes into the previously absent FORMAT path
and sync the destination directory. Direct partial writes to FORMAT are unsafe.

After FORMAT might have become visible, never recopy or overwrite the target.
A resumed `publishing` phase requires the exact complete FORMAT to be visible;
absence refuses without recreating it, including death between the durable
attempt marker and the link itself. This intentional ambiguity requires
reconciliation. An `authorized` phase has not attempted publication yet, so it
may reverify the entire unpublished copy and staged FORMAT before beginning.
A retry verifies the pinned destination identity and complete FORMAT bytes
before recording publication; mere file existence is insufficient. Changed or
ambiguous state requires reconciliation. A completed retry reports the prior
completion and preserves later local operations. The original store remains
attached and retired; there is no window for a local writer to fall back there.

Required witnesses include both forced retirement/enrollment race outcomes,
accepted retirement followed by client death, interrupted copying,
authorization and atomic-publication boundaries, competing destination creation,
and a real local write after publication surviving retry. A post-use return to
local mode requires a separate migration from current canonical history; this
pre-use command must refuse it.
