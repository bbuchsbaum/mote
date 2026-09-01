# Operational Audit and Git-Backed Store Hygiene

**Status:** implemented, including explicit out-of-band candidate reconciliation
**Applies to:** Mote v0.2 immutable-op storage and candidate protocol v3
**Implementation beads:** operational audit core and Git-backed store hygiene

## 1. Decision

Mote must distinguish three kinds of evidence instead of calling every
difference `drift`:

1. **Recorded state** is the deterministic projection of immutable Mote
   operations. A pending candidate, open bead, named assignee, or live session
   lease is not false merely because ambient Git or recent activity suggests a
   different follow-up action.
2. **Derived state** is computed only from the accepted op set and an explicit
   `as_of_ts`, such as readiness, lease disposition, child completion, or
   candidate landability. It is replayable and must not consult Git.
3. **Observed state** comes from the current filesystem, Git object database,
   refs, or clock. It is useful operational evidence, but it is labelled with
   its source and observation time and never rewrites recorded state.

`mote doctor` remains the storage, format, integrity, and identity diagnostic.
It gains one narrowly storage-level `git_backing` report because uncommitted op
files change which immutable source records are transportable to another
checkout. Cross-surface semantic findings belong to a new read-only
`mote audit` command.

Neither command mutates refs, publishes Mote operations, assigns an actor,
closes a bead, retires a candidate, or consumes landing authorization.

## 2. Why the boundary matters

- A TTL-valid session can be live while its actor has authored no recent op.
  Liveness and activity are separate facts; silence is not lease drift.
- A reservation holder coordinates a path. It is not thereby the bead
  assignee, reviewer, or owner.
- An open parent with closed children may be a release gate or umbrella. It is
  reviewable, not automatically closed.
- A pending candidate whose commit is reachable from a target ref was landed
  outside the recorded protocol. Ambient reachability does not retroactively
  prove review, authorization, or a governed landing transition.
- A command that displays only the first few landability reasons has a display
  problem, not inconsistent reducer state. Structured reason codes remain the
  authority.

This vocabulary prevents an audit convenience from weakening append-only
governance or converting coordination metadata into authorization.

## 3. `mote doctor`: physical store visibility

Doctor adds a `git_backing` object while keeping its existing `ok` and exit-code
semantics. Identity warnings and Git-backing warnings do not make a clean store
corrupt.

```json
{
  "git_backing": {
    "mode": "git_backed",
    "repository_root": "/redacted/in-json-only-if-already-public",
    "head": "<full oid>",
    "tracked_op_count": 7071,
    "working_op_count": 7114,
    "uncommitted_op_count": 43,
    "untracked_op_count": 41,
    "modified_op_count": 1,
    "deleted_op_count": 1,
    "oldest_uncommitted_op_ts": "2026-08-30T13:23:12.284353Z",
    "oldest_uncommitted_age_s": 1804,
    "as_of_ts": "2026-08-30T13:53:16.000000Z"
  }
}
```

`mode` is one of:

- `git_backed`: at least one operation path is present in the Git index;
- `local_untracked`: the store is inside a worktree but no operation is
  tracked, matching Mote's default local-store policy;
- `not_in_git`: the store is outside a Git worktree;
- `unavailable`: Git could not answer, with a structured detail.

Only `git_backed` produces an uncommitted-op warning. A wholly untracked local
store is not warned merely for following the documented default policy.

The implementation compares the operation filenames on disk with `git
ls-files`; it does not rely only on `git status`, because ignored new op files
are still absent from clones. Modified, staged, deleted, and on-disk untracked
operation paths contribute once to `uncommitted_op_count`. The oldest timestamp
comes from the sortable op filename, not file mtime. Deleted or malformed names
remain counted and receive a detail if no age can be derived.

Human output states the consequence and remedy:

```text
warn: git-backed op store has 43 uncommitted operations; oldest is 30m04s
      clones and worktrees populated from Git cannot observe them; commit or
      otherwise synchronize the op files before asking those producers to refresh
```

Doctor never runs `git add`, `git commit`, `git push`, or candidate refresh.

## 4. `mote audit` contract

```text
mote audit [--target-ref <ref>] [--stale-after <duration>]
           [--fail-on error|warning|never]
```

- The command is read-only and takes one coherent store snapshot before
  computing findings.
- `--target-ref` is required for ambient candidate reachability checks. Mote
  does not guess `main`, the current branch, or a remote default branch.
- `--stale-after` enables age-based informational findings. There is no hidden
  inactivity threshold.
- `--fail-on` defaults to `error`. Exit `2` means a finding met the threshold,
  `3` means invalid input, and `4` means the store could not be opened or
  replayed. Git visibility failures are structured `candidate_git_unavailable`
  findings so `--fail-on` controls them consistently. `never` is suitable for
  dashboards.
- Findings are sorted by severity, code, and subject. Human and JSON views use
  the same finding objects.

The JSON schema is additive and versioned:

```json
{
  "schema_version": 1,
  "ok": false,
  "as_of_ts": "...",
  "fail_on": "error",
  "context": {
    "store_id": "st-...",
    "target_ref": "refs/heads/main",
    "target_oid": "<full oid>"
  },
  "summary": {"error": 1, "warning": 2, "info": 3, "skipped": 0},
  "findings": [{
    "code": "candidate_pair_snapshot_gap",
    "severity": "error",
    "scope": "candidate",
    "subject": "cand-...",
    "message": "proposal op ... is absent from the producer snapshot",
    "evidence": {},
    "remediation": {
      "responsible_surface": "store_sync",
      "text": "synchronize the missing op, then publish one pair observation"
    }
  }]
}
```

`ok` means no finding met `fail_on`; it does not mean the finding list is empty.
Every ambient finding includes an observation timestamp and the exact Git ref
and OID or filesystem evidence used.

## 5. Initial finding catalogue

| Code | Severity | Evidence and meaning | Remediation boundary |
|---|---|---|---|
| `git_store_uncommitted_ops` | warning | A Git-backed op store has indexed, modified, deleted, or untracked differences | Store owner synchronizes ops; audit does not commit |
| `role_coverage_shortfall` | warning | A non-retired role has fewer active session-bounded assignments than its standing or contextual demand | A named assignment authority explicitly assigns or renews a holder; audit never infers one |
| `candidate_pair_snapshot_gap` | error | Landability names a proposal absent from a later producer snapshot, and the target store contains that proposal | Synchronize the proposal op to a producer, then publish one exact pair row |
| `candidate_reachable_unrecorded` | warning | With explicit `--target-ref`, a pending candidate commit is an ancestor of the observed target OID | Use the explicit out-of-band reconciliation operation; do not claim governed landing |
| `candidate_landing_repository_mismatch` | warning | The audited repository differs from the candidate's explicit landing binding | Run against the repository backing the store, or have the authorizer bind the pending legacy row |
| `candidate_object_unreachable` | warning | The exact commit and immutable parent anchors are not readable from the bound landing repository | Transfer or fetch from the recorded object source, then publish availability evidence |
| `candidate_git_unavailable` | warning | Requested ref or object cannot be resolved, or history is shallow | Repair Git visibility; never infer reachability |
| `candidate_blockers` | info | Full structured landability reasons and counts, without truncation | Follow each typed blocker; display order is not policy |
| `candidate_containment_recovery_recorded` | info | A successor authorizer used exact pair evidence to retire a contained predecessor | Review the immutable authority and evidence basis; do not call it governed landing |
| `candidate_out_of_band_landing_recorded` | warning | The proposal authorizer recorded exact target reachability and the pre-transition policy snapshot | Retain as reconciliation evidence; do not call it governed landing |
| `parent_all_children_closed` | info | An open relation parent has at least one child and all live children are closed | Review the parent; never auto-close it |
| `dangling_dependency` | warning | A live bead depends on a missing or deleted bead | Repair the explicit edge or restore the target |
| `dangling_relation` | info | A live hierarchy relation points to a missing or deleted bead | Review the organizational link |
| `orphaned_claim` | warning | A claim is still leased to closed or deleted work | Holder releases it or lets it expire |
| `orphaned_reservation` | warning | A reservation remains path-blocking but its binding is terminal | Authorized actor adopts/releases it or waits for expiry |
| `doing_without_live_claim` | info | A bead says `doing` but has no live claim | Review status and coordination; this is not proof of abandonment |
| `unassigned_with_live_coordination` | info | An unassigned bead has claims or reservations by named actors | Display both facts; never infer assignee from leases |
| `live_but_idle` | info | Only when `--stale-after` is supplied, a TTL-live actor has no op within that duration | Display lease and activity separately; do not alter presence or reviewer eligibility |
| `aged_open_request` | warning | Only when `--stale-after` is supplied, a request remains explicitly open past the threshold | Request owner reviews it; acknowledgement alone is not resolution |

Finding severity is part of schema v1. A later release may add codes but must
not silently strengthen an existing code from advisory to blocking.

## 6. Candidate and Git reconciliation

Candidate audit uses the current candidate v3 landability projection first. A
`git_evidence_stale` reason whose evidence op sorts after the missing proposal
and whose declared snapshot omits that proposal becomes
`candidate_pair_snapshot_gap`. If the corresponding proposal operation file is
also uncommitted in the target Git-backed store, the finding includes the
target's uncommitted count and oldest age and explicitly says the candidate
producer cannot fix the target from its incomplete checkout.

With `--target-ref`, audit first verifies that the current object database is
the candidate's bound landing repository and that the exact commit and parent
anchors are readable. Repository mismatch and missing objects produce the
specific findings above. It then resolves the ref once to a full OID and runs exact
`merge-base --is-ancestor <candidate-oid> <target-oid>` checks for candidates
whose landing repository identity matches the current object database. Result `0`
produces `candidate_reachable_unrecorded` when recorded phase is still pending;
result `1` is a clean not-reachable observation; all other results are
`candidate_git_unavailable`. Patch equality, tree equality, squash equivalence,
and cherry-pick inference remain out of scope.

Observed reachability never skips candidate pair coverage inside the reducer.
`mote candidate reconcile` records a passing exact `git-reachability` receipt
and a compare-and-set snapshot of the pending phase, review-policy and landing-
repository clocks, reviews, evidence, pair-evidence, authorization, and full
landability blockers. Only the proposal's
immutable authorizer may publish it. Acceptance produces the distinct terminal
phase `landed_out_of_band`, leaves authorization unchanged, and emits
`candidate_out_of_band_landing_recorded`. Until that operation is accepted,
candidate phase remains exactly as recorded.

## 7. Display rules

Board and list surfaces may add clearly named facets, but never replace a
recorded field with an inferred one:

- `phase_recorded` beside `git_reachability_observed`;
- `assignee_recorded` beside `claim_holder` and `reservation_holders`;
- `presence_state` beside `last_activity_ts` and `activity_age_s`;
- `status_recorded` beside `child_status_counts`.

Human output must not label a live-but-idle session `stale`, a reservation
holder `owner`, or an externally reachable candidate `landed`. JSON retains the
source and `as_of_ts` for every observed facet.

## 8. Acceptance matrix

Implementation is complete only when tests cover:

1. non-Git, wholly local-untracked, clean Git-backed, dirty Git-backed, ignored
   new ops, modified ops, staged ops, deleted ops, malformed filenames, and a
   detached Git HEAD;
2. exact count, oldest timestamp, age boundary, stable JSON, human remedy, and
   unchanged doctor exit semantics;
3. a candidate receipt that predates a proposal versus a later receipt whose
   declared snapshot omits it;
4. a target store whose missing proposal is uncommitted, including the explicit
   statement that repeating work in the producer checkout cannot repair it;
5. explicit target reachability for ancestor, non-ancestor, missing object,
   shallow history, repository mismatch, and no-target skip;
6. open parent with zero children, mixed children, all closed children, and an
   intentionally open release gate with no mutation;
7. dangling dependencies and relations, orphaned leases, doing without a live
   claim, and unassigned work with reservations without ownership inference;
8. TTL-live but idle versus expired but recently active actors, using an
   injected `as_of_ts` and explicit threshold;
9. deterministic ordering, `--fail-on` thresholds, no operation publication,
   and byte-identical store contents before and after both commands.

## 9. Non-goals

- repairing, committing, pushing, or synchronizing a Git-backed store;
- inferring assignee, reviewer, authorizer, grantee, or role from activity;
- closing parents because children are closed;
- changing session presence from last-activity time;
- treating ambient reachability as governed landing;
- natural-language inference from notes, reviews, or discussion;
- making reducer replay depend on Git, the filesystem, network, or wall clock.
