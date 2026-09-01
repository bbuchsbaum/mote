# Candidate, Review, and Landing Authorization Protocol

**Status:** candidate protocol with v2 convergence, role-review quorum, containment recovery, and out-of-band reconciliation implemented
**Applies to:** Mote v0.2 immutable-op storage
**Implementation beads:** candidate core, visibility, reservation binding, and
pairwise evidence convergence

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

### 3.2 Proposal and landing repository identities

Candidate state uses three identities together:

1. `store_id`, copied from `.mote/FORMAT.json`, identifies the coordination
   domain.
2. The proposal repository id (wire field `repository_id`) is the canonical
   hash of the Git object format and canonicalized `git-common-dir` observed by
   the proposing CLI. It remains immutable ancestry provenance.
3. `landing_repository_id` is the same kind of hash for the Git repository
   containing the configured shared `.mote` store. Object availability,
   governed landing, and out-of-band reconciliation are checked against this
   identity.

The receipt stores hashes, not absolute common-directory paths. Linked Git
worktrees share an identity, while independent clones do not. A proposal may
therefore originate in a standalone clone without binding its terminal state
to that clone. It records a typed object source containing the proposal
repository id, supplied commit ref, and an optional explicit locator such as a
path or pushed ref. Mote displays but never follows that locator implicitly.

Legacy proposals omit the landing identity and initially use their proposal
repository for both roles. Their authorizer may publish an explicit
`candidate_landing_repository_bind` operation, with phase and binding CAS plus
a reason, to bind a still-pending row to the repository backing the shared
store. This preserves proposal provenance and allows an already transferred or
landed exact object to reach an honest terminal state. A missing Git
repository, unresolvable common directory, unsupported object format, or
inaccessible object database never produces a repository id by guesswork.

### 3.3 Git object identities

All object ids are lowercase, full-length ids paired with an explicit object
format (`sha1` or `sha256`). Abbreviated hashes are rejected. A proposal records:

- candidate commit object id;
- declared base commit object id;
- direct parent commit object ids in Git order;
- immutable proposal repository id and current landing-repository binding;
- proposal commit ref and optional explicit object locator;
- bead id;
- normalized declared repository-relative paths;
- submitter actor;
- one landing authorizer actor;
- a non-empty review policy: named reviewer actors, role/count requirements,
  or both;
- named evidence requirements;
- informational evidence references;
- proposal operation id and timestamp.

The submitter must not be the landing authorizer or a named reviewer and can
never review their own candidate through a role. The landing authorizer must
not be a named reviewer. A role definition may separately exclude the
authorizer from consuming a role slot.

Legacy proposal version 1 retains its non-empty all-of list of named reviewers.
Role-aware proposal version 2 leaves the legacy `reviewers` array empty and
carries a separate review policy with sorted named reviewers plus sorted exact
role ids, definition op ids, and positive approval counts. A required count may
not exceed immutable role capacity. This split is deliberate: an older binary
that ignores the new field sees an empty legacy list and rejects the proposal
instead of silently weakening it.

Portable proposal version 3 also leaves the legacy `reviewers` array empty and
requires both a structured review policy and the landing-repository/object-source
pair. Version 1 and 2 proposals reject that portable pair, while version 3
rejects its absence. This makes mixed-version replay fail closed instead of
letting an older reducer silently treat a portable proposal as repository-bound.

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

Receipt freshness is structural, never wall-clock based. In v1, one ancestry
receipt had to cover the exact current candidate set. Under the v2 amendment,
the candidate's own receipt must still match its immutable anchors and prove its
base, while coverage is resolved independently for each candidate pair from an
exact observation published by either member of that pair. A current landing
receipt must name the current authorization op and the current evidence/review
basis op ids. Timestamps are provenance only.

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

- object format and the repository id in which this observation was made;
- candidate, base, and direct-parent object ids as observed from Git objects;
- whether base is an ancestor of candidate;
- one relation from every other known candidate commit to the immutable base,
  and one to the candidate tip: `ancestor`, `not_ancestor`, `unavailable`, or
  `ambiguous`;
- the sorted candidate ids and proposal op ids covered by the observation;
- the Git command/tool version and object-database identity used by the probe.

The v1 receipt is globally current only if it covers every same-landing-repository
candidate known at the reducer point where landability is queried. A candidate
proposed after the receipt therefore makes v1-only coverage stale, because that
later record may name an older ancestor commit. Section 4.5 replaces this
global-freshness rule when an exact pair observation is available.

The recorded candidate, base, and parents must exactly match the immutable
proposal, and the observation repository must be either the immutable proposal
repository or the current landing repository. This permits a later refresh in
the shared repository after object transfer without rewriting proposal
provenance. Base ancestry must be `true`. Missing objects, shallow-history
gaps, repository mismatch, an incomplete candidate set, or a legacy receipt
without the base-relative relation are ambiguous and block. A later evidence
refresh can replace that receipt without changing any historical candidate
operation.

### 4.5 Candidate protocol v2: convergent pair observations

Git commit ancestry is immutable, so evidence about a pair does not need to be
owned by the candidate whose landability is being queried. Requiring every old
candidate to refresh after every new proposal creates a non-convergent
protocol, and it is impossible when the old producer cannot see uncommitted
proposal operations in another store checkout. V2 therefore materializes
ancestry evidence as exact, reusable observations of candidate pairs.

For a receipt whose subject is candidate `S`, `relation_schema: 2` distinguishes
v2 rows; a missing value or `1` is legacy. A relation row for known candidate
`K`, together with its enclosing evidence operation, is bound to both immutable
proposals and records all four directed facts:

- `K.tip -> S.base` and `K.tip -> S.tip`, retained in the existing
  `base_relation` and `relation` fields;
- `S.tip -> K.base` and `S.tip -> K.tip`, added as
  `subject_to_known_base` and `subject_to_known_tip`;
- the exact candidate ids, proposal op ids, commit ids, and base ids for `S` and
  `K`, plus their repository id and object format. Subject identities may come
  from the enclosing accepted evidence operation and receipt anchors; they are
  not duplicated merely to make the row self-contained.

Each direction is `ancestor`, `not_ancestor`, `unavailable`, or `ambiguous`.
The base and tip observations are a unit: a row missing either required fact is
partial and cannot resolve that direction. A claim that a commit is an ancestor
of a candidate base but not its tip is internally inconsistent and ambiguous.

The reducer maintains up to two current pair-observation registers for an
unordered pair: the latest row explicitly published by each candidate's named
`git-ancestry` producer. A later receipt updates a pair register only when it
explicitly includes that pair. Omission from a later, incomplete snapshot does
not erase an earlier fact about immutable commits; an explicit later
`unavailable` or `ambiguous` row does replace the earlier row from that same
side. The ordinary latest-receipt rule still governs the subject candidate's
own anchor and base proof.

To evaluate candidate `C` against candidate `O` in the same landing repository, the reducer
accepts either `O -> C` facts from `C`'s receipt or the reciprocal `O -> C`
facts from `O`'s receipt. An observation is eligible only when every stored id
matches both immutable proposals exactly. Determinate eligible observations
must agree. A conflicting determinate observation yields `ancestor_ambiguous`;
an unavailable observation does not overrule a consistent determinate
observation from the other side. If no complete determinate observation exists,
the pair remains blocking as `git_evidence_stale` or `ancestor_ambiguous`.

This rule changes the safe convergence direction: publishing a later proposal
may temporarily make an older candidate stale, but the later proposal's normal
ancestry receipt can resolve both candidates at once. It never allows the mere
absence of a relation to mean `not_ancestor`, and it never imports patch or tree
equivalence as ancestry.

Every v2 receipt also declares the producer snapshot used to choose its known
candidate set:

- coordination `store_id`;
- sorted observed candidate ids and proposal op ids;
- count and canonical digest of the replayed operation-id set;
- Git commit of the store checkout, when available;
- count of uncommitted operation files in that checkout, when available.

Absolute store paths are not recorded. Snapshot provenance is diagnostic, not
an authority claim and not a substitute for an exact pair row. When the target
store contains a proposal absent from a receipt declared after that proposal,
the diagnostic must say that the proposal was absent from the producer
snapshot and that refreshing from the same incomplete checkout cannot fix the
target. If the evidence operation predates the proposal, the diagnostic instead
says that the observation predates it. In both cases the remedy is to publish a
pair observation from a store containing both exact proposal operations.

### 4.6 Landing-repository object availability

Portable proposals publish a built-in `git-object-availability` receipt after
the proposal and ancestry receipt. The probe runs against the Git repository
containing the configured shared store, not the proposer's current clone. It
records the landing repository id, object format, exact candidate object id,
observed direct parents, Git version, outcome, and diagnostic detail. A pass
requires the exact commit and immutable parent list to be readable. A missing
object yields `object_unreachable`; a missing or indeterminate observation
yields `object_availability_missing` or `object_availability_unavailable`.
Every code blocks landability.

The proposing CLI prints a loud warning when the initial observation does not
pass but retains the pending row and recorded source locator so transfer can be
coordinated. `candidate evidence availability` publishes a later observation
from the shared repository. The proposer, authorizer, named reviewer, eligible
holder of a required review role, actor with a recorded role review, or current
landing grantee may publish this built-in receipt. A passing exact landing or
reconciliation reachability receipt in the current landing repository also
proves object availability for that transition.

As with every receipt, availability is an immutable observation rather than a
claim that mutable Git storage can never later be pruned. The final landing and
reconciliation commands probe exact target reachability again. Reducer replay
never opens the repository or silently fetches an object.

### 4.7 Landing-target scope

A portable or explicitly rebound candidate must publish `git-target-scope`
before it can become landable. The command observes an explicit target ref in
the repository backing the shared store and records the derived repository id,
current landing-repository binding op, exact target OID, candidate and base
OIDs, proof that the candidate was not already reachable from that target,
every merge base, and a sorted conservative effective-path set. The
repository identity and OIDs come from Git; they are not caller-selected
labels.

The path set is the union of `merge-base..candidate` for every merge base.
Rename inference is disabled, deliberately representing a rename as its source
deletion and destination addition. Every effective path must overlap an
immutable declared candidate path. A missing observation yields
`target_scope_evidence_missing`; a receipt bound to another repository,
repository-binding clock, target, or candidate anchor yields
`target_scope_evidence_stale`; an effective path outside policy yields
`target_scope_uncovered`. All three block.

The target ref is mutable, so reducer replay claims only the exact recorded OID.
The final landing receipt names the current target-scope evidence and operation
ids. A fast-forward must move from that target OID to the candidate; a merge
must name that OID as the resulting commit's first parent. This exact-preimage
check rejects target advancement between scope observation and landing rather
than treating an older ancestor as current. Refreshing target scope publishes a
new immutable evidence operation; it never rewrites candidate policy.
An already-reachable candidate cannot obtain target-scope evidence: that state
uses the explicit `candidate reconcile` path and remains visibly
`landed_out_of_band`, rather than laundering ambient reachability into a
governed landing.

### 4.8 Immutable-producer ancestry recovery

Ordinary `candidate evidence refresh` remains owned by a producer named in the
immutable evidence requirement. When every named Git-ancestry producer is
unavailable, the immutable proposal authorizer may instead publish an audited
Git-only refresh with `--operator-override`. This does not inherit a producer's
identity or authority. The operation records and the reducer checks:

- the exact current candidate phase operation, authorizer identity, and
  landing-repository identity;
- the sorted immutable producer set and the exact prior evidence operation for
  each producer;
- that every prior receipt was passing and anchored to the candidate's exact
  repository or landing repository, object format, commit, base, and parents;
- a fresh ancestry receipt produced by the acting authorizer; and
- a non-empty reason plus sorted, non-empty durable authority references.

The acting authorizer must be distinct from every named producer. The override
can refresh reproducible Git facts after a terminal transition because it never
changes candidate phase, reviews, authorization, or proposal provenance.
Missing prior receipts, a stale phase, a policy mismatch, a failed Git probe, or
an authorizer that is itself a named producer fails closed.

Authority references are durable attestations for later human review. Mote
records and audits them, but does not dereference them or treat them as a
machine-rooted grant of authority. Every accepted use emits
`candidate_ancestry_operator_override_recorded`.

### 4.9 Containment-backed supersession recovery

Ordinary supersession remains owned by the predecessor's proposer or immutable
authorizer. A second, explicit recovery mode exists for the case where neither
owner is available: the successor's immutable authorizer may retire the pending
predecessor only when the v2 pair registers completely and determinately prove
`predecessor.tip -> successor.tip` as `ancestor`.

The recovery form of `candidate_supersede` records:

- authority `successor_authorizer_containment`;
- compare-and-set clocks for both the pending predecessor and pending successor;
- the sorted exact evidence operation ids currently resolving that pair.

The reducer re-derives the pair solely from accepted operations and requires the
recorded evidence-id set to equal the current determinate basis. Missing or
partial base/tip facts, `not_ancestor`, `unavailable`, `ambiguous`, conflicting
rows, stale evidence ids, an identity mismatch, or either phase changing causes
rejection. Equal commits count as containment because Git ancestry is reflexive.

Recovery never consults session liveness or wall-clock age: those observations
cannot grant authority inside deterministic replay. It does not bulk-retire old
candidates, infer patch equivalence, or turn containment into a governed landing
claim. The supersession record retains actor, authority, evidence ids, op id,
and timestamp, and `mote audit` surfaces recovery use for review.

### 4.10 Out-of-band reachability reconciliation

When a pending candidate is already reachable from an explicit Git target but
the formal landing transition was never recorded, the proposal's immutable
authorizer may publish `candidate_reconcile`. This is a repair record, not a
retroactive authorization claim. The CLI first resolves the explicit target
once and records a built-in `git-reachability` receipt containing the exact
repository, object format, candidate OID, target ref, observed target OID, Git
version, and `merge-base --is-ancestor` result.

The reconciliation operation names that receipt and carries:

- authority `proposal_authorizer`;
- the current pending phase op as a compare-and-set expectation;
- the current review-policy and landing-repository binding op ids;
- sorted current review, candidate-evidence, and relevant pair-evidence op ids;
- the current authorization op and status, if any;
- the complete pre-transition structured landability result.

The reducer re-derives this policy snapshot and requires exact equality. Only a
passing receipt produced by that same authorizer in the bound landing
repository for the exact candidate and target is accepted. A missing ref,
repository mismatch, shallow or unavailable
object, non-ancestor, arbitrary actor, stale policy register, or competing
terminal transition fails closed. The result is the distinct terminal phase
`landed_out_of_band`; it stores the observed target OID and the complete policy
basis. It does not create, grant, revoke, or consume authorization and never
claims that formal review or authorization governed the Git landing.

A second, explicit operator-recovery form exists when the immutable proposal
authorizer cannot act. A distinct operator supplies `--operator-override`, the
exact current phase clock, a non-empty reason, and sorted non-empty durable
authority references. The operation also binds the exact original authorizer
and landing-repository identity. The reducer accepts this form for a pending
candidate, or for an abandoned candidate whose exact commit is already
reachable from the target. It rejects superseded, landed, and already
reconciled candidates. Ordinary proposal-authorizer reconciliation remains
pending-only, and the proposal authorizer may not masquerade as an override
operator.

The explicit override does not confer inherited authorizer authority. Its
references are durable attestations, not a machine-validated grant; Mote stores
them so an independent reviewer can assess the recovery basis and emits
`candidate_reconciliation_operator_override_recorded`.

Operator recovery probes the Git repository backing the store. If that
repository differs from the candidate's recorded landing repository, the
operation carries a typed repository bridge: the exact current landing-binding
clock plus object-availability proof for the immutable commit and parent list
in the observed repository. The reachability receipt is then bound to that
observed repository while the proposal and landing repository identities remain
unchanged. Missing objects, changed parents, a stale binding, or a repository or
tool mismatch fails closed. Audit emits
`candidate_reconciliation_repository_bridge_recorded`; the bridge is evidence
of where the recovery was observed, never a provenance rewrite.

## 5. Review model

Each actor has one compare-and-set review register per candidate. A
`candidate_review` operation contains:

- candidate id;
- reviewer actor;
- verdict: `approve`, `block`, or `comment`;
- optional evidence references and body;
- `expect_review`, naming that reviewer's prior accepted review op or `none`;
- either the legacy named-reviewer qualification, or one exact role id,
  assignment id, and observed assignment clock.

Without a role binding, only an actor named in the current policy may review.
With a role binding, the reducer requires version 2, a current assignment clock,
an active assignment held by the operation actor, and an exact required role.
The proposer is always ineligible. The role's typed candidate exclusions are
rechecked both when accepting the review and whenever landability is derived.

A review consumes at most one policy slot. A named reviewer who deliberately
reviews through a role no longer satisfies their named slot, and one actor
holding several roles cannot fill several quorum slots with one operation.
Each role count therefore means distinct eligible actors.

Role qualification remains attached to one assignment id. Renewing that same
assignment preserves the review, but release, TTL expiry, bound-session end,
role retirement, or a newly applicable typed exclusion stops it from counting.
A later new assignment has a new id and does not revive an old approval. The
new or reassigned holder publishes their own review. An eligible `block` blocks
that role requirement only while its assignment remains eligible; `comment` is
visible but never counts. Named review behavior remains all-of and unchanged.

Two concurrent reviews by one actor with the same expectation cannot both win.
Replay order accepts the first and rejects the stale second. Reviews from
different actors commute. Current JSON reports the policy clock, amendment
history, every review qualification, eligible approvals and blocks per role,
ineligible review reasons, and the explicit `as_of_ts`.

### 5.1 Named-reviewer amendments

The proposal authorizer may replace only the named-reviewer portion of a
pending candidate's policy. `candidate_review_policy_amend` records the full
replacement set, the current candidate phase, the current policy op id, a
non-empty reason, actor, timestamp, and its own operation id. The replacement
must retain at least one named or role/count requirement and must preserve the
proposer/authorizer separation rules. Role ids, role counts, candidate object,
paths, and every other proposal anchor remain immutable.

The reducer preserves the entire review register. A named approval continues
to count when its actor remains named, stops counting when that actor is
removed, and is never converted into an approval for a newly added reviewer.
Removing and later re-adding the same actor does not revive their pre-removal
approval; they must advance their review register with its normal CAS.
Amendment history exposes the before, after, added, and removed sets. Two
concurrent amendments from one policy clock cannot both win. This operation is
the bounded remedy for reviewer availability; it does not infer content
equivalence or transfer a review to a different commit.

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
| Change a review verdict | a named reviewer, or an actor with an explicit currently eligible assignment to a required role |
| Amend pending named-reviewer policy | proposal-named landing authorizer only |
| Bind pending landing repository | proposal-named landing authorizer only |
| Grant or revoke landing authorization | the one proposal-named landing authorizer only |
| Supersede candidate | predecessor submitter/authorizer; or successor authorizer with explicit containment recovery |
| Abandon candidate | submitter or landing authorizer |
| Record landing | actor in the current grant's grantee set |
| Reconcile an out-of-band landing | proposal-named landing authorizer for an ordinary pending repair; or a distinct operator with the exact audited override basis |
| Change terminal candidate state | only `abandoned` to `landed_out_of_band` through the explicit audited operator-reconciliation exception |

The reducer compares op actor strings to the current recorded policy. This is strong
coordination attribution but, as stated in the safety boundary, not
cryptographic authentication.

## 7. Candidate phase and transitions

Candidate phase is separate from review and authorization state.

| Operation | Required actor | Preconditions | Result |
|---|---|---|---|
| `propose` | submitter | unique id; valid initial policy and Git identities | `pending` |
| `evidence` | recorded producer | candidate exists; receipt binds exact object id | updates one evidence register |
| `review` | named reviewer or eligible role holder | phase `pending`; review expectation matches; role form also matches exact active assignment clock and exclusions | updates that actor's one review register |
| `amend-reviewers` | landing authorizer | phase `pending`; phase and review-policy expectations match; replacement remains valid and differs | replaces named reviewers, preserves review records, advances policy clock |
| `bind-landing-repository` | landing authorizer | phase `pending`; phase and repository-binding expectations match; target is the repository backing the shared store | preserves proposal origin, advances landing binding, requires a fresh availability receipt |
| `authorize` | landing authorizer | phase `pending`; authorization expectation matches | `granted` or `conditional` |
| `revoke` | landing authorizer | phase `pending`; authorization expectation matches | `revoked` |
| `supersede OLD NEW` | old submitter or old authorizer | both pending; same store, landing repository, object format, and bead; old phase expectation matches; no existing successor | old becomes `superseded` and points to new |
| `supersede OLD NEW --containment-recovery` | new authorizer | ordinary identity checks; both phase clocks match; current exact v2 pair evidence completely proves old tip is an ancestor of new tip | old becomes `superseded`, retaining recovery authority and evidence basis |
| `abandon` | submitter or landing authorizer | phase `pending`; phase expectation matches | `abandoned` |
| `landed` | actor in current grant | phase `pending`; exact grant expectation matches; candidate is landable; landing receipt passes | `landed`, grant becomes `consumed` |
| `reconcile` | proposal authorizer | phase `pending`; phase and complete policy snapshot match; authorizer-produced reachability receipt passes for the exact target | `landed_out_of_band`, authorization unchanged, pre-transition blockers retained |
| `reconcile --operator-override` | actor distinct from proposal authorizer | phase `pending`, or `abandoned` for legacy recovery; exact phase, original authorizer, landing repository, non-empty reason, sorted durable references, complete policy snapshot, and acting-operator reachability receipt match; a cross-repository probe also carries the exact binding and object/parent bridge | `landed_out_of_band`, original provenance and authorization unchanged, pre-transition blockers retained, audit warnings emitted |

`pending` is the only non-terminal phase. `superseded`, `abandoned`, `landed`,
and `landed_out_of_band` are terminal. Authorization `revoked` is not a terminal candidate
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
satisfy its own policy. The supersession record snapshots every predecessor
review that was not carried, and human output reports the total and approving
actors, so the coordination cost is visible at the transition boundary.

Every mutating candidate command accepts an actor-scoped idempotency key.
Reusing a key with an identical canonical payload returns the original accepted
result; reusing it with different content is rejected. A concurrent exact retry
that loses reducer publication is likewise reported as the same success after
replay finds the accepted canonical payload.

## 8. Derived landability

Landability is a deterministic result with a boolean and a sorted list of
structured reasons. It is never just a boolean in JSON. Every reason retains
its stable string `code` and also carries:

- `class`: `substantive`, `process`, or `bookkeeping`; and
- `blocking`: a boolean independent from the class.

`substantive` means an explicit finding about the candidate or an ancestor's
work. `process` means a required review, evidence, eligibility, or
authorization gate is incomplete or unavailable. `bookkeeping` means the
candidate graph, Git provenance, or terminal-state record needs resolution.
Classification is presentation and triage metadata, not policy: every current
reason has `blocking=true`, so this addition does not weaken landability.
Human candidate detail groups reasons by blocking effect and class while JSON
keeps the established code/subject/detail order.

A candidate is landable only when all of these are true:

1. Phase is `pending`.
2. The current Git ancestry receipt passes, matches the proposal anchors, and
   proves the immutable base-to-tip relation.
3. Base is a proven ancestor of the candidate.
4. Every currently known candidate in the same landing repository has a complete,
   identity-bound pair observation from at least one side, and all determinate
   observations of that pair agree.
5. Every known candidate proven to be an ancestor is either:
   - `landed`; or
   - `landed_out_of_band`, with its explicit non-governed reconciliation
     record; or
   - `superseded` by this candidate through a complete supersession chain; or
   - `abandoned`, with its commit proven to be in both this candidate's
     immutable base and tip.
6. No ancestor is pending, blocked by review, authorization-revoked, ambiguous,
   or abandoned and introduced after the immutable base.
7. Every named reviewer currently approves and every role requirement has its
   distinct eligible approval count with no eligible block.
8. Every declared evidence requirement currently passes.
9. A current typed receipt proves the exact candidate object is readable from
   the bound landing repository.
10. For portable or rebound candidates, a current target-scope receipt is bound
    to the landing repository and binding clock, and every conservative
    effective path is covered by immutable policy.
11. Authorization is currently granted or conditional.
12. Every condition named by a conditional grant passes.
13. The prospective landing actor is in the grant's grantee set when checking
    permission for `candidate landed`.

An unknown relationship is not `not_ancestor`. It is ambiguity and blocks. In
particular, an abandoned commit that reaches the tip but has missing,
unavailable, or ambiguous base-relative proof remains fail-closed.

The closed reason vocabulary is classified as follows:

- Substantive: `review_blocking`, `review_role_blocking`, `evidence_failed`,
  `ancestor_blocked`, `target_scope_uncovered`.
- Process: `review_missing`, `review_role_unavailable`,
  `review_role_approval_ineligible`, `review_role_quorum_missing`,
  `evidence_unavailable`, `evidence_missing`, `git_evidence_unavailable`,
  `git_evidence_missing`, `object_unreachable`,
  `object_availability_unavailable`, `object_availability_missing`,
  `target_scope_evidence_missing`,
  `actor_not_grantee`, `condition_unsatisfied`,
  `authorization_revoked`, `authorization_absent`,
  `ancestor_authorization_revoked`.
- Bookkeeping: `candidate_missing`, `phase_not_pending`, `base_not_ancestor`,
  `git_evidence_stale`, `ancestor_ambiguous`, `ancestor_missing`,
  `repository_mismatch`, `proposal_anchor_mismatch`,
  `target_scope_evidence_stale`, `supersession_cycle`,
  `supersession_broken`, `ancestor_supersession_unresolved`,
  `ancestor_pending`, `ancestor_abandoned`.

Adding a reducer reason requires extending the typed vocabulary and choosing
both properties. JSON must retain the related candidate, reviewer, evidence,
or authorization subject and full detail.

## 9. Landing evidence

`candidate landed` requires a fresh built-in `git-landing` receipt. For a
portable or rebound candidate it also requires a current `git-target-scope`
receipt for the exact target ref before Git is changed. The landing receipt records
the target ref, target tip before and after landing, the candidate object id,
the current authorization op id, the current review and evidence basis op ids,
the exact target-scope evidence/op ids, and proof that the candidate commit is
reachable from the after-tip. The target-scope OID must be the exact
fast-forward preimage or the first parent of the resulting merge. The
receipt must be from the bound landing repository and object format, regardless
of which repository supplied proposal ancestry.

Mote records the landing after an external Git action; it does not perform the
action. Failure to publish the landed op leaves the candidate pending and the
grant unconsumed, so a retry uses an idempotency key or the exact expected
authorization op. A landed claim without reachability evidence is rejected.

`candidate reconcile` uses the narrower `git-reachability` receipt. It proves
only that the immutable candidate OID is an ancestor of the one target OID
resolved for the operation. The retained policy snapshot is evidence of what
was missing or present immediately before the terminal record. Unlike
`candidate landed`, reconciliation does not require landability, does not use a
grantee, and does not consume a grant. Its human, JSON, event, watch, TUI, and
audit labels must say `landed_out_of_band` or `out-of-band`; they must not
collapse it into governed `landed`.

## 10. Reducer and replay invariants

1. Same accepted op set and filename order yields byte-equivalent materialized
   candidate state and landability reasons on every machine.
2. Reducer behavior is independent of current Git refs, filesystem paths,
   network state, wall clock, and environment variables.
3. Candidate commit, base, proposal repository, role/count policy, bead, and
   declared paths never mutate. Only the named-reviewer set and landing
   repository binding may advance through authorizer-owned compare-and-set
   registers.
4. Every mutable register transition carries an expectation; at most one
   concurrent transition from one prior value is accepted.
5. Terminal candidate phases never reopen to `pending`. The sole terminal-to-
   terminal correction is audited operator reconciliation from `abandoned` to
   `landed_out_of_band`; `superseded`, `landed`, and `landed_out_of_band` remain
   immutable.
6. A grant is consumable at most once and only by a named grantee.
7. Evidence for one candidate object id cannot satisfy another candidate.
8. Reviews and authorization never transfer through supersession.
9. Adding a candidate can only preserve or reduce another candidate's
   landability until exact pair evidence is published. A valid pair observation
   may then restore landability from either candidate's named producer, but an
   omission, identity mismatch, or unknown relationship cannot make a candidate
   safer.
10. Missing, malformed, unknown, stale, unavailable, or ambiguous evidence is
    never interpreted as pass.
11. Rejected operations remain visible in history and do not mutate candidate
    state.
12. Mote never claims that Git was changed; it records only evidence and state
    transitions supplied by actors.
13. One actor review satisfies at most one named or role slot, and one role
    requirement counts each eligible actor at most once.
14. A role approval cannot outlive or transfer away from its exact assignment
    id; all time-dependent eligibility is derived from one injected timestamp.
15. Ancestry receipts record whether they observed the immutable anchors in the
    proposal or landing repository. Availability, landing, and reconciliation
    receipts bind only the current landing repository. Neither identity may
    silently substitute for the other.

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

A later proposal makes the pair temporarily unresolved. Landability changes to
`git_evidence_stale` until an exact receipt from either candidate relates both
immutable proposals. In the normal ordered case, the later candidate's initial
v2 receipt supplies the reciprocal row and restores the older candidate without
requiring its producer to refresh.

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

An ancestry receipt from neither the proposal nor landing repository, or an
availability or landing receipt from another landing repository, does not
match its anchor.
An object-format mismatch is always rejected. Cross-clone portability changes
which identity a receipt must match; it never treats the identities as
interchangeable.

### 11.9.1 Standalone proposal clone

A proposal resolved in standalone clone `S` records `S` as proposal provenance
and shared repository `L` as its landing domain. If the object is absent from
`L`, proposal succeeds with a warning but landability contains
`object_unreachable`. After the exact object is transferred and availability
is refreshed, review and authorization may make the row landable. Landing in
`L` is accepted because its receipt matches the landing binding, not `S`.

### 11.10 Actor spoofing

The op log truthfully records the supplied actor string but cannot prove who
controlled it. This limitation is explicit; installations needing stronger
identity must provide signed evidence in a future protocol.

### 11.11 Concurrent proposals from incomplete snapshots

Candidates `A` and `B` are proposed from snapshots that contain neither other
proposal. Neither initial receipt can relate the pair. Both remain
`git_evidence_stale`; replay order does not invent a winner. A later probe from
a store containing both proposal operations publishes one exact row and
resolves the pair for both candidates. If the target has uncommitted operations
that neither producer can see, diagnostics identify that snapshot boundary
rather than instructing either producer to repeat the same impossible refresh.

### 11.12 Conflicting pair observations

One eligible row says `A.tip` is an ancestor of `B.tip`; the other says it is
not. Both rows name the same proposal, commit, base, repository, and object
format identities. The pair yields `ancestor_ambiguous` for affected
landability until a later explicit row replaces the conflicting register.
Filename order alone never converts disagreement into proof.

### 11.13 Ownerless contained predecessor

The predecessor's proposer and authorizer may both be absent, but absence is not
itself replayable authority. If an exact v2 pair observation proves the
predecessor commit is contained in a pending successor, the successor's named
authorizer may publish containment recovery with both phase clocks and the
current evidence op ids. A stale clock, partial row, unrelated commit, arbitrary
actor, or later conflicting current register is rejected. Once accepted, the
terminal supersession is immutable and its recovery basis remains visible.

### 11.14 Out-of-band reconciliation races

Reconciliation, governed landing, abandonment, and supersession all expect the
same pending phase clock, so only the first terminal operation can win. Review,
authorization, evidence, or pair-evidence changes between the reachability
receipt and reconciliation change the policy snapshot and reject the stale
operation. A successful reconciliation leaves any grant exactly as recorded;
later revoke, grant, review, evidence, or terminal operations are rejected
because the candidate is terminal. Exact retries use the accepted operation's
idempotency record and do not probe ambient Git again.

### 11.15 Role reassignment and quorum races

Two actors assigned from one capacity snapshot cannot both overfill the role,
and two reviews from one actor cannot create two quorum votes. A renewal keeps
the same assignment id and therefore preserves its review. A release or expiry
immediately makes that vote ineligible at the injected snapshot time; a later
heartbeat or new assignment cannot revive it. If a role-bound blocker loses
eligibility, the block no longer governs, but the missing quorum remains until
an eligible holder approves. Supersession removes the terminal predecessor's
contextual role demand and never transfers its reviews to the successor.

## 12. CLI and JSON disposition

Core implementation provides:

```text
mote candidate propose --reviewer ACTOR --object-source LOCATOR
mote candidate propose --require-reviews 2 --from-role reviewer
# Repeat the paired flags for multiple role requirements; named and role
# requirements may be combined.
mote candidate show
mote candidate list
mote candidate evidence refresh
# Break glass only when every immutable ancestry producer is unavailable:
mote candidate evidence refresh CANDIDATE --operator-override \
  --expect-phase OP_ID --reason TEXT --authority-ref REF
mote candidate evidence availability CANDIDATE
mote candidate evidence target-scope CANDIDATE --target REF
mote candidate review CANDIDATE approve
mote candidate review CANDIDATE approve --from-role reviewer
mote candidate amend-reviewers CANDIDATE --reviewer ACTOR \
  --expect-phase OP_ID --expect-policy OP_ID --reason TEXT
mote candidate bind-landing-repository CANDIDATE \
  --expect-phase OP_ID --expect-repository OP_ID --reason TEXT
mote candidate authorize
mote candidate revoke
mote candidate supersede
mote candidate abandon
mote candidate landed
mote candidate reconcile
# A distinct operator may record exact out-of-band reachability when the
# immutable authorizer cannot act; abandoned rows require this form.
mote candidate reconcile CANDIDATE --target REF --expect-phase OP_ID \
  --operator-override --reason TEXT --authority-ref REF \
  --idempotency-key KEY
```

Mutating commands return exit `0` on accepted or idempotently matched state,
`2` on reducer rejection, `3` on invalid input, and `4` on repository/storage
failure. Git evidence unavailable during an explicit refresh is recorded as an
accepted receipt with a non-pass outcome; it is not fabricated as a command
success claim.

Candidate JSON has stable top-level identity (including proposal and landing
repositories plus object-source provenance), phase, policy, reviews,
`review_status`, evidence, authorization, supersession, landing,
reconciliation, and landability objects. `review_status` carries its
`as_of_ts`; `landability` always contains `landable`, `reason_codes`, and
structured `reasons`. Each reason contains `code`, `class`, `blocking`, an
optional `subject`, and `detail`.

## 13. Migration and compatibility

- Legacy candidate proposal and named-review operation shapes remain version
  `1`. Role-aware proposals and role-bound reviews use version `2`.
- Legacy reconciliation snapshots whose reasons predate `class` and `blocking`
  derive both fields from the stable reason code during deserialization. This
  preserves their policy snapshot equality and fail-closed replay behavior.
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
- A version-2 proposal stores named reviewers inside `review_policy` and leaves
  the legacy `reviewers` array empty. Older reducers therefore reject it under
  the existing non-empty-reviewer invariant rather than accepting only the
  subset of policy they understand. All candidate writers and landability
  readers must be upgraded before role quorum is used.
- A version-3 portable proposal additionally requires both the landing
  repository and object-source provenance, stores all named and role review
  requirements in `review_policy`, and leaves the legacy `reviewers` array
  empty. Versions 1 and 2 reject this portable shape, and version 3 rejects a
  repository-bound shape. Upgrade candidate writers and reducers before
  portable proposals are published.
- V2 adds optional relation-schema, reciprocal-relation, known-base, and
  producer-snapshot fields to `git-ancestry` payloads. The candidate operation
  protocol version remains `1`. Old binaries leave the immutable operation
  files untouched but may reject a v2 evidence operation when recomputing its
  payload digest without the unknown fields; all candidate writers and
  landability readers therefore need the v2-capable binary before rollout.
- Containment recovery adds an optional typed `recovery` object to
  `candidate_supersede`. Legacy supersession operations deserialize with no
  recovery and retain predecessor-owner semantics; older binaries may reject a
  recovery operation during canonical digest verification and must be upgraded
  before participating in that transition.
- Named-reviewer amendments add `candidate_review_policy_amend`, a current
  policy clock, amendment history, and a supersession snapshot of reviews not
  carried. Older binaries may reject the new op kind and must be upgraded
  before writers use reviewer substitution.
- Portable repository binding adds proposal-version-3 fields for
  `landing_repository_id` and object-source provenance, the
  `candidate_landing_repository_bind` op, and `git_object_availability`
  evidence. Legacy proposals default the landing repository to their proposal
  repository until an authorizer explicitly binds the pending row. Older
  reducers reject a v3 proposal rather than silently dropping its portability
  contract; all candidate participants must be upgraded before portable
  candidates are written.
- Target-scope evidence is mandatory for every portable v3 candidate and every
  legacy candidate after explicit landing-repository binding. Legacy candidates
  that remain repository-bound retain their prior landing behavior. The new
  evidence payload and landing receipt fields are additive, but older reducers
  do not enforce them; all candidate landability readers and landing writers
  must be upgraded together.
- Out-of-band reconciliation adds the `git_reachability` evidence payload,
  `candidate_reconcile` op, and `landed_out_of_band` phase. Older binaries may
  reject these additive variants and must be upgraded before writing candidate
  operations in a store that uses reconciliation.
- Audited ancestry recovery adds a distinct Git-ancestry override payload.
  Operator reconciliation adds the explicit authority variant, durable override
  basis, and optional cross-repository bridge. All candidate writers, reducers,
  and audit readers must be upgraded before these recovery forms are used.
- A legacy relation row remains valid in its original direction when its
  candidate id, proposal op id, commit id, repository, and receipt anchors all
  match. Missing reciprocal fields can never be used to resolve the opposite
  candidate.
- A v2 reducer rebuilds pair registers entirely from accepted immutable
  evidence operations. No migration operation, backfill, or ambient Git read is
  required. Pairs with only incomplete legacy evidence remain fail-closed until
  an explicit refresh publishes the missing direction.
- No migration deletes, edits, or repacks legacy operations.

## 14. Implementation boundary

The core implementation bead owns typed operations, state, pair projections,
reducer laws, Git receipt production, landability, CLI commands, JSON, and
focused tests. The visibility bead owns events, watch, in-flight, and TUI. The
reservation-binding bead owns candidate references and lease lifecycle
integration. This document is normative when those beads make implementation
choices.
