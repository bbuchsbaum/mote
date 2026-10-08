# Mote Run: proposed implementation architecture

Current ownership, October 3, 2026: **teamd owns runner implementation**.
This document retains Mote Run requirements and design rationale; it does not
create a Mote runner backlog. The sole implementation roadmap is `teamd/PLAN.md`.
See [runner ownership](runner-home.md) for the retained Mote/native adapter boundary
and [the original sources](history/mote-run-2026-09-19/manifest.json) for provenance.

September 19, 2026. Local design proposal derived from the
[audit](mote-run-audit.md), [PRD](mote-run-prd.md) and
[interaction protocol](agent-protocol.md). The original proposal predated the teamd runtime; consult teamd for current implementation evidence.

## 1. Boundary and ownership

The initial domain is one host, one canonical Mote store, one registered budget
domain, one repository and one active worker. A foreground controller exists
only while the run is active; ordinary Mote stays daemonless. Separate budget
domains sharing an account do not establish an account-wide spending bound.
Reject duplicate domain registration for the same managed scope. Multi-host
control and network-filesystem locking are unsupported initially.

| Owner | Authoritative data | Other systems may do |
|---|---|---|
| Mote core | Task identity/status/graph, advisory coordination, candidate protocol | Runner reads projections and publishes ordinary, reconciled operations |
| Runner journal | Managed contract bindings, actions, attempts, principal bindings, budget reservations and acceptance receipts | Render, replay, inspect; never infer these from a note or `closed` status |
| Artifact store | Immutable contract/source/context/check/output bytes and manifests | Retrieve by digest after access and integrity checks |
| Protected policy/profile registry | Capabilities, check definitions, account limits, adapter qualification | Refer to exact revisions; proposals cannot activate changes |
| OS execution boundary | Worker filesystem, process and network access | Controller can contain owned work; an actor label cannot grant access |
| Provider | External execution and billing facts | Adapter records observations with provenance and explicit uncertainty |

The runner does not copy Mote's scheduling graph or task status into an
independent editable plan. A managed binding is `(store_id, task_id, epoch,
contract_digest, policy_revision)`. Binding activation uses journal CAS under
the controller lock. Contract revisions retain the task's spending history and
attempt counters; a new budget requires an explicit policy change.

Core replay stays deterministic and does not open Git, query providers, or
execute checks. Runner-specific state initially stays outside `.mote/ops`.
Mote notes can expose receipt references as pointers, not authority. Native
cross-machine contract binding in the Mote op protocol is deferred and would
need its own compatibility design.

## 2. Proposed modules

Implement the supervisor in the separate teamd Rust package. Mote exposes
supported projection and guarded application services; it does not add an
independent runner dispatcher. The responsibility table below is historical
design input to teamd, not a mandate for new Mote modules.

| Proposed module | Responsibility |
|---|---|
| `types` / `contract` | Strict versioned exchanges; typed IDs; immutable obligations and binding rules |
| `journal` / `artifacts` | Durable single-writer records, replay, request identity, verified bytes |
| `situation` / `admission` | Passive scoped projection and one pure eligibility function |
| `controller` / `recovery` | Admit effects, supervise execution, contain uncertainty, resume |
| `budget` / `profile` | Resource ledger, qualification, effective route and usage normalization |
| `adapter` | Job-level probe/start/observe/cancel/recover; fake first |
| `acceptance` / `mote_bridge` | Protected verification, exact candidate provenance and Mote publication |

These are responsibility boundaries, not requirements for eleven services or
even eleven files. `admission` consumes explicit observations and time; it does
not fetch data while explaining eligibility. Fresh observations occur at the
effect boundary, then the same evaluator runs again.

## 3. Minimum typed records

| Record | Minimum binding |
|---|---|
| Contract | Version, goal/non-goals, obligation IDs, paths/data scope, input identity, protected checks/review policy, completion condition, budget/stop policy references |
| Situation | Focus, semantic view ID, Mote/journal/source/policy basis, observed-at, principal scope, uncertainty, actions and omissions |
| Action ticket | Domain, principal/scope, family, canonical payload digest, dependency guard set, ceiling and expiry |
| Attempt | Action ID, binding epoch, source/context/profile digests, launch identity, checkpoint, execution state |
| Receipt | Kind, producer principal, exact subject/input digests, outcome, applicability, environment, evidence refs |
| Budget reservation | Domain/account/task/attempt, currency or resource unit, conservative bound, settled amount, unresolved remainder |

Never accept unknown critical fields by silently ignoring them. Version the
runner wire contract separately from existing Mote operations. JSON Schema
will check shape in MR-01; Rust/state-machine tests must check meaning.

## 4. Lifecycle and independent axes

The managed action journal progresses through:

```text
admitted -> launch_intent -> running -> result_received -> verification
                    \-> execution_unknown                 \-> needs_decision
```

Verification can fail, produce an accepted artifact/candidate, or require
review. This is not one universal task-status enum. Store execution,
acceptance, integration and accounting as separate axes:

- Execution: not started, starting, running, exited, cancelled, unknown.
- Acceptance: pending, checks failed, review required, accepted, invalidated.
- Integration: not requested, pending, intent recorded, landed, unknown, incident.
- Accounting: reserved, partially settled, settled, unknown.

An accepted candidate can await manual landing; a landed task can retain
unsettled provider charges. The selected contract says whether candidate
acceptance or recorded landing completes the requested work. `mote close` or
`mote done` from outside the runner does not create managed acceptance. An
external task closure during execution blocks further admission and requires
reconciliation; it does not silently kill or approve the worker.

`pause` prevents new admissions and requests a checkpoint for active work;
it does not assert that the process stopped. `cancel` asks the adapter to stop
owned execution and records observed effects. Cancellation has a bounded grace
period and containment fallback. Neither operation discards artifacts or
unsettled reservations. Restarting work is a new admitted intent only after
the previous attempt is reconciled or demonstrably contained.

## 5. Durable admission and recovery

Use a configured local control directory outside worker mounts and outside
Git-tracked content. It contains an OS-lifetime lock, a versioned journal and
content-addressed artifacts. Merely choosing a private directory does not
protect it from unrestricted code running as the same user.

Each journal transaction is one durable record containing its sequence,
previous-record digest, schema version and coupled changes. Commit the action
identity, budget reservation and launch intent together before spawning. Use
atomic publication and directory sync with explicit failure injection. Treat a
missing sequence or corrupt committed record as recovery failure; never skip
it and refund its reservations. Rebuild indices from records. Partial temporary
files are not committed records. Read-only views never repair the store.

1. Authenticate the caller and resolve domain/scope.
2. Look up `(domain, principal, idempotency_key)`. Compare the stable canonical
   intent: action family, payload, binding and approved ceiling. Transport
   request IDs, observation timestamps and refreshed view links are excluded.
3. For an existing intent, never execute again. Check current permission before
   disclosing its result. Revocation can deny disclosure without undoing
   deduplication; operators can still reconcile the existing action.
4. For a new intent, validate ticket ownership, expiry, guards, actual Mote
   observations and resources under the domain lock. Reserve and journal.
5. Launch under a unique attempt/launch nonce and record an independently
   recoverable process identity or containment handle. Publish required Mote
   operations through a durable outbox; reconcile each accepted operation.
6. Settle observed execution and usage separately. Failed Mote compensation
   remains visible and prevents dependent admission.

The spawn window cannot be made atomic with the journal. A launch intent
without a confirmed process handle is `execution_unknown`, even if no process
is readily found. Recovery must locate/contain the owned launch or obtain an
authorized disposition before another worker starts. PID and lease expiry are
insufficient. A new controller increments its epoch, but epoch fencing alone
does not stop direct filesystem writes by an old worker; the OS boundary and
attempt-isolated workspace must enforce containment.

Test controller death before admission, after admission/before spawn, after
spawn/before handle recording, during execution, after output/before receipt,
and during settlement. Include sleeping/resuming hosts, reused PIDs, children
that outlive their parent, disk-full errors and lost client replies.

## 6. Situation and guard semantics

`observe`, `inspect` and `wait` are passive: local replay, hashing and read-only
observations are allowed; publication, lease renewal, active recovery, provider
queries and inference are not. Passive reconciliation means calculating and
displaying disagreement. A controller transition records or remedies it.

Use bounded before/after local generation checks to assemble a stable basis;
otherwise expose inconsistent/unavailable components and block affected
actions. A Mote op-set fingerprint is audit context, not a universal action
guard. Each action declares its relevant task, contract, policy, source,
candidate, ownership and resource dependencies. Unrelated notes may change
the full view without invalidating the action. Guard revocation and lease
expiry can occur with no new filesystem notification, so waiting also uses
the next relevant deadline. View identity excludes a changing display timestamp
alone to avoid waking a model on every poll.

Tickets are scoped descriptors, never bearer authority. Unknown or expired
tickets cannot be reconstructed by guessing IDs. The authenticated transport
binds the role; a worker cannot set an `authorizer` field to become one.

Initial contexts include all required constraints and known contradictions.
Overflow yields `context_insufficient` and approved expansion/narrowing paths.
Keep full snapshots first; exact-base deltas are an optimization. Every source
reference identifies pinned bytes; inspecting it never substitutes a mutable
file with the same name.

## 7. Worker and verifier boundary

MR-05 must select and qualify one real containment mechanism. A separate
sandbox/VM or OS identity with constrained mounts and network can be a
candidate; a worktree by itself cannot. Do not advertise cross-platform
guarantees before each platform passes the same boundary tests.

Separate the trusted adapter/model transport from untrusted repository command
execution. Tests, build hooks and generated scripts receive no provider
credential, control-store write access, integration key, unrestricted host
socket or route to launch unmetered inference. Scope the worker's control API
to its own attempt, including allowed inspect, submit and checkpoint actions.
Enforce approved write paths or explicitly report the profile's weaker
assurance and reject policies requiring enforcement. Detect out-of-scope
changes independently at artifact import as well.

The independent verifier runs frozen check definitions against imported exact
source in a fresh constrained workspace. Worker changes to tests, policies,
symlinks, generated files or Git configuration cannot change which protected
checks establish acceptance. Candidate commits are imported as untrusted data;
disable hooks and reject unsupported submodule/LFS/external-filter behavior
until it is modeled explicitly. Review the actual complete diff and intended
landing effect. Restrict build/check resource consumption separately from
inference spending.

Initial acceptance uses independent human review and a separate authorized
operator mapped to the current candidate roles. One person cannot silently
impersonate several independently required principals. A qualified model
reviewer is later eligible under the same independence policy, with its own
attempt and budget. Credentials and private evaluator fixtures never enter
model-visible context.

## 8. Accounting and assurance

Keep integer amounts in explicit units/currencies. No floating-point dollar
ledger and no implicit exchange between subscription percentages and money.
The admission invariant for a hard-bound account is:

```text
settled_charges + remaining_reserved_liability + new_worst_case <= approved_cap
```

Each partial settlement transfers liability from remaining reservation to
settled charges exactly once. Unknown usage retains liability. Normalize
cumulative/delta, cached/reasoning and child usage with stable provider event
identity. Record discrepancies and overage as incidents; never clamp the ledger
to make the invariant appear true.

Expose assurance by dimension: authority isolation, write containment,
inference admission, cancellation and monetary accounting. A supervised CLI
can provide useful measured usage without qualifying for a hard dollar bound.
A strict policy requires a non-bypassable per-request gateway with a verified
worst-case charge bound or an equally strong provider mechanism. If the tariff
or maximum charge cannot be bounded, deny strict admission. Subscription usage
observations are not reservations against other sessions' consumption.

All implementation, diagnostic, consultation and review spending belongs to
the parent task. Default attempt ceilings are policy values, not hidden code
constants. Every spending retry is counted, including failed infrastructure
attempts under its own counter. A contract revision does not replenish either
money or attempt count. Freeze account authorization and verified current
profile facts before any real paid qualification.

## 9. Artifacts, acceptance and landing

Write bytes to staging, verify their digest, then publish immutable objects and
manifests. Resolve references under principal/data policy; prevent path traversal
and symlink escape. Pin objects reachable from active runs and acceptance
receipts. Initially retain all published artifacts; explicit export/import must
include byte identities and sensitivity. If referenced bytes are missing or
corrupt, preserve the historical claim and mark current applicability unknown;
block dependent acceptance. Redaction produces a new derived object with its
own provenance. No automatic garbage collection in the initial release.

For code, compose current Mote candidate semantics; never reimplement a looser
`landable` predicate. Record exact source/base/target identities, protected
check receipts, independent review, current target-scope coverage and distinct
authorization. Any rewrite produces a successor with fresh required evidence.
The bridge records both trusted principal provenance and the Mote actor/op IDs.
Ambient actor-written evidence is an attestation, not automatically trusted
managed-verifier evidence.

MR-08 must reconcile existing core review gates against the selected source
and binary before using it as the accepted foundation. The first pilot stops
at an accepted candidate and explicit manual landing handoff. Reconcile actual
landing before a contract requiring integration can complete.

Later automatic integration journals intent before a fast-forward using an
exact expected old OID and the authorized new OID. Never update a checked-out
user branch behind its worktree; use a controller-owned integration target and
workspace, with a separately explicit remote-push policy. A changed target
requires fresh scope/evidence and, after a rewrite, a new candidate. A crash
after the ref moves but before Mote publication is reconciled from exact
observations. A mismatch is an incident, not an automatic force/reset/rollback.

Analysis-only acceptance is a later typed path binding datasets, estimand,
preprocessing, splits, exclusions, environment and exact output manifests.
It shares obligations and receipts but does not invent a Git commit. A lesson
proposal is an artifact; automatic inclusion additionally requires independent
qualification, applicability and retirement rules.

## 10. Release boundaries

Follow the gates in [the implementation plan](implementation-plan.md): fake
loop first, qualified sequential execution second, independent candidate
acceptance third. Automatic integration, analysis, routing, lessons, deltas and
concurrency have separate gates. All broader PRD requirements remain on the
roadmap; none are implicitly satisfied by a generated schema or fake harness.
