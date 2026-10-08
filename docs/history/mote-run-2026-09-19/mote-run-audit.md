# Mote Run: audit and design workshop

September 19, 2026. Planning assessment, not runtime qualification.

## Recommendation

Build a **recoverable, sequential runner for one contracted Mote task**. Its
first useful outcome is an exact candidate with independent evidence and an
honest disposition after interruption. Keep the situation interface: it can
make the existing coordination machinery substantially easier to use. Prove
that benefit before adding provider optimization, automatic learning, or
parallel workers.

The revision-2 PRD has unusually good requirements for uncertainty, independent
acceptance, and resource accounting. Its weakness is the distance between those
requirements and an implementable first product. It specifies several research
and infrastructure projects at once, sometimes describing missing mechanisms
as things to “retain.” The right next step is a complete narrow loop with
explicit trust and recovery boundaries.

Deliverables: [architecture and decisions](architecture.md),
[implementation plan](implementation-plan.md), and [machine plan](plan.json).
The user selected code changes in one repository as the first workload;
analysis acceptance remains a separately gated extension. No runtime implementation,
provider selection, paid execution, commit, or publication is part of this work.

## Evidence and limits

Inspected the five supplied files in `docs/`, current Rust source, candidate
documentation, relevant test definitions, and the local Mote board. Source HEAD
was `a790ca63dccacec3c68e696e980f965721a89130`; the checkout reported ten commits
ahead of its local tracking ref. That is not a live remote comparison. The
original `docs/` files were untracked; unrelated skill edits and a scheduler
lock were already present.

No runner module or `run` CLI variant exists in the inspected source. Test
definitions below establish existing design intent, not a fresh runtime test
result. `mote doctor` reported a clean store before the planning issue was
created. No historical implementation issue was closed by this audit.

## Findings

Severity here means impact on planning or on the claimed runner guarantee.
“Blocking” prevents the affected milestone, not all design work.

| ID | Finding and source | Consequence | Disposition |
|---|---|---|---|
| A-01, blocking | The original PRD links an absent architecture, plan, baseline and schemas. The supplied `VALIDATION.md` reports checks whose scripts, fixtures and report are absent. | The package cannot be reproduced or treated as a completed design. | Add a fresh plan and architecture; distinguish local checks from the imported historical report. Do not reconstruct or certify the missing baseline. MR-01 owns runtime schema completion. |
| A-02, high | PRD §6 says “retain immutable contracts and compare-and-set task bindings,” but [Bead](../src/state.rs) has no contract binding and [Op](../src/op.rs) has no runner contract operation. | An apparent integration detail is actually a new persistence and migration decision. | Keep immutable contracts and their managed task bindings in the runner journal initially; Mote retains task identity and workflow status. |
| A-03, blocking for managed execution | PRD §§4.2, 7.2 require authenticated roles, protected checks and credential separation. Existing [candidate protocol](../candidate_protocol.md) explicitly treats actors as strings and coordination as advisory. | `MOTE_ACTOR`, a worktree, or a printed ticket cannot enforce these requirements. Repository tests can execute arbitrary code. | Qualify a real worker boundary before unattended execution. Gate authority, filesystem access and network/model access separately. |
| A-04, blocking for recovery | PRD §7.1 gives the controller an OS-lifetime lock, but does not say what happens to surviving workers when that controller dies. | A successor can acquire the lock while an old process or provider request still runs. | A journaled launch identity plus owned process containment and recovery precede replacement. A new lock owner alone is insufficient. |
| A-05, high | `observe` is passive, yet protocol §3 calls its basis “reconciled”; §2 allows `wait` to “reconcile locally.” | An innocent status call could publish state, renew a lease or trigger effects unless reconciliation is defined. | Passive projection may compute disagreement. Only an admitted controller transition records reconciliation or changes external state. |
| A-06, high | Protocol §6 resolves duplicates before fresh guards but leaves canonical request identity and credential revocation unspecified. | A retry from a refreshed view may conflict spuriously; a revoked principal may retrieve confidential results. | Authenticate first; deduplicate stable intent independently of transport/view metadata; authorize result disclosure separately; never rerun a known intent. |
| A-07, blocking for budget claims | PRD §9 correctly distinguishes hard monetary bounds from observed CLI cancellation, but no concrete admission boundary or settlement rules exist. | A subscription observation or final usage report cannot enforce a dollar ceiling. | Use explicit assurance classes, integer accounting and retained unknown reservations. Deny policies requiring hard bounds when the adapter cannot prove them. |
| A-08, high | PRD §§5–8 define receipts and references, but omit artifact ownership, retention, import and garbage-collection rules. | A digest of a deleted log is not inspectable evidence; a copied artifact can cross a data boundary. | Content-addressed immutable storage, verified reads, protected import and pinned reachability before acceptance. No automatic garbage collection in the first release. |
| A-09, high | Existing [candidate protocol](../candidate_protocol.md) requires distinct submitter, reviewer and authorizer roles, with portable v3 proposals and target-scope evidence. The runner migration text says simply “v1 unchanged.” | A two-agent implementation/review story does not completely describe landing authority. Reusing an old baseline can miss current checks. | Preserve all current candidate rules. Start with independent human review and a distinct authorized operator; bind trusted principals to allowed Mote actors at the bridge. |
| A-10, high | PRD §§7–9 cover coding, analysis, consultations, learning, providers and concurrency before any executable slice exists. | A first release can become a framework without a demonstrably useful task path. | One repository, one task, one worker, one qualified profile. Defer analysis, routing, lessons, automatic integration and concurrency behind distinct gates. |
| A-11, medium | “Three implementations, one consultation, one review plus follow-up” is a fixed procession ceiling with no handling for infrastructure retries or revised contracts. | Rebinding a contract or renaming an action could reset the effective cap; infrastructure failures can consume the wrong counter. | Counters and resource envelopes live at the parent task; every model-spending execution counts somewhere. Explicitly classify infrastructure retries; contract revision never resets spending. |
| A-12, high | PRD §11 asks for material resource reduction without a frozen estimand, failure denominator, or go/no-go threshold. | A cheap successful subset can hide increased failures and human repair. | Paired same-profile trials, all assigned tasks counted, fixed acceptance, explicit quality margin and economic threshold before model evaluation. No rare-defect claim from 30–50 tasks. |

### What can actually be reused

| Current mechanism | Runner use | Limit |
|---|---|---|
| `State`, replay, task dependencies and nonblocking relations | Compile task scope and readiness; retain one task graph | No existing contract binding, attempt journal or authenticated principal |
| `preflight`, `begin`, claims and reservations in [cli.rs](../src/cli.rs) | Coordinate managed paths with standalone users | Compound operations and compensations are not transactions; replay can change acceptance |
| Candidate types, `candidate_landability_at`, evidence and current target-scope checks | Record and evaluate exact code proposals | No sandbox or Git ref mutation; recorded attestations need a trusted runner bridge |
| [events.rs](../src/events.rs), [watch.rs](../src/watch.rs) | Passive waiting and change notifications | Need a shared view cursor over both Mote and runner state; time expiry also matters |
| [publish.rs](../src/publish.rs), canonical payload conventions | Reuse proven publication patterns where applicable | An append-only coordination log is not a serialized budget ledger |
| [coord tests](../tests/coord.rs), [candidate tests](../tests/candidate_protocol.rs), [target-scope tests](../tests/candidate_target_scope.rs), [failpoints](../tests/failpoints.rs) | Regression fixtures and failure-injection patterns | Existing green tests would not qualify the new execution boundary |

The reserve race test explicitly allows both callers to report temporary
success before replay accepts one reservation. The separate `begin` race test
expects one immediate winner. Neither is proof of exclusion against arbitrary
processes or late imported operations. Keep the stronger runner guarantee in
its own managed domain.

### Existing work that must be reconciled before integration

These are observed local tracker states, not newly diagnosed code defects:

- `bd-01M1DBERGRFM6ETT3RYX31VSF8` remains open; its latest recorded independent
  review blocked an older recovery commit over missing policy/binding clocks.
  Current source contains a complete-clock guard, so requalify the selected
  baseline instead of declaring the older defect still present or resolved.
- `bd-01M1E3EH7J5A284G088PW0WJCP` remains doing although current source and
  `tests/candidate_target_scope.rs` contain target-scope machinery. Establish
  exact accepted implementation/release evidence before automatic landing.
- The board also contains open evidence-identity, binary-provenance and
  candidate-pair ancestry work. MR-08 inventories the applicable gates;
  unrelated tracker cleanup does not block the fake runner.

## Workshop: choose the product deliberately

| Direction | Useful outcome | Main cost | Decision |
|---|---|---|---|
| Thin harness launcher | One command starts a model for a bead | Leaves interruption, accounting and acceptance to the user | Useful adapter test, insufficient as the product |
| Sequential managed task | One task has a recoverable attempt, bounded authority and independently checked result | Requires a small durable controller and explicit execution boundary | Recommended first product |
| Autonomous portfolio manager | Selects tasks, routes models, learns procedures and runs workers concurrently | Multiplies policy, evaluation and recovery interactions | Later hypotheses, contingent on measured gains |

Keep the five semantic operations. Humans should eventually get convenient
`start`, `status`, `resume` and `cancel` commands that compose the same kernel.
An agent should see a goal, evidence, constraints and a few useful alternatives;
the ordinary editing loop belongs to its native harness. The “one orientation
call” goal is valuable, but must not become “one enormous response.”

The first end-to-end example should be a bounded regression repair: supply a
task, exact base, allowed paths, protected regression check and review policy;
obtain a patch; interrupt the controller; resume without duplicate work; verify
the exact candidate; hand it to an independent reviewer and landing authority.
Also demonstrate one failed check and one unresolved launch. A happy-path demo
alone would miss the distinctive value of this product.

Make three outcomes visible: **accepted candidate**, **useful checkpoint**, and
**blocked with an explicit decision need**. A stopped process is not a fourth
spelling of “accepted.” During the first usable pilot, manual landing remains
explicitly external, and the task stays open until its contract's completion
condition is independently recorded.

## Decision register

Except for the user-confirmed first workload, the defaults below are planning
recommendations, not additional user approvals or implemented capabilities.

| Decision | Recommended default | Reconsider when |
|---|---|---|
| First workload | Code changes in one repository, confirmed by the user | A later product decision adds analysis through MR-11 |
| Control persistence | Local append-only journal plus verified immutable artifacts; one writer, rebuildable projection | Measured load or transactional complexity justifies a database |
| Contract activation | Trusted operator activates an exact contract under managed CAS | A separately authorized policy can safely activate routine contracts |
| Real execution | One foreground controller, one qualified sandboxed worker/profile | Process and accounting recovery pass before background service or concurrency |
| Native provider CLI | Eligible only at its measured assurance level | It proves stronger access/metering controls; never infer these from its name |
| Independent acceptance | Protected deterministic checks plus independent human review; separate landing authority | A model reviewer is qualified for the same task class and confidentiality |
| First integration | Prepare candidate and explain next required decision | The separate automatic-landing milestone passes all crash windows |
| Evidence reuse | Same exact attempt/input identity only | Complete closure and fresh candidate-bound receipt rules are tested |
| Knowledge | Preserve receipts and optional proposed counterexamples | Held-out trials show a promoted-lesson mechanism pays for its maintenance |

Unresolved engineering questions have named gates: choose and falsify the OS
containment mechanism in MR-05; qualify one actual harness and auth mode in
MR-07; reconcile candidate baseline/release status in MR-08; freeze quality and
economics thresholds in MR-12. The plan does not invent answers to deployment
questions before measurements exist.

## Stop and continuation rules

Stop real-worker rollout if credential separation or owned-process containment
cannot be demonstrated. A fake/nonbillable demonstrator remains useful. Stop
automatic integration if candidate evidence cannot establish the exact target
and full landing effect. Keep unknown effects and usage visible and reserved.

After the sequential pilot, continue feature investment only if the failure
record and matched evaluation justify it. Report additional overhead when the
interface fails to save work. A useful runner can succeed at reliability
without proving the PRD's 50% frontier-consumption hypothesis.
