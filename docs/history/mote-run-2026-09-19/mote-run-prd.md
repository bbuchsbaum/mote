# Mote Run — agent-centered product requirements

**Revision:** 2.0 · **Date:** September 18, 2026 · **Status:** proposed; not implemented by this document.

This revision proposes an optional runner. It does not replace Mote's existing storage, coordination, or candidate protocols. The imported document names historical repository baseline `bbuchsbaum/mote@a4992aa9d6c3e9f3a3b9031a52118536d73672f4`; that historical claim was not re-certified here. The referenced original `baseline/mote-run-prd.v1.md` was not supplied in this checkout and its preservation cannot be verified.

**Local audit, September 19, 2026:** [audit and workshop](mote-run-audit.md) records the current source baseline, gaps and recommendations. The architecture and implementation plan now describe a proposed sequential first release and gated extensions; they are newly prepared planning artifacts, not the missing original package. See [validation](VALIDATION.md) for what is actually checked locally.

**Read this first.** [Architecture](architecture.md) defines the semantic model and invariants; [agent protocol](agent-protocol.md) defines the interaction contract; [implementation plan](implementation-plan.md) and [plan.json](plan.json) define delivery; [decisions and migration](decisions-and-migration.md) explain what changes and what remains. [AGENT_GUIDE.md](AGENT_GUIDE.md) is the small runtime bootstrap, not a replacement for the specification.

## 1. The product, reconsidered

Mote Run is a **budgeted, evidence-driven work system that makes the next justified action legible**. It is not primarily a collection of agents, a provider router, or an automated project manager.

Its central loop is:

```text
Observe a bounded situation
    → choose an action justified by the remaining obligations
    → execute within explicit authority and resources
    → check what actually changed
    → retain reusable evidence and continue, stop, or escalate.
```

The durable unit of work remains **a bounded attempt against a frozen contract**. The unit of interaction becomes **a decision supported by a current situation**. These are complementary: the situation tells an agent why an attempt is appropriate; the attempt makes the resulting work accountable.

From an agent's seat, an excellent system answers, without transcript archaeology:

> What am I trying to establish? What do we actually know? What is still unknown? What can I do? What would that action consume and change? What result would permit us to move on?

The system should make an inexpensive capable model effective by removing avoidable reconstruction, ambiguity, coordination, and recovery work. It must not assume that a smaller prompt, more delegation, or a stronger model automatically improves accepted-task economics.

### 1.1 Three meanings of agent-centered

**Agent-intuitive:** concepts have one meaning across tools, documentation, reports, and enforcement. An action named `submit` submits an artifact; it never silently approves, lands, or closes the task. Empty, unknown, unavailable, and not applicable are distinguishable.

**Agent-ergonomic:** the agent obtains the relevant situation in one operation, follows short meaningful references for detail, and receives actionable results. It need not remember many commands, manufacture IDs or shell quoting, correlate unrelated status displays, or keep a long conversation alive to preserve control.

**Agent-accretive:** completed work increases reusable, warranted capability. A later agent can inherit a validated counterexample, invariant, procedure, or architectural decision without inheriting the earlier transcript. Accumulation includes retirement and correction; more stored text is not inherently more knowledge.

These are testable product properties, not aesthetic descriptions.

### 1.2 What optimization means

Correctness policy, privacy, authority, and approved budgets are constraints. Within those constraints, minimize the **total resources needed for independently accepted outcomes**: subscriptions, API money, repeated context, compute, wall time, failures, review, integration repair, and human intervention.

There is no default exchange rate between provider quota percentages, dollars, and human minutes. Report them separately. A user-approved policy can express preferences, but the router must not quietly trade away the quality floor for cheaper execution.

## 2. One tower of linked abstractions

The tower is a set of refinement relationships, not six services or a generic graph engine.

| Level | Question | Representation | What connects it downward |
|---|---|---|---|
| Intent | Why are we doing this? | Existing Mote task, decision, parent relation | Contract binds the intended outcome and exclusions |
| Obligations | What must be established? | Stable requirement IDs inside a frozen contract | Each obligation declares admissible checks or authorized judgment |
| Situation | What matters now? | Deterministically compiled, scoped view | References exact contract, observations, evidence, policy, and unresolved dependencies |
| Action | What may happen next? | Named typed action with guards and resource envelope | Admission evaluator checks the relevant state and actual principal |
| Attempt | What bounded work executes? | Existing harness job, verifier job, or controller transition | Pinned inputs, execution authority, stop rules, process and budget journal |
| Evidence | What actually happened? | Artifacts and receipts with provenance and scope | Supports, refutes, or leaves an obligation unresolved; may support a reusable lesson |

Evidence feeds the next situation. A promoted lesson can inform a later contract or context package, but cannot secretly rewrite either. Every useful object has links **up to its purpose and down to its evidence or implementation**.

The common vocabulary is deliberately small: **task, contract, obligation, situation, action, attempt, receipt, lesson**. A candidate is the existing Mote exact-commit proposal. A profile is an execution configuration. These do not become alternative kinds of task.

Use typed records and references. Do not introduce a graph database, arbitrary graph query language, ontology framework, or second dependency scheduler.

## 3. Product laws

### 3.1 One meaning, one owner

Mote owns durable coordination and candidate state. The runner owns managed execution and accounting. The contract owns success criteria. The effective policy owns permission and resource ceilings. A situation is a projection, never a competing source of truth.

### 3.2 One decision function

The same admission evaluator supplies available actions, dry-run explanations, and execution checks. There must not be separate notions of “allowed” in the CLI, scheduler, and authorizer. Execution re-evaluates against fresh relevant observations; a displayed option is not a promise that later execution will be admitted.

### 3.3 No stronger conclusion than the evidence

A worker's assertion is an assertion. A passed test is evidence for that check and input set. A candidate authorization is permission, not proof of general correctness. A successful CLI write is not proof that an advisory reservation will remain accepted after additional concurrent operations appear.

### 3.4 No hidden information loss

Every bounded response declares its scope and omissions. Required constraints, safety boundaries, blocking uncertainty, and acceptance criteria may not be silently dropped to hit a token target. If the required decision context will not fit, return `context_insufficient` with lawful alternatives.

### 3.5 Safe repetition and honest uncertainty

Identical managed requests reuse their original logical action, rather than starting another job. Unknown external effects remain unknown. Idempotent local admission cannot establish exactly-once provider inference or atomicity across Git and Mote.

### 3.6 Progress changes an obligation or preserves useful work

Producing a long report, sending a message, or exhausting a budget is not progress by itself. Useful progress includes a verified patch, a narrowed uncertainty, a reliable counterexample, an explicit decision need, or a recoverable checkpoint. Do not measure progress as completed-agent count.

### 3.7 Complexity stays below the agent's decision boundary

The agent decides what needs reasoning. Ordinary software handles leases, scheduling, artifact storage, accounting, notifications, and waiting. Routine file edits and local tests remain native to the coding harness; this protocol does not wrap every keystroke or tool call in a new approval ritual.

### 3.8 Accretion is reversible

History is append-only. Current belief and lesson eligibility are not monotone: new evidence can contradict or invalidate them. Retraction leaves an audit trail and prevents future injection of the invalidated lesson.

## 4. The agent's working surface

### 4.1 One entrance

The primary operation is `observe`, scoped to a run, task, or attempt. It returns a **Situation** with a stable version and observation basis:

- goal, active contract, remaining obligations, and non-goals;
- verified observations, attributed assertions, unresolved questions, and freshness;
- active attempt, ownership and process uncertainty, and material change since the prior view;
- authorized operation families, available actions, meaningful blocked alternatives;
- resource state, enforcement assurance, and any missing telemetry;
- compact references for source, evidence, history, and current applicable lessons;
- an explicit stop/wait/decision condition.

A cold-start agent should not need separate board, claims, inbox, quota, candidate, log, and plan reads merely to orient itself. Existing Mote commands remain available to ordinary tracker users and operators.

The situation is **decision-complete relative to its declared scope**, not omniscient. It does not claim to have discovered every relevant fact in a repository. It exposes which dependencies were checked and which remain unknown.

### 4.2 Five operations, one vocabulary

| Operation | Meaning | Side-effect rule |
|---|---|---|
| `observe` | Obtain the current scoped situation, or a delta from an exact known view | No model call, worker launch, message acknowledgement, or external refresh |
| `inspect` | Expand a returned reference by facet, including evidence, source, impact, alternatives, or history | Read-only; external/billable probes require a separate admitted action |
| `propose` | Validate a typed alternative and obtain its preview/ticket | No execution, reservation, authority expansion, or active contract change |
| `act` | Request execution of a returned action ticket, with guards and an idempotency key | Admission and effects are explicit and independently recorded |
| `wait` | Wait boundedly for a relevant state change | No inference, heartbeat publication, or implicit acknowledgement |

Typed action families include implement, verify, consult, review, submit, checkpoint, pause, cancel, revise-contract, and promote-lesson. The exposed subset depends on the authenticated principal. Action schemas are discovered from returned references rather than loading a giant catalog into every prompt.

A worker can inspect, submit, checkpoint, request an approved diagnostic, or propose a consultation. It cannot mint credentials, authorize its own result, modify policy, activate a contract, or broaden its write boundary by naming a privileged action.

`mote run status`, `explain`, `plan`, and human controls remain convenience views over these same operations. CLI, a future tool transport, and human output are renderers of the same semantics, not separate implementations.

### 4.3 High agency without unlimited authority

Do not make the agent follow a single “recommended next step” mechanically. Show admissible alternatives and reasons: implement now, obtain missing context, run a discriminating check, repair the contract, consult a stronger model, checkpoint, or wait.

The agent may propose a different method or profile within its scope. A rejected proposal returns the exact limiting requirement and a lawful recovery route. Policy permits substantial initiative inside approved boundaries; it does not dictate the implementation algorithm.

Do not display invented success probabilities or decorative confidence percentages. Cost and latency estimates identify their measurement basis and uncertainty. A conservative reservation is a capacity commitment, not a forecast of actual spending.

### 4.4 Decision wake-ups, not conversational polling

The supervisor continues mechanically when the next action follows directly from policy and evidence. It invokes an agent only for actual reasoning work or a decision outside the deterministic policy.

Events are relevance-filtered and coalesced. Waiting on tests must not cause an expensive model to repeatedly ask whether tests are done. Reaching a budget reserve produces a resumable checkpoint, not a new frontier explanation loop.

A higher-level agent may supervise a run through the same surface, but the product does not require a permanent agent supervisor. Human and agent control share the same authority checks.

## 5. Situations, evidence, and context

### 5.1 Distinguish truth categories

Every decision-relevant claim is one of: recorded observation, attributed assertion, derived result, hypothesis, or authorized decision. Evidence applicability is separately `current`, `stale`, or `unknown`. An obligation is `open`, `satisfied`, `contradicted`, or `needs_decision`.

Do not mix these axes into a single success flag. A previous test pass remains a valid historical observation even when its applicability to a rewritten commit is stale.

Checks can return pass, fail, unavailable, or ambiguous. A timeout, missing fixture, or incomplete data coverage is never a pass. Existing Mote candidate outcome semantics remain authoritative for candidate acceptance.

### 5.2 A snapshot is a basis, not magical global atomicity

A situation records the Mote operation-set fingerprint, relevant field and candidate clocks, Git/input manifests, policy/contract versions, journal generation, and observation time. External provider readings include their own observation times and provenance.

Compile a stable local basis with bounded retry; report concurrent change or unavailable components rather than fabricating a globally atomic snapshot. Critical actions re-observe and revalidate their dependency set under the managed controller lock. Unrelated notes should not invalidate a harmless context read or force an agent to rebuild its entire view.

Mote's advisory concurrency remains advisory. Strict worker exclusivity comes from the runner's own fenced managed domain and execution isolation, not a claim that all other local processes obey Mote. Detect an observed ownership contradiction and stop affected acceptance; do not silently bless previously produced work.

### 5.3 Context is a dependency-aware working set

Each attempt receives an immutable context manifest. The initial rendering contains the contract, relevant source slices, applicable constraints and decisions, check identities, prior counterexamples, and scoped lessons. Every selection has a reason and a source identity.

References carry labels, kind, digest or revision, sensitivity, freshness, size estimate, and legal inspect facets. The agent copies references returned by the system rather than inventing identifiers or paths. Small batch expansion is permitted to avoid unnecessary round trips.

Pin required constraints and known conflicting evidence. Use remaining space for relevant source and examples. Indicate omitted material and the exact route to obtain it. A target such as 6,000 initial tokens is a tuning default, not a safety limit or proof of sufficiency.

Stable policy/contract material and changing evidence are rendered separately. Delta responses name their exact base; a client without that base requests a full view. Compaction preserves original references and uncertainty, not just a fluent summary.

Native harness sessions can continue for a direct repair. Cross-provider handoffs transfer a context manifest, artifacts, receipts, and a short attributed hypothesis—not proprietary raw transcripts or hidden reasoning. No agent must produce a private chain-of-thought log to use the system.

### 5.4 Reuse exact evidence without laundering it

A cached check key includes the check definition, complete input closure, environment/tool versions, seeds where applicable, relevant policy, and declared nondeterministic dependencies. Unknown dependency closure means no acceptance-grade reuse. For the first release, prefer exact-run reuse and conservative invalidation over clever partial caching.

A check executed on one commit does not automatically satisfy a different candidate. Even when computation is safely cached under exact inputs, a trusted verifier must produce a fresh candidate-bound receipt under an approved equivalence rule. Candidate review and authorization never transfer through rewrite or supersession.

External conditions, mutable services, deadlines, and flaky checks need explicit freshness/re-execution rules. Historical success is evidence to inspect, not a permanent warranty.

## 6. Contracts and obligations

Introduce immutable contracts and compare-and-set managed task bindings; these are proposed runner mechanisms, not existing Mote core features. Include stable obligation IDs, evidence/judgment requirements, known uncertainties, and sensitivity alongside goal/scope/invariants/non-goals/checks. The initial binding lives in the runner journal as specified in the architecture.

An obligation describes an externally meaningful property, not “the implementer believes this is good.” It can require deterministic evidence, specified independent review, or a decision by a named authority. Not every design judgment is reducible to a test, and test coverage must not be overstated.

Plans remain in Mote. Parent/child organization uses nonblocking relations; actual prerequisites use dependencies. The runner must not silently turn decomposition into a duplicate task graph.

Contracts constrain outcomes and boundaries, not every intermediate implementation step. An exploratory task can authorize investigation with a clear question, artifact, budget, and stop condition. A request to revise behavior becomes a proposed new contract; only an authorized activation changes the task binding.

For analysis work, bind the estimand, units, dataset/manifests, exclusions, preprocessing, missingness, split/held-out rules, seeds, output expectations, and numerical/reference tolerances. Protect held-out data and avoid revealing enough evaluator details to contaminate subsequent attempts. Produce sanitized counterexamples where permissible.

## 7. Bounded execution and acceptance

### 7.1 Before spending

Reconcile outstanding attempts and unsettled account reservations. Evaluate readiness, actual authority, path conflicts, contract completeness, context availability, provider permission, profile qualification, and all applicable resource ceilings.

Freeze inputs and effective profile. Durably reserve budget and record launch intent before spawning. Observe actual accepted results of compound Mote operations; compensate partial failures explicitly. A nonzero or ambiguous intermediate outcome is not success-by-assumption.

One managed controller initially holds an OS-lifetime lock for its budget domain. There is one default worker; two are enabled only after reliability qualification. Frontier concurrency never exceeds one. PID alone is not a recovery identity.

### 7.2 During work

The worker receives the source workspace and constrained tools. Ordinary edits, source inspection, and developer tests remain local harness operations. The controller handles process groups, deadlines, lease renewal, accounting, and relevant notifications without inference.

An attempt-wide and task-wide envelope covers all internal calls and permitted child activity. Native delegation and advisor routes are disabled until a profile proves it can account for and constrain them. Model-authenticated access must not expose control-store or integration authority to executed repository code.

Context expansion is permitted within approved data and resource boundaries. A worker reports a precise missing prerequisite instead of inventing it or accepting an irrelevant summary.

### 7.3 Failure is a structured result

Every result identifies the execution outcome, affected obligation, observed failure, last checkpoint, side-effect certainty, evidence references, unsettled usage, and permitted next steps.

Distinguish infrastructure failure, specification ambiguity, implementation failure, stale input, absent authority, resource exhaustion, and unknown external execution. A stronger model is not the default remedy for every failure.

Initial task ceilings remain three implementation attempts, one expert consultation, and one normal review plus one follow-up after an authorized change. These are maximums, not a required procession of agents. All share the parent task envelope. Repeating the same failure without a new hypothesis or changed evidence stops the loop.

A frontier consultation should answer the smallest unresolved question that matters. It may implement the critical part when that is the best route; forcing cheap-model repair after a clear frontier solution would be a false economy.

### 7.4 Finish means independent acceptance

For code, preserve Mote's exact-commit candidate protocol. Rebase before final checks/review; produce a new candidate after a rewrite. Independently inspect the real diff, including untracked/generated/deleted files, symlinks, and policy or test changes. Run protected checks on pinned source, obtain required independent review, refresh ancestry coverage, and authorize under current policy.

A deterministic reviewer may satisfy the independent-review role only for a narrowly preapproved, fully machine-checkable transformation class. Otherwise use an independent qualified model or human reviewer; high-risk work retains stronger review. The implementer never approves itself.

Serialize integration. Fast-forward only the exact authorized commit using expected-old-OID checks. Record intent before the external Git action; record landing evidence and close only after reconciliation. Do not pretend Git mutation and Mote publication are one transaction. Later contradiction or concurrent unmanaged change creates a visible incident, not an automatic rollback that might destroy other work.

For analysis-only work, acceptance binds exact artifacts, inputs, environment, and required independent evidence. No synthetic Git commit is required. The runner records a typed acceptance receipt referenced from the task; worker prose or an ordinary close command cannot manufacture managed acceptance.

A task may be accepted while provider spending is still unsettled. Keep acceptance and accounting settlement separate; do not release the unsettled reservation or erase the uncertainty.

## 8. Agent-accretive knowledge

### 8.1 Retain the useful residue of work

Useful durable knowledge includes a reference implementation, minimal counterexample, validated invariant, architectural decision with rejected alternatives, reliable setup procedure, diagnostic recipe, and a scoped limitation of a model/profile.

The preferred output is often executable: a regression test, data check, or reproducible diagnostic. A paragraph is appropriate for a human design decision, but it must retain authority, assumptions, and source links.

Do not require a summarizer after every task. Capture receipts mechanically; permit a small lesson proposal in the ordinary result. Deduplicate against existing scoped records. Qualification and promotion consume their own visible maintenance budget when inference or compute is needed.

### 8.2 Promotion is separate from proposal

A lesson starts as a proposal. To become eligible for automatic context inclusion it needs:

1. a bounded claim and applicability predicate;
2. source evidence, including failed alternatives when material;
3. an independent check or authorized review appropriate to that claim;
4. invalidators, sensitivity, owner/authority, and a retirement path.

Use `proposed`, `promoted`, and `retired` for lifecycle, with current/stale/unknown applicability computed separately. Repetition, a high model confidence score, or multiple summaries of one source do not constitute independent corroboration.

A promoted lesson is not policy. It cannot change spending limits, provider permissions, success criteria, or tool authority. A procedure with executable behavior is versioned and reviewed like other code before unattended use.

### 8.3 Retrieve selectively and permit disagreement

Match lessons to current contract/source/profile constraints before ranking them. Include a small set with explicit relevance and supporting references. Show contradictions; do not resolve them by choosing the more confidently written text. An agent can inspect why a lesson was included, challenge it, or propose a replacement.

Invalidate on dependency changes, provenance loss, retracted decisions, counterexamples, or relevant provider/profile drift. Time expiry applies where the claim is time-sensitive; it is not a substitute for structural invalidation of code evidence.

Start with file/path/symbol/tags and lexical retrieval over a rebuildable local index. No vector database is required. Any later retrieval model is a replaceable selector; it does not acquire acceptance authority.

### 8.4 Learning must pay for itself

Evaluate whether lessons reduce cold-start context, repeated failures, and accepted-task resource use on held-out related tasks. Measure misleading retrieval and stale-lesson use as defects. Retire low-value or harmful records from automatic injection while preserving audit history.

Separate evaluation data from operational learning. Do not feed held-out answers into lessons or use an evaluator's private fixtures as worker context. Router qualification also needs stratification and held-out checks; raw acceptance counts from differently difficult workloads are not a fair model comparison.

## 9. Model profiles and resource economics

Retain job-level adapters for existing coding harnesses: `probe`, `start`, `observe`, `cancel`, `recover`. The system does not normalize vendor-private reasoning formats or implement a new coding loop.

The routing unit is provider + model + harness/version + effort + tool policy + authentication/billing mode + data permissions + assurance level. Qualification is by task class and relevant environment. Pin and record resolved configuration; an alias alone is not an immutable model version.

Kimi, DeepSeek, and Gemini Flash—including the requested Gemini 3.8 Flash profile—are candidate economical execution profiles, not inherently subordinate roles. Each can be evaluated for implementation, diagnosis, or independent review. The desired result is inexpensive accepted work, not a predetermined vendor hierarchy.

**Deployment facts are not normative product constants.** Exact model IDs, API compatibility, reasoning settings, CLI flags, prices, and quota telemetry belong in a dated, source-linked profile registry and conformance tests. This revision does not re-certify the original document's provider-specific claims or rate example. Unqualified examples are disabled. Enabling a paid profile requires explicit account authorization and verified current tariffs.

Maintain separate actual API spending, subscription observations, and execution metrics. Distinguish settled spending, reservations, and unknown/unsettled charges. Preserve raw observations; normalize cumulative/delta, cache, reasoning, and nested usage without double counting.

A strict monetary guarantee requires a tested non-bypassable admission boundary with conservative per-request charge reservations. Observed CLI cancellation and subscription headroom are weaker controls and must be labelled as such. Missing usage is unknown, not zero. Unrelated sessions can consume allowance outside the runner's control.

A router first eliminates profiles that violate privacy, authority, quality, capability, or assurance requirements. It then chooses among qualified routes using measured total accepted-task economics and explicit policy. Start with a deterministic table, not an LLM router or online bandit.

Keep a protected resource reserve and include human intervention in reporting. Ordinary control operations and idle status must make zero model calls. An optional expert decision has a budget and stopping rule like any other attempt.

## 10. Requirements and traceability

| ID | Requirement | Observable acceptance condition |
|---|---|---|
| R-01 | Preserve Mote's meanings and ownership | Existing tracker behavior and candidate protocol remain usable without runner installation |
| R-02 | Compile one current scoped situation | A new agent can identify goal, remaining obligations, known uncertainty, authority, and next options without unrelated status calls |
| R-03 | Use one admission evaluator | Displayed reasons, dry-run reasons, and execution reasons agree for the same basis/principal/policy |
| R-04 | Preserve epistemic distinctions | Missing, stale, contradictory, or ambiguous evidence never becomes an applicable pass |
| R-05 | Guard actions by relevant dependencies | Relevant changes reject or re-evaluate; unrelated notes do not force a full restart |
| R-06 | Make repetition and partial effects explicit | Lost replies cannot create duplicate managed launches; unknown external effects remain unsettled |
| R-07 | Bound context without hiding requirements | Omitted nonessential material has references; required context overflow blocks rather than silently truncates |
| R-08 | Support native, cross-model recovery | A qualified replacement reconstructs a task from manifests/artifacts without the original transcript |
| R-09 | Enforce actual authority | A worker cannot self-approve, widen policy, alter protected checks, or launch unaccounted inference within the claimed boundary |
| R-10 | Preserve and account for all resources | Retries/advice/review share parent budgets; missing usage retains a conservative reservation |
| R-11 | Retain durable lifecycle recovery | No replacement launches until prior process/launch uncertainty has been reconciled or explicitly contained |
| R-12 | Independently accept exact artifacts | Rewrites invalidate candidate-bound approval; analysis artifacts receive independent acceptance without fake commits |
| R-13 | Classify failure and offer lawful recovery | A failed action returns stable cause, effect certainty, evidence, and permitted follow-ups |
| R-14 | Make coordination inference-free | Idle polling, routine renewal, status, and normal scheduling produce zero inference requests |
| R-15 | Permit bounded initiative | A worker can propose an alternate method, context need, or diagnostic without changing the active contract |
| R-16 | Accumulate qualified, scoped knowledge | Only promoted and applicable lessons are auto-included; invalidation and retirement prevent stale reuse |
| R-17 | Make evidence reuse dependency-correct | Missing dependency closure or changed candidate binding prevents acceptance-grade cache reuse |
| R-18 | Make routing empirical and inspectable | Profile eligibility and selection have explicit basis; provider updates trigger conformance checks |
| R-19 | Keep privacy and provenance through derivation | Summaries, counterexamples, logs, lessons, and exports retain applicable sensitivity controls |
| R-20 | Keep artifacts and explanations auditable | Referenced bytes are retrievable/hash-checked; explanations trace actual rules/evidence rather than a generated story |
| R-21 | Reconcile Mote's provisional concurrency | Publication, acceptance-as-of, execution, and authorization are distinct; late/conflicting operations trigger reconciliation |
| R-22 | Make documents and schemas coherent | One vocabulary, working internal links, schema-valid examples, acyclic plan, and requirement coverage are checked |
| R-23 | Test ergonomics as a causal change | Same-model flat-CLI versus situation-surface trials isolate interface effects from model choice |
| R-24 | Preserve graceful degradation | Provider loss, budget exhaustion, laptop sleep, stale lessons, and missing artifacts yield explicit resumable/blocked states |

The architecture owns invariants behind these requirements. The protocol owns wire semantics. The machine-readable plan is the authority for task IDs, dependencies, and gates; the Markdown plan is generated from it.

## 11. Evaluation and release criteria

### 11.1 Deterministic correctness first

Use a fake harness and controlled clocks before paid models. Test late Mote operations, concurrent reservations, stale guards, duplicate requests, crashes before/after spawn, unknown billing, contradictory evidence, corrupt artifacts, unauthorized proposals, Git/log crash windows, and lesson invalidation.

The planned schema/fixture checks will validate the design package, not the future Rust implementation. Those schema/fixture files were not supplied locally; MR-01 creates them. Current local checks cover the roadmap and documentation only. Runtime conformance requires independent implementation tests and adversarial execution-boundary tests.

### 11.2 Agent ergonomics benchmark

Use matched tasks and the same model/profile with: (A) the existing multi-command surface and task text; (B) the new situation surface, with the same available underlying facts and authority. Then evaluate provider substitution separately.

Scenarios include cold start, interrupted resume, handoff to a different provider, misleading stale summary, a necessary file outside initial context, an unrelated concurrent note, revoked authority, unknown quota, a bad recommendation, a counterexample to a promoted lesson, and a valid unconventional solution.

Measure accepted-task quality, escaped defects, total billed context and inference, reorientation calls, invalid actions, repeated failed work, unjustified confidence, unnecessary escalation, elapsed time, and human minutes. Count the entire pipeline, including context construction, maintenance, review, and failures.

Initial engineering targets—not claims of achieved performance—are: one orientation call for supported fixtures; at most two targeted context expansions on the median routine fixture; no silent required-context loss; zero model calls for ordinary coordination; zero wrong-scope authorization in adversarial tests; and a material reduction in accepted-task resources without a lowered acceptance standard.

Retain the original aspirational 50% frontier-consumption reduction on ordinary work as a hypothesis to test, not an entitlement or guaranteed multiplier. A 30–50-task pilot can reveal problems; it cannot certify rare-defect parity. Report distributions and uncertainty, not a single flattering average.

### 11.3 Build the complete thin loop first

The first vertical slice is **observe → inspect → guarded act → fake bounded attempt → evidence → revised observe**. It includes stale state, failure, and a restart. This tests coherence before adding providers or elaborate automation.

Next add protected sequential execution and one native adapter, then exact acceptance, then economical profiles and qualified knowledge. Parallelism is last. Every stage leaves the tracker independently useful.

## 12. Non-goals and guarantee boundaries

No permanent frontier manager, agent chat mesh, generic workflow DSL, graph database, vector-store requirement, automatic policy self-modification, unbounded self-improvement loop, or new coding tool loop. No expectation that every task emits a lesson. No mandatory RPC around every native shell/test operation.

The system does not guarantee perfect semantic correctness, globally complete context, immutable vendor behavior, exactly-once inference billing, globally serializable Mote writes, or atomic Git/Mote mutation. It cannot protect against unrestricted same-user processes merely by using worktrees and actor labels.

Unattended guarantees apply only to the tested managed boundary. A profile that cannot enforce its claimed sandbox, billing, or credential separation must be downgraded or denied; the UI must not disguise the limitation.

## 13. Design result

The agent should experience one intelligible environment: **a goal, a set of remaining obligations, a current evidence-backed situation, a few lawful actions, and an accountable result**. Implementation modules exist to preserve those meanings.

The system becomes more capable by turning hard-won understanding into checked, scoped, reusable artifacts—not by retaining an ever longer conversation or installing a more expensive permanent supervisor.
