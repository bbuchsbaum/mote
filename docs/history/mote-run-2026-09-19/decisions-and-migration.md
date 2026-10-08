# Mote Run — decisions, migration, and scope of change

**Revision 2.0 · September 18, 2026; local audit annotations September 19.** This is the imported change rationale, not a second source of runtime rules. The [audit](mote-run-audit.md) identifies missing original artifacts and updates the implementation recommendations. Follow the [PRD](mote-run-prd.md), [architecture](architecture.md), and [protocol](agent-protocol.md) for proposed semantics; none is an implemented runner guarantee.

## 1. What was wrong with the earlier center of gravity

The earlier PRD made bounded execution, budget control, verification, and exact-commit integration first-class. Those remain necessary. But the agent was still implicitly responsible for assembling its own situation from contracts, task state, source, logs, model results, and policy. “Context packaging” appeared as a subsystem rather than the central interface through which every subsystem becomes intelligible.

A capable agent should not need to reconstruct a hidden global join or infer what a successful command actually established. The revision therefore does not add an “agent UX layer” on top of an unchanged arrangement. It makes **the evidence-backed situation and the guarded action** the shared interface of the controller, worker, reviewer, operator, and scheduler.

## 2. Decisions

| ID | Decision | Consequence and rejected alternative |
|---|---|---|
| D-01 | Keep bounded attempts; make situations the unit of interaction | A clean job abstraction alone does not solve orientation or control. Do not replace attempts with an autonomous manager. |
| D-02 | One admission evaluator for previews and execution | `why` and `allowed` are derived from actual rules; no separately maintained recommendation logic. |
| D-03 | Five semantic operations, specialized actions discovered lazily | Avoid a wrapper around every existing CLI leaf. Native coding tools remain inside attempts. |
| D-04 | Stable obligation IDs and explicit evidence applicability | Completion is not inferred from prose, confidence, or process exit. |
| D-05 | Dependency-scoped guards and exact-base deltas | Avoid invalidating everything for an unrelated note; avoid treating stale relevant state as current. |
| D-06 | Managed publication/admission/effects are separate | Mote's convergence semantics do not become stronger by rendering a green check mark. |
| D-07 | Context is a versioned working set with omission semantics | No giant mandatory memory file and no silent “summary equals source” substitution. |
| D-08 | Lesson proposal, qualification, promotion, invalidation, retirement | Accretion means warranted reusable capability, not unchecked self-modifying instructions. |
| D-09 | Exact evidence and provenance are shared across context, recovery and learning | No separate incompatible handoff, memory, and review formats. |
| D-10 | Deployment facts live in qualified profiles | Remove volatile model/price/flag assertions from the operative PRD; a historical model name is not an enabled route. |
| D-11 | Build a whole thin nonbillable loop first | Replace component-by-component expansion with an immediately inspectable vertical slice. |
| D-12 | Evaluate interface gains independently of model gains | A cheaper model benchmark alone cannot show that the system is agent-ergonomic. |
| D-13 | Preserve strong verification and finite authority | Agent initiative is encouraged through proposals, not through permission to rewrite its own evaluator. |
| D-14 | Existing Mote primitives stay underneath | Use tasks, nonblocking relations, dependencies, candidates, notes, sessions and evidence; no second tracker. |

## 3. Requirements carried forward

The imported revision reports this mapping of 16 original functional requirements. The original baseline was not supplied, so preservation of its complete requirements has not been independently verified locally:

| Original requirement | Revision 2 coverage |
|---|---|
| FR-01 freeze contract/policy | R-01, R-05, R-08 |
| FR-02 reuse coordination | R-01, R-09, R-21; clarify managed exclusivity versus advisory convergence |
| FR-03 snapshot reconciliation | R-02, R-21 |
| FR-04 effective provider/model/auth | R-09, R-18 |
| FR-05 parent budgets/deadlines | R-10, R-24 |
| FR-06 accounting uncertainty | R-06, R-10 |
| FR-07 worker lifecycle | R-06, R-11, R-24 |
| FR-08 independent verification | R-04, R-09, R-12, R-17 |
| FR-09 exact-commit acceptance | R-12, R-17 |
| FR-10 Git/log recovery | R-06, R-11, R-12 |
| FR-11 authority protection | R-09, R-19 |
| FR-12 data routing | R-18, R-19 |
| FR-13 token-free status | R-14 |
| FR-14 explain blocked/routing state | R-03, R-13, R-20 |
| FR-15 analysis artifacts | R-12 |
| FR-16 backward compatibility | R-01, R-22 |

The original provider price example and detailed model/CLI compatibility assertions are described as historical material. That baseline is absent locally; these claims are not re-certified or used to authorize deployment. Current facts must be resolved through official sources and conformance tests when a profile is enabled.

## 4. Repository-specific corrections

The current Mote already offers combined views (`preflight`, `board`, `in-flight`), generated help, structured errors, idempotent candidate mutations, and discussion semantics. The new surface should compose and expose those services, not claim that agent ergonomics begins from zero.

The current reserve race test permits both concurrent clients to report success before replay converges on one accepted reservation; a separate begin race test expects one immediate winner. Neither proves a strict execution lock. This audit does not edit the older coordination addendum; managed exclusivity is specified in the new architecture.

The existing `AGENTS.md` includes standalone `mote done` instructions. A managed worker must not use that as managed acceptance. A conditional installed-managed-mode branch is planned in MR-09; it has not been installed by this documentation work.

The current candidate protocol, including portable v3 proposals and current target-scope checks, remains authoritative. No core protocol edit is made here. The runner cannot transfer approval to rewritten commits or turn actor labels into access control.

The root `PRD.json` is an existing consolidated core specification. It is not overwritten or augmented with unverified fields. The new machine-readable plan lives beside the runner specification and is explicitly not native Mote import input. A future importer must be idempotent and preserve task/status semantics.

## 5. Document ownership and precedence

| Document | Owns | Does not own |
|---|---|---|
| Existing Mote core docs and candidate protocol | Current tracker/storage/candidate meaning | Model execution or stronger security claims |
| `mote-run-prd.md` | Product scope, requirements, outcomes and non-goals | Exhaustive provider settings |
| `architecture.md` | Types, ownership, invariants, failure and authority model | Duplicate product priorities |
| `agent-protocol.md` | Interaction semantics, operation and error contracts | Independent eligibility rules |
| Planned `schemas/protocol.schema.json` (MR-01) | Future shape of core exchange fixtures; absent locally | Full runtime semantic verification |
| `plan.json` | Implementation task IDs, dependencies, acceptance gates | Live Mote tasks or execution authority |
| `implementation-plan.md` | Generated human view of `plan.json` | Independently editable task truth |
| `AGENT_GUIDE.md` | Minimal runtime bootstrap | Permission grants or speculative commands in uninstalled systems |
| Qualified deployment registry | Current effective models, auth, tariffs and assurance evidence | The product's quality policy |

When shape and prose conflict, it is a design defect, not permission to pick the convenient interpretation. Block the affected action/profile and resolve it through a reviewed change. Do not use a blanket “schema always wins” rule to erase a safety invariant absent from a shape validator.

## 6. Migration path

First, install these documents as proposed design, not implemented capability. Keep existing tracker agents on the current guide unless a trusted installed supervisor explicitly manages their session.

Build the fake-executor vertical slice without changing existing task/candidate behavior. Add typed references compatibly and version the new runner exchange separately. Old operation replay remains unchanged; unknown critical runner versions fail explicitly.

Freeze new attempt contracts; never retroactively assign a contract hash or managed acceptance to historical work that was not checked under it. Legacy observations can inform context as historical evidence with their actual scope, not acquire new authority.

Activate one tested managed execution path and a standing policy. Only then add automatic integration, provider diversity, promoted lessons, or parallelism according to the machine plan's gates. No profile or API budget becomes enabled merely because it appears in an example.

## 7. What this deliverable changes

The local checkout initially contained five revision-2 Markdown files directly under `docs/`. The original baseline, installer, architecture, plan, schemas, fixtures and validation report described by the imported text were absent.

The September 19 audit adds an audit/workshop, architecture, machine plan, generated roadmap and a local plan checker. It corrects the supplied documents' package-status claims. It does not recreate a purported original baseline or claim to have run the missing installer. Schemas and runtime fixtures remain future work under MR-01.

A local Mote issue tracks the documentation/planning work. No remote repository changes, runtime implementation, model launch, provider conformance or agent benchmark are claimed. The [validation record](VALIDATION.md) distinguishes current plan checks from the imported historical assertions.
