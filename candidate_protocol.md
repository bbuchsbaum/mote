# Candidate, Review, and Landing Authorization Protocol

**Status:** accepted design for candidate protocol v1
**Applies to:** Mote v0.2 immutable-op storage
**Implementation beads:** candidate core, visibility, and reservation binding

## 1. Purpose

Mote currently records discussion about a proposed Git change but has no
machine-readable answer to these questions:

- Which exact commit is the candidate?
- Which base and parent chain were inspected?
- Who must review it and who may authorize landing?
- Which evidence is required, and did it pass for this exact commit?
- Does the commit contain another known candidate that is unlanded or blocked?
- Was authorization granted, made conditional, revoked, or consumed?

Candidate protocol v1 answers those questions with immutable operations and a
deterministic reducer. Mote records coordination state; it never merges, rebases,
pushes, or mutates Git refs.

The motivating failure is a candidate whose touched-path tree looked safe while
its ancestry contained a separate blocked, unlanded candidate. Tree equality is
not ancestry evidence and can never make a candidate landable under this
protocol.

## 2. Safety boundary and non-goals

The protocol is a fail-closed social coordination mechanism, not a security
boundary.

- Actor names are attributable strings, not cryptographic identities.
- Evidence receipts prove what an identified producer recorded, not that the
  producer or external service is honest.
- Mote does not infer authority from prose, a Git remote, branch protection, or
  a successful push.
- Mote does not treat reservations as filesystem locks.
- Candidate v1 supports exact Git object inclusion. Squash and cherry-pick
  equivalence are not inferred from patches or trees.
- Reducer replay never opens a Git repository, accesses the network, reads a
  clock, or runs a command.

An installation that needs cryptographic signatures or hosted branch-policy
enforcement must add those as explicit evidence producers in a later protocol.

## 3. Identity and immutable proposal

### 3.1 Candidate identity

A candidate id is `cand-` followed by a ULID. It is globally unique within a
store and never reused. A changed commit is a new candidate; it never mutates an
existing candidate.

### 3.2 Repository identity

Candidate v1 uses two identities together:

1. `store_id`, copied from `.mote/FORMAT.json`, identifies the coordination
   domain.
2. `repository_id` is the canonical hash of the Git object format and the
   canonicalized `git-common-dir` identity observed by the proposing CLI.

The receipt stores the hash, not the absolute common-directory path. Linked Git
worktrees therefore share an identity, while two independent clones on one
workstation do not silently share candidate state. Moving or replacing the
common directory requires a new candidate; v1 deliberately favors local
fail-closed identity over cross-clone portability. A missing Git repository,
unresolvable common directory, unsupported object format, or inaccessible
object database produces an `unavailable` or `ambiguous` receipt; it never
produces a repository id by guesswork.

### 3.3 Git object identities

All object ids are lowercase, full-length ids paired with an explicit object
format (`sha1` or `sha256`). Abbreviated hashes are rejected. A proposal records:

- candidate commit object id;
- declared base commit object id;
- direct parent commit object ids in Git order;
- bead id;
- normalized declared repository-relative paths;
- submitter actor;
- one landing authorizer actor;
- a non-empty set of required reviewer actors;
- named evidence requirements;
- informational evidence references;
- proposal operation id and timestamp.

The submitter must not be the landing authorizer and must not be a required
reviewer. The landing authorizer may also be a required reviewer. Required
reviewers use all-of semantics in v1; threshold policies are deferred.

Declared paths are coordination metadata. They neither prove the Git diff nor
limit ancestry checks.

## 4. Evidence receipts

### 4.1 Recorded facts versus ambient observations

Git and external checks are observed before publication and serialized in a
`candidate_evidence` operation. Once published, the receipt is an immutable
fact about that observation. The reducer uses only the receipt.

`candidate propose` may publish the proposal and initial Git receipt as a
compound sequence. A crash between them leaves a visible candidate with
missing evidence and therefore `landable=false`; no compensation is required
for safety. `candidate evidence refresh` publishes a later receipt.

Receipt freshness is structural, never wall-clock based. A current ancestry
receipt must cover the exact current candidate set. A current landing receipt
must name the current authorization op and the current evidence/review basis
op ids. Timestamps are provenance only.

### 4.2 Common receipt envelope

Every receipt contains:

- stable `evidence_id` derived from the canonical payload hash;
- candidate id and exact candidate object id;
- evidence name and kind;
- producer actor and producer/tool description;
- observation timestamp;
- outcome: `pass`, `fail`, `unavailable`, or `ambiguous`;
- canonical payload digest;
- optional human references such as log paths or hosted run URLs.

The latest accepted receipt for one candidate, evidence name, and required
producer is current. A later `fail`, `unavailable`, or `ambiguous` result
replaces an earlier pass for landability.

### 4.3 Declared evidence requirements

Each proposal declares evidence requirements by stable name, kind, and a
non-empty set of required producer actors. A requirement passes only when the
latest receipt from every required producer is `pass` and is bound to the
candidate object id. Conditional authorization may reference only names already
declared by the proposal; it cannot create an underspecified requirement in
prose.

### 4.4 Git ancestry receipt

Every candidate has a mandatory built-in requirement named `git-ancestry`. Its
payload records:

- object format, repository id inputs, and repository id;
- candidate, base, and direct-parent object ids as observed from Git objects;
- whether base is an ancestor of candidate;
- one relation from every other known candidate commit to the immutable base,
  and one to the candidate tip: `ancestor`, `not_ancestor`, `unavailable`, or
  `ambiguous`;
- the sorted candidate ids and proposal op ids covered by the observation;
- the Git command/tool version and object-database identity used by the probe.

The receipt is current only if it covers every same-repository candidate known
at the reducer point where landability is queried. A candidate proposed after
the receipt makes coverage stale until refreshed, because that later record may
name an older ancestor commit.

The recorded candidate, base, and parents must exactly match the immutable
proposal. Base ancestry must be `true`. Missing objects, shallow-history gaps,
repository mismatch, an incomplete candidate set, or a legacy receipt without
the base-relative relation are ambiguous and block. A later evidence refresh
can replace that receipt without changing any historical candidate operation.

## 5. Review model

Each required reviewer has a compare-and-set review register. A
`candidate_review` operation contains:

- candidate id;
- reviewer actor;
- verdict: `approve`, `block`, or `comment`;
- optional evidence references and body;
- `expect_review`, naming that reviewer's prior accepted review op or `none`.

Only a reviewer named in the proposal may publish a review transition.
`comment` is visible but does not satisfy review. Every required reviewer must
currently be `approve`. Any `block`, `comment`, or missing review makes the
candidate not landable.

Two concurrent reviews by one reviewer with the same expectation cannot both
win. Replay order accepts the first and rejects the stale second. Reviews from
different required reviewers commute.

## 6. Authorization model

Authorization is a compare-and-set register owned by the one landing authorizer
named in the proposal.

An authorization grant records:

- candidate id;
- state: `granted` or `conditional`;
- non-empty grantee actor set permitted to perform and record landing;
- named evidence conditions for a conditional grant;
- `expect_authorization`, naming the prior accepted authorization op or `none`.

A revoke records the same expectation and moves authorization to `revoked`.
The authorizer may grant again from `revoked`, but candidate-bound reservations
closed by the revocation do not automatically reopen.

Landing consumes the exact grant. `candidate_landed` names the current
authorization op in `expect_authorization`; it is rejected if a revoke or newer
grant won first. After landing, authorization is derived as `consumed` and no
further grant or revoke is accepted.

This register makes contradictory grant, revoke, and landing instructions
machine-resolvable. Timestamp prose is never authorization.

### 6.1 Authority matrix

| Action | Accepted actor |
|---|---|
| Propose candidate | actor recorded as submitter |
| Publish evidence | any actor; only receipts from requirement-named producers satisfy that requirement |
| Change a review verdict | that named required reviewer only |
| Grant or revoke landing authorization | the one proposal-named landing authorizer only |
| Supersede candidate | original submitter or landing authorizer |
| Abandon candidate | submitter or landing authorizer |
| Record landing | actor in the current grant's grantee set |
| Change terminal candidate state | nobody |

The reducer compares op actor strings to this immutable policy. This is strong
coordination attribution but, as stated in the safety boundary, not
cryptographic authentication.

## 7. Candidate phase and transitions

Candidate phase is separate from review and authorization state.

| Operation | Required actor | Preconditions | Result |
|---|---|---|---|
| `propose` | submitter | unique id; valid immutable policy and Git identities | `pending` |
| `evidence` | recorded producer | candidate exists; receipt binds exact object id | updates one evidence register |
| `review` | named reviewer | phase `pending`; review expectation matches | updates that reviewer register |
| `authorize` | landing authorizer | phase `pending`; authorization expectation matches | `granted` or `conditional` |
| `revoke` | landing authorizer | phase `pending`; authorization expectation matches | `revoked` |
| `supersede OLD NEW` | old submitter or old authorizer | both pending; same store, repository, and bead; old phase expectation matches; no existing successor | old becomes `superseded` and points to new |
| `abandon` | submitter or landing authorizer | phase `pending`; phase expectation matches | `abandoned` |
| `landed` | actor in current grant | phase `pending`; exact grant expectation matches; candidate is landable; landing receipt passes | `landed`, grant becomes `consumed` |

`pending` is the only non-terminal phase. `superseded`, `abandoned`, and
`landed` are terminal. Authorization `revoked` is not a terminal candidate
phase because the authorizer may regrant, but it is a terminal event for any
candidate-bound reservation active at that moment.

A reservation can bind directly to a pending candidate. Its normalized paths
must be a subset of the candidate's immutable declared exact paths. The
binding remains an advisory TTL lease: it never locks the filesystem and it
participates in the same overlap checks as a bead-bound reservation. Landing,
abandonment, supersession, or an accepted authorization revoke derives the
binding as orphaned while its TTL remains live. That orphaned lease remains
conflict-producing and visible in candidate JSON, reservation events,
in-flight views, watch, and the TUI. Regranting after a revoke does not revive
reservations active at the revoke; after releasing or adopting the orphan, a
newly opened reservation after the grant is distinct. Existing bead-only
reservation operations retain their original wire shape and replay behavior.

Supersession is immutable, one-to-one from an old candidate, and acyclic. The
new candidate does not inherit reviews, evidence, or authorization. It must
satisfy its own policy.

Every mutating candidate command accepts an actor-scoped idempotency key.
Reusing a key with an identical canonical payload returns the original accepted
result; reusing it with different content is rejected. A concurrent exact retry
that loses reducer publication is likewise reported as the same success after
replay finds the accepted canonical payload.

## 8. Derived landability

Landability is a deterministic result with a boolean and a sorted list of
reason codes. It is never just a boolean in JSON.

A candidate is landable only when all of these are true:

1. Phase is `pending`.
2. The current Git ancestry receipt passes, matches the proposal anchors, and
   covers every currently known same-repository candidate.
3. Base is a proven ancestor of the candidate.
4. Every known candidate proven to be an ancestor is either:
   - `landed`; or
   - `superseded` by this candidate through a complete supersession chain; or
   - `abandoned`, with its commit proven to be in both this candidate's
     immutable base and tip.
5. No ancestor is pending, blocked by review, authorization-revoked, ambiguous,
   or abandoned and introduced after the immutable base.
6. Every required reviewer currently approves.
7. Every declared evidence requirement currently passes.
8. Authorization is currently granted or conditional.
9. Every condition named by a conditional grant passes.
10. The prospective landing actor is in the grant's grantee set when checking
    permission for `candidate landed`.

An unknown relationship is not `not_ancestor`. It is ambiguity and blocks. In
particular, an abandoned commit that reaches the tip but has missing,
unavailable, or ambiguous base-relative proof remains fail-closed.

Required reason codes include:

- `phase_not_pending`
- `git_evidence_missing`, `git_evidence_stale`, `git_evidence_unavailable`
- `repository_mismatch`, `proposal_anchor_mismatch`, `base_not_ancestor`
- `ancestor_pending`, `ancestor_blocked`, `ancestor_abandoned`
- `ancestor_authorization_revoked`, `ancestor_ambiguous`
- `review_missing`, `review_blocking`
- `evidence_missing`, `evidence_failed`, `evidence_unavailable`
- `authorization_absent`, `authorization_revoked`, `condition_unsatisfied`
- `actor_not_grantee`

Human output may summarize these reasons; JSON must retain stable codes and the
related candidate, reviewer, evidence, or authorization ids.

## 9. Landing evidence

`candidate landed` requires a fresh built-in `git-landing` receipt. It records
the target ref, target tip before and after landing, the candidate object id,
the current authorization op id, the current review and evidence basis op ids,
and proof that the candidate commit is reachable from the after-tip. The
receipt must be from the same repository and object format.

Mote records the landing after an external Git action; it does not perform the
action. Failure to publish the landed op leaves the candidate pending and the
grant unconsumed, so a retry uses an idempotency key or the exact expected
authorization op. A landed claim without reachability evidence is rejected.

## 10. Reducer and replay invariants

1. Same accepted op set and filename order yields byte-equivalent materialized
   candidate state and landability reasons on every machine.
2. Reducer behavior is independent of current Git refs, filesystem paths,
   network state, wall clock, and environment variables.
3. Candidate commit, base, policy, bead, and declared paths never mutate.
4. Every mutable register transition carries an expectation; at most one
   concurrent transition from one prior value is accepted.
5. Terminal candidate phases never reopen.
6. A grant is consumable at most once and only by a named grantee.
7. Evidence for one candidate object id cannot satisfy another candidate.
8. Reviews and authorization never transfer through supersession.
9. Adding a candidate can only preserve or reduce another candidate's
   landability until ancestry coverage is refreshed; it cannot silently make
   another candidate safer.
10. Missing, malformed, unknown, stale, unavailable, or ambiguous evidence is
    never interpreted as pass.
11. Rejected operations remain visible in history and do not mutate candidate
    state.
12. Mote never claims that Git was changed; it records only evidence and state
    transitions supplied by actors.

## 11. Adversarial cases

### 11.1 Hidden blocked ancestor

Graph: `BASE -> BLOCKED -> CANDIDATE`. `BLOCKED` is a known, unlanded candidate.
Even if `git diff BASE..CANDIDATE -- declared-paths` equals an expected tree, the
ancestry receipt reports `BLOCKED=ancestor`. Landability returns
`ancestor_pending` or `ancestor_blocked`.

### 11.2 Contradictory grant and revoke

Grant and revoke both expect authorization `none`. Whichever operation sorts
first is accepted; the other is rejected stale. If revoke expects and follows
the grant, state is revoked. Prose timestamps cannot override either result.

### 11.3 Landing races revocation

Landing and revocation both name the current grant. If revoke wins, landing is
stale and rejected. If landing wins, authorization is consumed and revocation
is rejected because the candidate is terminal.

### 11.4 Git is unavailable or shallow

The probe records `unavailable` or `ambiguous`. The candidate remains visible
with explicit reason codes and cannot land until a complete receipt is added.

### 11.5 Candidate appears after ancestry inspection

A later proposal makes the older coverage set stale. Landability changes to
`git_evidence_stale` until a receipt relates the new candidate.

### 11.6 Concurrent supersession

Two successors race from the same pending candidate. Both name the same phase
expectation. Exactly one supersession is accepted; the other is rejected.

### 11.7 Reusing old approval or authorization

A replacement candidate has a new id and object id. Reviews, evidence, and
grants for the old candidate do not match and cannot satisfy it.

### 11.8 Patch-equivalent cherry-pick

A cherry-picked or squashed commit has a different object id. Patch or tree
similarity is not exact inclusion; v1 reports missing landing reachability and
does not infer equivalence.

### 11.9 Repository or object-format mismatch

A receipt from another root-history set or object format does not match the
proposal and yields `repository_mismatch`.

### 11.10 Actor spoofing

The op log truthfully records the supplied actor string but cannot prove who
controlled it. This limitation is explicit; installations needing stronger
identity must provide signed evidence in a future protocol.

## 12. CLI and JSON disposition

Core implementation provides:

```text
mote candidate propose
mote candidate show
mote candidate list
mote candidate evidence refresh
mote candidate review
mote candidate authorize
mote candidate revoke
mote candidate supersede
mote candidate abandon
mote candidate landed
```

Mutating commands return exit `0` on accepted or idempotently matched state,
`2` on reducer rejection, `3` on invalid input, and `4` on repository/storage
failure. Git evidence unavailable during an explicit refresh is recorded as an
accepted receipt with a non-pass outcome; it is not fabricated as a command
success claim.

Candidate JSON has stable top-level identity, phase, policy, reviews, evidence,
authorization, supersession, and landability objects. `landability` always
contains `landable`, `reason_codes`, and structured `reasons`.

## 13. Migration and compatibility

- Candidate ops are new additive tagged variants with their own protocol
  version `1`; existing op shapes do not change.
- A new binary replays a legacy store into an empty candidate map without
  rewriting any operation.
- Candidate ids cannot collide with bead, reservation, message, post, or
  session ids because the prefix is distinct.
- Existing prose reviews and push authorizations are not silently imported.
  Operators must propose a candidate and record explicit transitions.
- Unknown candidate op kinds remain immutable files to an older binary, which
  may report them as malformed. Therefore candidate workflows require all
  participating writers to use a candidate-capable Mote version; ordinary
  legacy issue operations remain structurally unchanged.
- A partially upgraded store is safe by failure: missing candidate evidence or
  transitions cannot produce `landable=true` in a capable binary.
- No migration deletes, edits, or repacks legacy operations.

## 14. Implementation boundary

The core implementation bead owns typed operations, state, reducer laws, Git
receipt production, landability, CLI commands, JSON, and focused tests. The
visibility bead owns events, watch, in-flight, and TUI. The reservation-binding
bead owns candidate references and lease lifecycle integration. This document
is normative when those beads make implementation choices.
