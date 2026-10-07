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
`authority_version` (`0` or `1`), `genesis_digest`, and the capabilities
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

Every in-tree publisher, including library and server writers, takes an exclusive
OS lock on `.mote/local/publication.lock`. Do not delete or replace that file
while any Mote process is running. The OS releases the lock when its process
exits. Normal read-only replay does not take the write lock.

The first strict operation preserves the existing filename-ordered replay prefix
in `.mote/authority/00000000000000000000.json`. Subsequent immutable, contiguous
20-digit admission records each bind an operation filename and its full BLAKE3
content digest. `FORMAT.json` records the authority version and genesis digest.
Replay follows admission order. A later publication with an earlier timestamp
cannot reorder this prefix or undo a committed landing authorization.

Publication uses a durable `authority/publication.json` containing the exact
operation name and bytes, then the existing Maildir publication protocol, then
an atomic durable admission record. It finally removes the publication journal.
After an interruption, the next writer finishes those exact bytes before any new
mutation. A pending operation is not visible to readers until admitted. The
journal decision belongs before subsequent writers even if recovery happens
later. Retries must retain the original request key and arguments.

Back up or version **FORMAT.json, ops/, and authority/ together**. The numbered
records, pending journals, and completed landing receipts are source of truth.
Copying only the op files is no longer sufficient. Missing admission records,
changed admitted bytes, and unadmitted files copied into `ops/` fail closed.
`fsck` checks these bindings too. Event op cursors follow admission order in
fenced stores. Resuming from a timed synthetic cursor conservatively replays raw
operation IDs, which consumers can deduplicate; it does not skip late admissions. Do not hand-edit journals, remove a pending
landing, merge independently writable authority directories, or run older Mote
binaries against a fenced store. All worktrees must select the same `MOTE_STORE`.

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
and direct publication bypass detection. Fault hooks exist only in debug builds.
