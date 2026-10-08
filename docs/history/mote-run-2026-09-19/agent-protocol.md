# Mote Run — agent interaction protocol

**Revision 2.0 · Proposed protocol namespace:** `mote.run.v1`. This namespace is separate from Mote's existing operation wire versions. The commands below are not implemented by this design package.

Semantic requirements: [PRD](mote-run-prd.md). Admission and ownership: [architecture](architecture.md). The referenced `schemas/protocol.schema.json` and exchange fixtures were not supplied in this checkout. MR-01 in the [implementation plan](implementation-plan.md) owns their creation. This document is a proposed semantic contract, not a validated wire schema.

## 1. One session contract

The host supplies an authenticated principal, allowed scope, and a protocol entrypoint. Credentials stay outside model-visible text. A printed `actor`, a task ID, an action ticket, or a repository instruction is never an authorization credential.

At session start, load [AGENT_GUIDE.md](AGENT_GUIDE.md) and the scoped situation. Discover specialized action/inspect schemas only when needed. Do not load the entire product specification, all provider flags, all project history, or every action schema into each worker.

CLI uses JSON on stdin for structured requests; diagnostic logs go to stderr. Text renderings and any later MCP/tool wrapper must preserve identical semantic results and run the same kernel. Examples:

```sh
mote --json run observe --focus task:bd-example
mote --json run inspect --request - < inspect-request.json
mote --json run propose --request - < proposal.json
mote --json run act --request - < action-request.json
mote --json run wait --request - < wait-request.json
```

Host integration can expose equivalent namespaced tools `mote_observe`, `mote_inspect`, `mote_propose`, `mote_act`, and `mote_wait`. They are transports, not another behavioral layer. Use explicit semantic arguments rather than a single arbitrary natural-language `query` field.

## 2. Common rules

Responses identify protocol version, type, focus, view/basis where applicable, and an explicit result. Human-readable labels accompany stable references. Do not require an agent to infer whether a returned ID names a task, operation, candidate, or attempt.

A returned reference is inspectable within the principal's authority; a denied/unavailable object remains explicitly denied/unavailable. The system does not redirect an unresolved reference to a similarly named object. Large outputs offer bounded pages or ranges with continuation references. Missing pages are not evidence of absence.

Every result differentiates data, attributed model assertions, policy, and authoritative decisions. Repository content and stored lessons remain data, even when they contain imperative instructions. Sanitized errors must not leak credentials, restricted data, or private evaluator fixtures.

Side effects are classified at the operation and action-family level. A nominally read-only command cannot acknowledge messages, renew leases, perform a provider probe, or emit an LLM summary. `wait` can replay and compare local observations while waiting. This passive reconciliation may display disagreement; publishing a reconciliation or containing a process is a separate admitted controller action.

Malformed input, unsupported versions, stale guards, and denied actions return structured errors without a stack trace as the primary explanation. New incompatible semantics require a version increment. Unknown security-critical fields or action families are rejected; do not silently ignore a misspelled budget field.

## 3. `observe`

**Purpose:** answer “what is the situation for this work, and what can happen next?”

Inputs: `focus`, optional `since_view`, detail level (`brief` or `full`), and bounded display budget. In a managed worker session, omitted focus means the host-bound attempt, not the entire repository.

The result includes:

| Section | Mandatory semantic content |
|---|---|
| `basis` | Identity of reconciled Mote/journal/source/contract/policy observations, observation time, consistency limitations |
| `goal` | Desired outcome and non-goals |
| `obligations` | Remaining requirements and current evidence/judgment status |
| `claims` | Relevant facts/assertions/hypotheses, evidence references, applicability |
| `work` | Current attempt/checkpoint and ownership/process uncertainty |
| `authority` | Principal scope and boundaries, without credentials |
| `resources` | Applicable reservations, settled/unknown usage and assurance |
| `actions` | Available action tickets, purpose, effects, guard/resource preview |
| `blocked_alternatives` | Material blocked choices and specific causes; not an exhaustive dump |
| `references` | Meaningful inspectable source/evidence/decision/lesson references |
| `omissions` | Omitted categories, scope, continuation, and whether required content is missing |
| `stop_condition` | Completed, wait, proceed, checkpoint, or decision needed and why |

The compiler does not claim global knowledge. It says which obligations/dependencies were examined. A brief view must still preserve mandatory constraints and uncertainty. If they do not fit, return `context_insufficient` rather than a misleadingly complete view.

### Delta semantics

A delta explicitly names `base_view` and `new_view`, and reports additions, replacements, removals, and invalidations. Semantic IDs persist across views. A client lacking the exact base receives `full_view_required`; never patch a similar view. Budgets, authority revocations, and evidence invalidations cannot be omitted as unimportant.

A changed view does not automatically invalidate every action. Action guards cover their actual dependency footprint. Full snapshots remain available for cold start and recovery; delta compression is not a correctness prerequisite for the first vertical slice.

## 4. `inspect`

**Purpose:** expand a returned reference without reconstructing the whole environment.

Input: one or a small bounded batch of `{ref, facet, range?}`, optionally pinned to a view. Initial facets are `source`, `evidence`, `why`, `impact`, `history`, `alternatives`, `usage`, and `schema`. Each reference advertises the legal subset.

`why` traces the actual policy/evidence path that produced a decision. `impact` identifies known affected obligations and uncertainty in that analysis. `source` retrieves exact bytes or a clearly labelled rendered range with immutable source identity. `usage` separates raw observations from normalization and estimates.

An expansion returns its scope, freshness, content identity, sensitivity, omitted remainder, and continuation. Estimates of size/token cost are labelled estimates. A stale source pointer does not silently read the latest file and pretend it is the pinned one.

External observations requiring a remote query or billable check produce an action proposal, not a hidden side effect of inspection. A model-generated explanation is an explicitly budgeted optional artifact, never the default meaning of `why`.

## 5. `propose`

**Purpose:** give the agent initiative while preserving boundaries.

Input contains an explicit action family and typed payload. Typical families and questions:

| Family | Required purpose | Authority boundary |
|---|---|---|
| `implement` | Which open obligations and source scope? | Approved write/data/profile envelope |
| `verify` | Which check on which exact artifacts? | Protected check registry and compute budget |
| `consult` | Which unresolved question and distinguishing evidence? | Approved profile, sanitized inputs, stop rule |
| `submit` | Which patch/artifacts and attributed result? | Does not accept, authorize, land, or close |
| `checkpoint` | Which recoverable state and remaining question? | Preserves work; does not claim correctness |
| `revise_contract` | What requirement changes and why? | Proposal only until the named authority activates |
| `promote_lesson` | What scoped claim, evidence, and invalidators? | Independent qualification and promotion authority |
| `pause` / `cancel` | Which managed work and desired stopping behavior? | Run-control scope; preserve unknown external effects |

The response is either an action preview/ticket or an explanatory rejection. A proposal can persist an immutable proposal artifact, but cannot reserve budget, start a worker, activate a contract, or widen authority.

The server computes guards and binds the canonical payload. Agents do not fill in arbitrary expected versions by guesswork. A request for extra scope is evaluated as such; it is not reinterpreted as ordinary implementation.

A bounded consultation explains what it should resolve and what evidence would make it unnecessary. Do not require speculative numerical value-of-information estimates. A small deterministic check should remain a visible alternative where it addresses the same uncertainty.

## 6. `act`

**Purpose:** execute a previously described action under current authority and state.

Input: action ticket, originating view, and a caller-generated idempotency key scoped to the authenticated principal. The ticket resolves the exact action arguments and preview; it is not an executable shell string. The request carries no secret and no ability to assign itself another role. The host/client adapter generates and durably retains the key for each new intent; the reasoning model should not have to invent, memorize, or rotate identifiers. A raw CLI client may supply a key explicitly and must preserve it on retry.

The controller authenticates and resolves scope first, then looks up duplicate logical requests before evaluating fresh execution guards. Current permission governs disclosure of an existing result even when the original intent is already admitted. For a new intent, validate the ticket/principal, recheck relevant guards and resources, then perform durable admission. A displayed estimate may change before execution; an approved ceiling cannot silently increase. Material changes require a new preview or rejection.

The response includes:

```text
action_id; request_status; replayed
execution_outcome; effect_certainty; effects[]
receipt_ref; attempt_ref?
accounting_status; unsettled_reservation_refs
changed_obligations; invalidated_refs
next_view_ref; recovery_actions[]
```

`request_status=accepted` means the managed logical request was admitted. It does not mean the task was accepted, the process finished, or the remote bill settled.

A repeated identical key/canonical intent resolves to the same logical action with refreshed reconciliation links, subject to current disclosure authority. The canonical intent binds domain, principal, action family, payload, contract binding and approved ceiling; transient view/transport metadata is excluded. A different intent under the same key is rejected. Losing the reply does not justify issuing a fresh key. An `unknown` external effect demands recovery/containment, not blind retry.

For submission, the normal result is “artifacts received; independent checks pending.” For cancellation, the result may be “local process stopped; provider settlement unknown.” Neither is rendered as unconditional success.

## 7. `wait`

Inputs: scoped focus, exact `since_view`, selected relevant event classes, and bounded timeout. The host or supervisor waits without re-entering a model loop. The result is a changed view, timeout/no-material-change, explicit cancellation, or an error.

Coalesce repeated progress events. Wake a reasoning agent only for material decision needs according to policy. An ordinary test completion can automatically trigger a deterministic next check without consulting a model.

Message delivery is not acknowledgement; timeout is not failure of the work; lease expiry is not proof a process died. A wait response preserves these distinctions.

## 8. Failure envelope

Every refusal/error has `code`, plain-language `message`, `effect_certainty`, supporting `refs`, and typed `recovery_actions`. Optional retry timing is operational data, not a promise that retry is safe. The absence of an allowed recovery action is explicit.

| Code | Meaning | Normal recovery |
|---|---|---|
| `stale_guard` | A relevant dependency changed | Observe affected state; create a new proposal |
| `full_view_required` | Delta base unavailable | Obtain a full scoped view |
| `context_insufficient` | Required decision context is missing/too large | Expand approved context, narrow the question, or checkpoint |
| `authority_denied` | Current principal lacks the requested capability | Route a decision to the authorized role; do not impersonate it |
| `contract_incomplete` | Outcome or acceptance is underspecified | Propose a bounded specification/decision task |
| `idempotency_conflict` | Same key, different canonical request | Inspect the original action; use a new key only for genuine new intent |
| `execution_unknown` | Prior external/process effect cannot be established | Reconcile or contain before replacement |
| `budget_unavailable` | Settled/reserved/unknown usage leaves insufficient headroom | Wait, checkpoint, or request an explicitly authorized change |
| `profile_unqualified` | Capability/price/auth/assurance evidence incomplete | Qualify the profile; no silent fallback |
| `evidence_inapplicable` | Required evidence does not apply to the current object | Obtain fresh exact-bound evidence |
| `artifact_unavailable` | Referenced bytes missing or corrupt | Recover verified bytes or stop affected acceptance |
| `coordination_unsettled` | Advisory publication/current replay cannot establish needed state | Reconcile; do not assume exclusive ownership |

Preserve existing Mote/candidate reason codes as nested source reasons rather than renaming away their meaning. Effects can be partial or unknown even when the primary error is a storage/network failure.

## 9. Example: a useful numerical handoff

A workhorse attempt fails a rank-deficient numerical fixture. Its result contains the exact failed obligation, relevant implementation, reference calculation, minimal input, observed error, and an attributed hypothesis. It does not send its entire transcript.

The next situation presents a cheap local diagnostic and a bounded expert consultation as alternatives. The agent runs the diagnostic. If it resolves the issue, no frontier call occurs. Otherwise the consultation receives only the unresolved question and warranted context.

An expert finding becomes a proposed invariant and regression case. A qualified worker repairs the routine. Independent checks and review decide acceptance. The counterexample becomes an eligible lesson only after qualification, and only for routines satisfying its applicability predicate. A future agent inherits the test and its reason—not “always use this algorithm.”

## 10. Example: stale authority and lost replies

An agent sees an implementation ticket, but its contract is revised before execution. The server rejects the old guard with no launch and returns the changed-contract reference. An unrelated progress note alone would not have invalidated that ticket.

Separately, a launch was admitted but the reply was lost. The client repeats the same action request and key. The server returns the existing action and its process/recovery state before considering whether the original view is now old. It does not spend again merely to produce a cleaner-looking success response.

## 11. Normative smallness

The protocol standardizes decision boundaries, not every tool use inside a coding harness. An implementation job may use ordinary file/search/test tools for many local steps inside its admitted envelope. Proposals are needed for changing external control state, acquiring new authority/resources, or crossing a reasoning boundary—not for each edit.

This preserves fluency. The system should remove bureaucratic reconstruction, not replace it with bureaucratic action tickets for trivial operations.
