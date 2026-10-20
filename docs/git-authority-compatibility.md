# Git authority binary and replay compatibility

`mote --json authority capabilities` works without a store or network access.
It reports the executing file's path and BLAKE3 digest, package version, explicit
release ownership, and the exact seven immutable profiles compiled into it.
This evidence identifies the actual executable. Source SHA, dirty state and
target metadata are explicitly unavailable pending F10; the command never
infers them from the Git checkout where it happens to run.

`authority status`, `doctor`, read-only `fsck`, and constrained `audit` fetch and
validate the current chain. Their output includes store, generation, root, head,
sequence and observation time, manifest requirements, binary evidence and
structural compatibility. `doctor`, `fsck`, and `audit` label their scope as
`git-authority-canonical-chain-v1`; they do not claim to audit local operations,
candidate reachability, TTL presence, or filesystem cleanup. `authority cached`
is the separate offline path and always labels its provenance as stale.

Manifest validation rejects unknown schemas, ownership policy and capability
sets before any legacy replay or transaction planning. Profiles are exact,
sorted sets; adding a capability to an existing root is not an upgrade path.
On incompatibility, use `authority capabilities` to identify a compatible
executable. Do not recreate or downgrade the authority. A compatible manifest
alone cannot establish legacy replay compatibility: a migrated root additionally
requires the verified packed archive and its exact retained closed-build replay
profile. An ordinary build can diagnose its supported profiles but refuses a
migrated root whose replay profile it cannot verify.

New Git stores fence the old local writer and reducer with their FORMAT mode
and descriptor. Console, TUI, batch/import, candidate and role routes have no
remote implementation in this release scope and refuse before local publication.
The command matrix and regression tests describe the supported surface.

The first release targets local machines using a shared Git remote. Cloud
qualification follows separately. A physical simultaneous-machine pilot and
hosted policy qualification remain required before claiming adoption readiness.
Migration does not transfer machine-local routing or credential configuration.

Rollback is limited to an unused migrated authority: it must win the first
canonical transaction, permanently retire the shared authority, and restore the
verified stopped backup into a new private `.mote` directory. Once shared work
has occurred, conversion back to local mode is deferred by the user's release
scope decision. Preserving canonical operation order, long logical IDs and read
cursors requires a separately versioned local continuation format; no lossy
exporter is provided.

## Remote event and actor observations

`events` emits JSONL when `--json` is selected. Each row carries fresh authority
provenance and `data.cursor`, `data.phase`, `data.position`, and `data.event`.
Legacy accepted operations precede remote accepted effects. Cursors bind the
authority root to a legacy source ordinal or remote transaction sequence and
effect index:

```
ga:ROOT:legacy:00000000000000000000:000000
ga:ROOT:remote:00000000000000000001:000000
```

Ordering never uses timestamps or the lexical ordering of combined adapter IDs.
Rejected receipts produce no accepted event. Filters select output without
changing its positions. `--after` must identify an actual emitted event position
in this root; foreign, invented and future positions refuse. Resume with the same
cursor after reconnecting a replacement client. Total machine-cache loss requires
an explicit `authority connect` to restore local binding/routing custody; it does
not change the cursor or authority root. Watch and follow polling publish no
requests, acknowledgements, read cursors or ownership renewals. A fetch failure
stops a fresh stream; it does not silently emit a cached authoritative state.

Actor observations distinguish explicit session enrollment from process
liveness, which remains unknown. Claims and open reservations come only from
canonical ownership maps. Message and discussion attention comes from replayed
domain state. Supplemental recent activity uses client-supplied timestamps and
is descriptive; neither timestamps nor TTL expiry release ownership. Legacy
presence filters and age-based audit modes refuse rather than imitate local TTL
semantics.
