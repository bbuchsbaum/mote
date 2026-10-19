# Shared-store authority protocol

`candidate land` and `handoff` provide prospective authority at a serialized
mutation boundary. They activate authority format 1 for their store. This is a
single-workstation protocol for upgraded Mote writers sharing the **same store**
and a local filesystem with working POSIX file locks. It is not a distributed
consensus protocol or an authentication system. Actor names remain attributable
strings, as in the candidate protocol.

## Authority discovery and activation

Automation can validate the local authority without activating or recovering it:

```sh
mote authority status
```

It prints `mote.authority-status.v1` JSON with `store_id`, `enabled`,
`authority_version` (`0`, `1`, or `2`), `genesis_digest`, `binding` (`local`,
`format` for a binding not yet migrated, or null), `foreign_format_binding`, and
the capabilities
`stable_claim_order`, `holder_checked_handoff`, and
`checked_landing_results`. Enabled status validates the immutable admission
prefix but does not acquire a writer lock, recover journals, or mutate the
store. To activate deliberately, use the idempotent writer operation:

```sh
mote authority enable
```

`mote begin` activates under the same local writer lock after validating its
arguments and before making its first reservation timestamp or reading claim
state. Direct library publication against an unactivated store retains legacy
filename ordering and is provisional; strict callers must activate or use an
upgraded writer first.

## Publication and replay

Activation is per replica. Running `begin` in a store whose `ops/` is tracked
by Git prints a note to that effect, because ops pulled from other clones must
then be admitted explicitly (see below).

Every in-tree publisher, including library and server writers, takes an exclusive
OS lock on `.mote/local/publication.lock`. Do not delete or replace that file
while any Mote process is running. The OS releases the lock when its process
exits. Normal read-only replay does not take the write lock.

The first strict operation preserves the existing filename-ordered replay prefix
in `.mote/authority/00000000000000000000.json`. Subsequent immutable, contiguous
20-digit admission records each bind an operation filename and its full BLAKE3
content digest. `local/authority.json` records the authority version and genesis
digest for this replica. Replay follows admission order. A later publication with an earlier timestamp
cannot reorder this prefix or undo a committed landing authorization.

Publication uses a durable `authority/publication.json` containing the exact
operation name and bytes, then the existing Maildir publication protocol, then
an atomic durable admission record. It finally removes the publication journal.
After an interruption, the next writer finishes those exact bytes before any new
mutation. A compound command retaining its writer also recovers any pending
publication before publishing another operation. If recovery fails, the original
journal remains intact and the new operation is not published. `begin` propagates
publication errors in its optional note and compensation steps rather than
continuing after a partial write. Work already admitted is retained.
A pending operation is not visible to readers until admitted. The
journal decision belongs before subsequent writers even if recovery happens
later. Retries must retain the original request key and arguments.

Back up **FORMAT.json, ops/, authority/, and local/authority.json together**.
The numbered records, pending journals, and completed landing receipts are
source of truth for the replica. Copying only the op files is no longer
sufficient to restore it. Missing admission records, missing or changed admitted
op files, and unadmitted files copied into `ops/` fail closed. `fsck` checks
these bindings too. Event op cursors follow admission order in
fenced stores. Resuming from a timed synthetic cursor conservatively replays raw
operation IDs, which consumers can deduplicate; it does not skip late admissions. Do not hand-edit journals, remove a pending
landing, merge independently writable authority directories, or run older Mote
binaries against a fenced store. All worktrees must select the same `MOTE_STORE`.

## Replicated op logs and per-replica authority

A store whose `ops/` is versioned in Git has one replica per clone (machine,
cloud VM, CI checkout). The model is:

- **Replicated:** `FORMAT.json` and `ops/`. Op files are immutable and uniquely
  named, so merging two clones' `ops/` is a set union.
- **Per replica:** `authority/` (admission records, publication and landing
  journals, landing receipts) and `local/` (including the binding in
  `local/authority.json`). Never commit them, and never merge two replicas'
  copies: admission records are sequential, and two writers produce different
  record N for the same slot. Ignore `.mote/authority/`, `.mote/local/`, and
  `.mote/tmp/`. `mote doctor` warns when Git tracks any of them, when it does
  not ignore `authority/` in a fenced replica of a versioned op log, and when a
  tracked `FORMAT.json` still carries a legacy binding.

An op that arrives by `git pull` has no admission record in this replica. Reads
fail closed with `unadmitted operation …; run mote admit`. Admit it under the
writer lock:

```sh
mote admit           # or: mote --json admit  (mote.admit.v1)
```

`admit` first recovers any pending publication, refuses to run while a landing
needs recovery, and validates the complete admitted prefix. It then validates
every unadmitted op as `fsck` does: filename, JSON, content hash, op shape, and
envelope id and timestamp. Any defect fails closed and nothing is admitted. An
admitted op whose bytes changed (the same op id with different bytes, as when a
merge replaces a file) fails closed as `authority op changed`. Valid ops are
appended as one atomic admission record, in op-id order, after the existing
prefix. The prefix is never reordered. Admitting a session-bound claim raises the
authority version before the claim becomes visible, as publication does. Run
`admit` after every pull, or from a `post-merge` hook, and do not suppress its
exit status.

**Ordering.** Each replica orders foreign ops after its own admitted history, so
replicas agree on the op set but not necessarily on the order. Ops on different
issues replay identically everywhere. Concurrent conflicting ops (two claims on
one issue, a close racing a patch) resolve in each replica's admission order and
can resolve differently on different replicas. `admit` reports, as
`out_of_order`, the admitted ops whose id sorts before the previous admitted
tail: these are exactly the ops a pre-authority replay would have placed
earlier. Surfacing or rejecting semantic conflicts is not implemented.

**Exclusivity.** Claims, reservations, holder-checked handoff, and fenced landing
are exclusive within one replica. A claim made on another machine is visible
after a pull and admit, but it never excluded anyone here. Landing receipts and
pending journals are strictly per replica and are never replicated.

**Fresh clones.** A clone has `ops/` but no `authority/` or `local/`. It is
unfenced until its first strict write (`begin`, `claim`, `authority enable`),
which records the clone's current filename-ordered ops as its own genesis.

### Binding migration

Earlier binaries recorded the binding in the `authority` field of `FORMAT.json`,
a file that versioned stores track. This binary keeps the binding in
`local/authority.json`:

- New activations write only `local/authority.json`; `FORMAT.json` is unchanged.
- In an existing store bound only in `FORMAT.json`, the first writer copies that
  binding (same version and genesis digest) into `local/authority.json`. Reads
  and `authority status` keep working before and after; `binding` reports which
  copy governs. `FORMAT.json` is left untouched.
- `mote authority migrate` then removes the `FORMAT.json` field after the local
  binding is durable, rewriting `FORMAT.json` in the layout `init` creates. It
  keeps the higher version if an earlier binary raised only the `FORMAT.json`
  copy. It is idempotent.
- A `FORMAT.json` binding for another genesis came from another replica (for
  example a fresh clone of a repository where someone committed the field). A
  replica with its own local binding ignores it for reads; earlier binaries
  fail closed on it. A replica without a local binding fails closed and names
  the remedy, `mote authority migrate --discard-foreign-format`, which removes
  only a binding that does not name this replica's genesis. Use it only when
  you know the field was replicated, not when this replica's `authority/` was
  lost.

Compatibility with earlier binaries: they ignore `local/authority.json`. They
read a migrated store correctly, but each of their writes adds the `FORMAT.json`
binding back (for the same genesis, so this binary accepts it and `migrate`
removes it again). They cannot admit foreign ops and fail closed on them.
Earlier binaries also enforced the version fence only through `FORMAT.json`, so
once a store is migrated they are no longer fenced out by a version raise.
Upgrade every binary that writes the store, then migrate.

## Fenced landing

First record exact target-scope evidence and obtain the required reviews and
landing grant. Then, as a named grantee:

```sh
mote --actor landing-agent --json candidate land cand-... \
  --target main --before FULL_OLD_OID \
  --expect-phase PHASE_OP --expect-authorization AUTHORIZATION_OP \
  --idempotency-key landing-1
```

This primitive supports a nonempty fast-forward of a branch to the candidate's
immutable commit. It verifies the bound repository, object format, exact target
preimage, current target-scope observation, paths, policy, reviews, evidence,
authorization, and caller. A merge result must be proposed and reviewed as its
own immutable candidate. The operation updates the ref only; it does not update
an index or working tree, merge, rebase, push, or bypass branch protection.

Under the store lock, Mote constructs and validates the exact future evidence
and confirmation operations and durably writes `authority/landing-active.json`.
This prepare is the authorization decision. It binds the entire admitted prefix,
actor, candidate, repository, target, old/new OIDs, and exact confirmation bytes.
Mote then performs `git update-ref --no-deref` with the old-OID CAS and a unique
reflog receipt, publishes confirmation, and archives the journal by retry key.

If revocation is accepted before prepare, landing is refused and Git is unchanged.
If prepare wins, revocation waits; after confirmation it is rejected against the
consumed grant. Other Mote writes remain blocked after an interrupted prepare
until that exact landing is recovered. Role/lease eligibility is evaluated at
the durable decision, not at a later restart time.

Repeat the **same command and key** to recover. Recovery uses the bound repository,
current ref, unique reflog receipt, and exact journaled operation bytes. It never
resets Git. If the ref moved after Mote's update, or confirmation is rejected or
unknown, the command returns nonzero, with `outcome=recovery_required`, the
journal path, exact `old_oid`, `new_oid`, and `current_oid`, and `git_updated=true`
when the update is established. An indeterminate Git outcome uses
`git_updated=null` and `git_updated_unknown=true`; it is never described as an
unchanged-ref abort. A failed cleanup also returns nonzero, even after a confirmed
landing. While a matching active journal remains, the retry is a
`current_completion` and can succeed only after cleanup and an exact current-ref
observation. Once only the archive remains, the retry is an `archived_receipt`:
it reports `historically_confirmed` and the current ref without updating Git
again. A cleanup or drift error retained in that archive remains nonzero.
Preserve journals and reflogs until recovery finishes.

`candidate show` and `candidate list` include `landability_actor` and evaluate a
selected actor's grantee eligibility. A null actor identifies a generic policy
view. A display is not a mutation fence. The existing `candidate landed` command
continues to record an external landing; it does not provide this prospective
contract. `candidate reconcile` still records out-of-band reachability.

## Holder-checked handoff

```sh
mote --actor alice --json handoff bd-... --to carol \
  --expect-holder alice --expect-claim CLAIM_OP \
  --idempotency-key handoff-1 --note 'Tests remain'
```

Both preconditions are required together and are copied verbatim into one
`handoff` operation. The reducer requires that the open issue still has that
exact live holder/token and that the sender is its holder. A public claim token
does not authorize a nonholder, including through the lower-level claim or
release operations. Release requires the current holder and, when supplied,
the exact claim token; no coordinator override is inferred from possession of
that public token. A handoff cannot turn a stale observation of
Alice/T1 into a transfer of Bob/T2. Ordinary claim CAS renewal/transfer likewise
requires the current live holder. After expiry, acquisition is a fresh claim,
not a stale-token transfer.

The note, claim transfer, and optional `--release` of the sender's reservations
are one reducer decision. There is no success after only the note or only a
reservation release. Keys are scoped to the sender; byte-identical retries
return the original accepted or rejected operation, without extending a lease,
adding another note, or transferring a later claim. Changed arguments conflict.
The JSON result distinguishes `transferred`, `applied_no_longer_current`,
`conflict`, `unauthorized`, `expired`, and `recovery_required`, and reports the
current holder/token and expiry. A historical accepted transfer does not imply
that its ownership is still current.

Interactive callers may omit both preconditions; Mote selects only the caller's
own claim under the same publication fence. Omitting a retry key generates one,
returned in JSON. Automated callers should supply all three values.

Without `--release`, the sender's carrier reservations become orphaned and retain
their existing TTL and conflict protection. **No reservation is transferred or
renewed by handoff.** Adoption is a separate accepted transition, requiring a
live orphan, its exact reservation clock, and the adopter's live issue claim:

```sh
mote --actor carol --json adopt rv-... --issue bd-... \
  --expect-reservation RESERVATION_OP
```

Adoption validates under the publication fence. Competing accepts using the same
clock have one winner. Once the carrier TTL expires, protection is gone and
adoption is rejected. Neither a message ACK nor a successful claim transfer
proves that adoption succeeded.

## Acceptance evidence

`tests/fenced_landing.rs` exercises real Git ref CAS, both sides of the revocation
boundary, grantee denial, late earlier-stamped revocation, journal interruption
and restart, byte-identical retries, and a moved ref after Git mutation.
`tests/handoff_authority.rs` exercises replacement claims, nonholder operations,
expiry, competing transfers/adoptions, atomic release, and publication restart.
`tests/authority.rs` covers frozen replay order, missing/tampered authority data,
and direct publication bypass detection. `tests/replicated_authority.rs`
exchanges fenced ops between two Git clones and admits them, and covers
fail-closed admission, pending-publication recovery, binding migration, foreign
bindings, and the `doctor` Git checks. Fault hooks exist only in debug builds.
