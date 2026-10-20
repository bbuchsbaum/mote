# Legacy migration development contract

GA-22 development seam. No existing project is migrated by this document.
The initial profile may refuse an archive after a harmless build-input change;
that conservative compatibility limit is acceptable for disposable development
roots. Adoption requires a separately qualified supported-client profile.

## Selected source cut

Capture one local source under its existing publication lock, without calling
Writer::acquire's automatic recovery. Recheck local mode under the lock. Reject
pending publication, active/malformed/nonterminal landing journals, inconsistent
binding, admission gaps, missing/changed admitted bytes, and unadmitted files.
Use the exact admitted order (or the explicitly identified unfenced filename
order), including rejected/malformed operations. Never sort an admitted stream
again or union replicas. Additional supplied source cuts must match or carry an
explicit source-selection/reconciliation record; an exporter cannot discover
undisclosed replicas.

Record the selected source store, exact FORMAT bytes, source order mode,
authority version/genesis and immutable admission evidence. The export contains
only a closed allowlist of archive data, not local credentials, actor config,
locks, temporary files, or writable journal directories. A full backup remains
local and is separate from the shareable archive.

## Pinned replay

The development legacy profile is an embedded fingerprint of the actual build
inputs: the entire semantic crate (including dirty/untracked build inputs),
build script and generated semantic input, dependency lock/manifests, path
dependencies, enabled features and relevant compilation configuration. Git HEAD
alone is insufficient. New or unsupported build-input classes must either be
included or make profile generation refuse. The fingerprint implementation
must have independent fixtures proving changes to a helper and to dependency
or build input change the fingerprint.

An importer requires an exact supported embedded profile before replay, seed
acceptance or high-water advancement. It never tries the current reducer on an
unknown profile. A newer implementation either deliberately retains compatible
profile support or refuses the archive, including an operation the old profile
rejected as unknown. Profile equality does not replace integrity validation.

Capture evidence after each original input, in original order. Retain every
appended entity/orphan history event with original identity, actor, timestamp,
acceptance and reason; do not reconstruct ordering by flattening final maps.
Replay reproduces that evidence, final legacy state and the task seed exactly.

Existing admissions prove byte/order custody, not which reducer an old
publisher ran. This archive establishes the selected source's explicitly
identified pinned replay at export time. Earlier publisher decisions require
separate historical evidence and a supported historical profile; they are not
inferred from an admission journal.

## Canonical state evidence and digest graph

The terminal state is serialized from the actual complete `State` type, using
serialization-only derives and ordered arrays of `[key, value]` for maps whose
keys are tuples. No deserialization, presentation projection, debug formatting,
or manually maintained semantic mirror participates in this evidence. The
pinned profile identifies this serialization. The evidence envelope is:

```json
{"schema":"mote.legacy-state.v1","profile":"PROFILE_ID","state":"SERIALIZED_COMPLETE_STATE"}
```

Here `state` is the serialized object itself, not a JSON string. Its digest is
BLAKE3 over the UTF-8 bytes `mote.legacy-state.v1` followed by one zero byte and
then the canonical encoding of the complete envelope. Field clocks, all domain
records, histories, orphan/rejected evidence, tuple keys and tombstones are
therefore covered.

Digest dependencies are acyclic and point only to already-computed inputs:

1. `source_cut`: source identity, pinned profile, order mode, raw FORMAT digest,
   ordered original op names/digests and ordered admission names/digests.
2. `final_state`: the complete state evidence resulting from that source cut.
3. `task_seed`: canonical verified seed derived from the same replay.
4. `transition`: source-cut digest, cleanup input ordinal, final-state digest,
   task-seed digest and ownership policy.
5. `export`: the complete canonical export index, including ordered decisions
   and the transition digest.
6. Root manifest: the export digest.

No digest contains itself or a downstream digest. Exact raw FORMAT bytes are
stored in `legacy/format.bin`; changing only their whitespace changes the
source-cut digest. Reordering admission evidence changes that digest. Changing
only the designated cleanup boundary changes the transition digest. These are
required independent fixtures in addition to the witnesses below.

## Cleanup and ownership transition

Require a designated final legacy cleanup suffix, preserved as original ops.
After replay there must be no remaining claim records, all reservation paths
must be explicitly closed, and every session explicitly ended. These checks
never consult the current clock. Expiry alone and session end with residual
claim records fail cleanup. Resolve them using normal local commands before
export; the exporter does not publish cleanup on behalf of a holder.

Record a cleanup transition binding the selected source-cut digest, suffix
boundary, terminal-state digest, seed digest and `explicit-release-v1` policy.
The suffix boundary is an input ordinal in the selected order; an empty final
suffix is valid when the source is already explicitly clean. Remote ownership
maps start empty. Consumed task/session/reservation IDs are derived from the
complete terminal state, including tombstones, and remain consumed. Clients enroll fresh sessions; old TTL events never become
remote ownership events or request receipts.

## Closed archive and migrated root

The root manifest selects a distinct migrated-root capability. Existing roots
and profiles continue refusing archive files. The complete root is a closed
file set containing its manifest and this archive:

- `legacy/format.bin`: exact original FORMAT file bytes, without normalization.
- `legacy/export.json`: schema/profile, source identity/order, raw FORMAT digest,
  ordered entry index (ordinal, original filename, raw blob path and digest),
  admission evidence index, decision/final-state/cleanup/task-seed digests.
- `legacy/ops/<20-digit-ordinal>.bin`: exact original bytes, including malformed
  or unsupported inputs legitimately rejected by the pinned profile.
- `legacy/admissions/<original-record-name>.json`: immutable admission evidence
  as archive data, never a live writable journal. Absent for unfenced sources.
- `legacy/decisions.json`: ordered per-input events and their history placement.
- `legacy/transition.json`: explicit cleanup/source-cut/policy boundary.
- `legacy/task-seed.json`: all task records, including tombstones, scalar
  clocks, creation/deletion provenance, tags, dependencies, relations and notes.

Validate canonical metadata, unique ordinals/names, exact file-set equality,
all digests, actual pinned replay decisions, cleanup and seed before accepting
a migrated root. Derive the reducer scratch task state from the verified full
legacy replay/seed; never filter and republish accepted task operations, and
never place legacy TTL operations in the remote task_entries list. New task
ops use the existing reducer against that scratch state and preserve original
field clocks. Full legacy history remains independently available as archive
history. Unsupported legacy live domains remain explicitly unavailable, while
their original operations/evidence remain preserved in the archive.

Remote sequence starts at 1 after the root. History positions distinguish
legacy ordinal from remote sequence/effect index. Original timestamps order
neither phase. New remote transactions never mutate the legacy archive.

## Required discriminating witnesses

1. Backdated admitted operations change results if lexically resorted; import
   preserves exact order, bytes and every decision.
2. A expires, B acquires without A releasing, B releases: all legacy decisions
   survive, and timeout-only takeover after transition fails.
3. Unknown-op rejection survives under its matching profile. Changed profiles,
   decisions, bytes, missing files, extra files and admission damage refuse.
4. Publication/landing journals and unadmitted or divergent source cuts refuse
   without recovery or publication side effects.
5. Session end with a residual expired claim fails cleanup; explicit local
   release permits the boundary. Time passing alone never changes eligibility.
6. Deleted task IDs cannot be recreated; original clocks support later CAS;
   rejected/orphan evidence and archive bytes remain unchanged after new writes.
7. A profile mismatch advances neither usable state nor cache/high-water data.
