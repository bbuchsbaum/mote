# RFC: immutable operation packs for Git-scale stores

Status: accepted design candidate; implementation requires a separate bead
Scope: storage representation only; operation meaning and replay order do not change
Evidence date: 2026-08-31

## Decision

Add immutable, content-addressed operation packs as a second authoritative
representation beside loose operation files. Do not add a mutable append log.
Do not make snapshots authoritative.

A pack is a content-addressed byte blob plus a content-addressed manifest. No
mutable catalog says which pack is current. Readers take the set union of valid
packed and loose operations, require byte-identical duplicates, sort the unique
operations by existing operation id, and run the existing reducer. Pack creation
never blocks loose publication. Loose files are retired only in a separate,
verified step and can be reconstructed byte-for-byte from a pack.

This is a Git hygiene change, not a replay-performance response. The motivating
`storymodel4s` store had 2,157 loose JSON files and about 8.6 MB of operation
data while `mote board` still replayed in roughly 0.05 seconds.

## Goals and non-goals

The design must:

- reduce Git index and worktree churn from thousands of immutable files;
- preserve the current append-only audit history and deterministic reducer;
- allow loose publication, replay, fsck, and pack creation to run concurrently;
- merge normally when two Git worktrees pack identical or overlapping sets;
- detect a duplicate operation id with different bytes and fail closed;
- survive a crash at every publication and retirement boundary;
- allow a schema-2 store to return to a loose-only schema-1 representation.

The first implementation will not:

- compress blobs (compression can be a later format version);
- garbage-collect redundant packs automatically;
- use a snapshot to skip validation or change reducer results;
- sign history or protect against a collaborator who can rewrite the repository
  and recompute every hash;
- coordinate Git commits, pushes, or branch policy.

## Threat and concurrency model

The design covers these ordinary failures:

1. A process crashes while writing a pack blob or manifest.
2. Loose operations arrive before, during, or after a packer's directory scan.
3. Two packers select the same or overlapping loose operations.
4. Two Git worktrees pack overlapping history and later merge.
5. A tracked file is truncated, altered, renamed, or paired with the wrong
   manifest.
6. A loose operation and a packed entry claim the same id.
7. Operation timestamps are skewed, so a newly published id sorts before a
   packer's nominal high-water mark.
8. An older binary sees a store whose loose copies have been retired.

The trusted boundary is the local filesystem and repository writer. Content
hashes detect accidents and inconsistent merges; they are not authentication.
Symlink traversal, hostile repositories, signatures, and remote object storage
need their own security design before implementation expands beyond local
POSIX filesystems.

## Canonical on-disk format

Schema 2 adds these paths:

```text
.mote/
  FORMAT.json
  ops/                                      # unchanged loose publication
  packs/v1/blobs/blake3-<64-lower-hex>.pack
  packs/v1/manifests/blake3-<64-lower-hex>.json
  tmp/                                      # durable publication staging
```

Both hashes are full 32-byte BLAKE3 digests rendered as 64 lowercase hex
characters. A truncated operation-name hash remains part of the existing loose
operation envelope, but it is not sufficient as a pack content address.

### Blob

The blob is the concatenation of selected operation bytes in ascending
operation-id order. Each operation uses the exact canonical bytes stored in its
current loose file. One ASCII LF follows every entry, including the last. The
LF is a record separator and is not included in the entry's `length` or
`content_blake3`; it is included in the blob's BLAKE3 digest.

Packers must not parse and reserialize a valid loose file when copying it into
the blob. Preserving exact bytes makes duplicate comparison and rollback
mechanical.

### Manifest

The manifest is canonical JSON using Mote's existing encoder. It has no trailing
newline. Its filename is the BLAKE3 digest of those exact manifest bytes. The
manifest does not contain its own digest.

```json
{
  "entries": [
    {
      "content_blake3": "<64-lower-hex>",
      "length": 417,
      "offset": 0,
      "op": "20260831T120000.000000Z-p12345-c0000-rabcd-h123456"
    }
  ],
  "first_op": "20260831T120000.000000Z-p12345-c0000-rabcd-h123456",
  "kind": "mote_op_pack_manifest",
  "last_op": "20260831T120000.000000Z-p12345-c0000-rabcd-h123456",
  "pack_blake3": "<64-lower-hex>",
  "pack_bytes": 418,
  "v": 1
}
```

Manifest rules are canonical and fail closed:

- `entries` is non-empty and strictly sorted by `op`.
- `first_op` and `last_op` equal the first and last entry ids.
- The first offset is zero. Every later offset equals the previous offset plus
  its length plus one separator byte.
- `pack_bytes` equals the final offset plus final length plus one.
- Every indexed separator is LF and no unindexed bytes exist.
- `content_blake3` hashes exactly `length` bytes starting at `offset`.
- Parsed entry `op` equals the manifest id; its timestamp and existing
  operation-name hash satisfy the same envelope checks as a loose file.
- The blob and manifest filenames match their full content digests.
- Unknown manifest versions or fields that change canonical interpretation are
  rejected, not ignored. Additive metadata belongs in a new manifest version.

There is deliberately no `current`, `packs.json`, generation counter, or other
mutable index. The manifest set is the index. Each manifest gives direct
offset/length lookup within one blob, while the loader builds an in-memory
operation-id map across manifests and loose files.

## Mixed packed and loose replay law

Let `L` be every valid loose `(op_id, bytes)` pair and `P` every valid pair
addressed by every valid manifest. Define `U` by grouping `L union P` on
`op_id`:

- one byte string for an id contributes one operation, regardless of how many
  loose files or packs contain it;
- more than one distinct byte string for an id is corruption and aborts replay;
- invalid source bytes abort replay; another valid copy does not mask them.

Then:

```text
mixed_replay(L, P) = reducer(sort_by_op_id(U))
```

For any valid loose-only history `L` and packs `P` containing only exact copies
from `L`:

```text
mixed_replay(L, P) = loose_replay(L)
mixed_replay(L - packed(P), P) = loose_replay(L)
```

This is the compatibility law that implementation tests must exercise across
accepted operations, rejected operations, duplicate overlapping packs, and
randomized pack boundaries. Source order never breaks ties: equal ids must have
equal bytes.

## Race-safe pack creation

`mote pack create` is a read-copy-publish operation:

1. Replay and fsck the store. Capture an explicit sorted list of loose
   filenames and their exact bytes. Selection is by membership, never by
   deleting every id before a timestamp or high-water mark.
2. Build the canonical blob and manifest in `.mote/tmp/` with `O_EXCL`; fsync
   each completed file.
3. Publish the blob first with the existing link-if-absent pattern and fsync its
   destination directory. If the content-addressed path exists, compare exact
   bytes and accept only an identical file.
4. Publish the manifest second and fsync its directory. A crash before this
   point can leave only an unreferenced blob, which replay ignores and fsck
   reports as an orphan.
5. Replay the mixed source set and compare its ordered logical operation digest
   and reducer result with the pre-pack replay.
6. Report the manifest id and the exact loose filenames eligible for a later
   retirement. Creation itself deletes nothing.

Loose publishers continue using `.mote/tmp -> .mote/ops` throughout. A new op
that appears after step 1 is absent from this manifest and stays loose. Clock
skew cannot make it an accidental retirement target because the packer retained
an explicit filename set.

Two packers selecting the same bytes derive the same paths; identical existing
content makes publication idempotent. Overlapping selections derive different
content-addressed files and are both valid. Replay deduplicates their shared
entries by exact bytes.

## Retirement, migration, and rollback

Pack read support lands before any loose retirement.

1. A schema-1 store may create shadow packs while retaining every loose file.
   Old binaries continue to see complete history.
2. Run mixed-source replay, fsck, randomized equivalence tests, and a clean
   consumer check.
3. Upgrade `FORMAT.json` to schema 2 in a dedicated commit. As an implementation
   prerequisite, schema-1 binaries must reject unknown schema versions before
   reading or publishing; silently ignoring packs is unsafe after retirement.
4. `mote pack retire <manifest>` revalidates the manifest, verifies exact bytes
   for every selected loose file, and removes only that enumerated set. A file
   that is missing, changed, or not named in the manifest is not touched.
5. For Git-backed stores, commit pack additions first and loose deletions
   second. Each review boundary is independently reversible and no partial
   checkout depends on an uncommitted mutable catalog.

Retirement is recoverable because `mote pack unpack <manifest>` republishes each
entry through the normal exclusive loose-file path. An existing identical file
is success; an existing different file is corruption. To roll back to schema 1,
unpack all manifests, verify loose-only replay and fsck, then downgrade
`FORMAT.json`. Packs may remain as ignored shadow data or be removed in a later
Git commit.

A crash during retirement leaves a mixed set, which the replay law explicitly
supports. Re-running retirement is idempotent after verification.

## Ordinary Git merge behavior

Content addresses remove the shared mutable-file conflict:

- identical pack selections add the same paths with the same bytes;
- different or overlapping selections add different paths;
- deletion of the same immutable loose file on both branches agrees;
- deletion of different loose subsets merges as their union;
- new loose files on either branch remain loose after merge;
- overlapping packed operations remain exact duplicates at replay.

Git is not the integrity oracle. After any merge, `mote fsck` still validates
the union and rejects a same-id/different-bytes history.

The included experiment creates two worktrees from ten tracked loose
operations. Worktree A packs and retires operations 1-6. Worktree B independently
packs and retires 4-9 and publishes operation 11 loose. Merging B into A with
ordinary Apple Git defaults produces no unmerged paths. The result has two
loose files, two pack blobs, eleven logical operations, three exact duplicates
from the overlap, and zero conflicting duplicates.

The first draft of the fixture used identical padding in every synthetic op,
which caused Git's rename heuristic to report a rename/delete conflict unrelated
to pack semantics. The checked-in harness uses a deterministic per-op payload,
matching the byte diversity of real operation records, and leaves rename
detection at its default setting.

## `fsck` contract

Schema-2 `mote fsck` checks, in order:

1. existing loose filename, JSON, envelope, timestamp, and hash invariants;
2. manifest filename digest and canonical JSON encoding;
3. manifest schema, ordering, offsets, lengths, separators, and total size;
4. referenced blob existence and full BLAKE3 digest;
5. every entry's full content digest and existing operation-envelope rules;
6. the global operation-id map across loose files and every manifest;
7. orphan blobs, manifests with missing blobs, and temporary debris.

A missing blob, malformed manifest, digest mismatch, invalid entry, or
same-id/different-bytes pair makes fsck unclean and blocks replay. An unreferenced
blob is a warning because a crash may have occurred between blob and manifest
publication; replay ignores it. `--clean-tmp` retains its current scope and does
not delete orphan blobs or packs heuristically. Explicit `pack prune` behavior,
if later desired, needs its own age and reachability policy.

`fsck --json` should add packed and unique counts, duplicate-source counts,
orphan paths, manifest/blob failures, and the ordered logical-operation digest.
Human output must distinguish physical entries from unique replayed operations.

## Snapshots and the mutable-log alternative

An authoritative snapshot is rejected for the first implementation. State
contains reducer-version assumptions, rejected-operation provenance, and
time-derived leases. Proving that a snapshot is complete and compatible is more
complex than validating an immutable pack, while packs already solve the Git
file-count problem without changing the replay authority.

A future snapshot may be a disposable cache named by the full ordered
logical-operation digest plus reducer schema. Readers must be able to delete it
and recover the same result from operations alone. It must never authorize loose
retirement.

A single mutable append log is also rejected. Concurrent worktrees would append
to the same path, ordinary Git merges would conflict inside that file, a torn
write would affect multiple operations, and a mutable side index would add a
second conflict point. It is strictly worse than immutable content-addressed
files for the demonstrated multi-writer workflow.

## Reproducible benchmark evidence

Run:

```sh
scripts/op_pack_rfc_experiment.sh
```

The harness uses 2,157 tracked synthetic operations, adds a 300-operation dirty
batch, and compares that batch as loose files against one pack blob plus one
manifest. Each timing is the median of nine warm local invocations. It then
runs the two-worktree overlap experiment above. Fixture filenames use SHA-256
from Python's standard library because this is a Git-behavior harness, not the
format implementation; the RFC format remains BLAKE3.

Result on macOS 14.3 arm64, Apple Git 2.39.3, Python 3.14.7, on 2026-08-31:

| Measure | Loose | Packed |
| --- | ---: | ---: |
| tracked paths at the 2,157-op baseline | 2,157 | 2 |
| dirty paths for 300 new ops | 300 | 2 |
| `git status --porcelain`, median | 24.320 ms | 19.992 ms |
| `git add -A`, median | 47.773 ms | 24.885 ms |
| Git index bytes after staging | 196,615 | 713 |
| logical bytes after baseline plus new batch | 10,102,077 | 10,431,275 |

The pack does not materially compress data and the manifest adds about 329 KB;
that is expected. The gain is path and index cardinality: 2,457 logical
operations are represented by two baseline paths plus two dirty paths instead
of 2,157 baseline paths plus 300 dirty paths. Compression is orthogonal.

Final merge receipt:

```text
merge.conflicts=0
merge.loose_files=2
merge.pack_files=2
merge.logical_ops=11
merge.exact_duplicate_entries=3
merge.conflicting_duplicate_entries=0
```

Timings are machine-local evidence, not a product performance guarantee. The
path counts, merge result, and logical-union checks are deterministic acceptance
evidence.

## Implementation acceptance boundary

Implementation must be a separate accepted bead. At minimum it needs:

- schema-version fail-closed behavior before retirement is possible;
- pack/manifest parser and durable publication failpoint tests;
- property tests for the mixed replay laws and randomized overlapping packs;
- injected races with concurrent loose writers and packers;
- fsck corruption fixtures for every manifest and duplicate rule;
- the two-worktree Git experiment in CI or a documented release gate;
- migration, unpack, crash-resume, and rollback tests;
- proof that all existing schema-1 scripts and loose-only stores behave exactly
  as before while shadow packs retain loose copies.
