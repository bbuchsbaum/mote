# Mote Run planning validation

September 19, 2026. Scope: the locally present design documents and roadmap.
This is not runner implementation, protocol-schema, provider or sandbox
qualification.

## Local reproducible checks

Run from the repository root:

```sh
python3 docs/checks/validate_run_plan.py
```

Local result on September 19, 2026: **PASS** — 16 tasks, 7 stages, 19
dependency edges, all 24 requirement IDs covered, 7 negative controls, 43
relative file links resolved, and exact generated-roadmap match. Repository
`git diff --check` passed; an explicit `rg` scan found no trailing whitespace
in `docs/`. These results apply to this planning package only.

The checker validates unique task IDs, dependency references and acyclicity,
stage ordering, milestone gates, nonempty deliverables/acceptance/falsifiers,
coverage of all 24 PRD requirement IDs, exact generated Markdown and relative
file links in the top-level `docs/*.md` package. Seven deliberately invalid
plan variants exercise duplicate IDs, a cycle, missing dependencies, unknown
requirements, absent acceptance gates, an unknown milestone gate and missing
requirement coverage. Coverage is a planning property, not evidence of completed
requirements. File-link checks do not verify Markdown section anchors or remote
URLs.

After editing `plan.json`, regenerate the human view with:

```sh
python3 docs/checks/validate_run_plan.py --write
```

The repository-root `git diff --check` checks tracked changes only; these
initially untracked documents also need explicit whitespace inspection. The
audit and planning changes leave Rust source and existing unrelated edits
outside scope, so no Rust runtime test result is claimed by this work.

## Imported validation claims: unverified here

The originally supplied `VALIDATION.md` asserted the following September 18
results. Their report, scripts and supporting fixtures were not supplied in
this checkout, so they are preserved below as attributed historical claims,
not passes for the current package:

| Historical assertion | Local status |
|---|---|
| 7 valid core fixtures; 16 invalid variants rejected | Schema and fixtures absent; MR-01 owns new fixtures |
| 23 tasks, 5 stages, 63 dependency edges | Original plan absent; replaced by the new 16-task roadmap |
| Duplicate IDs and a self-cycle rejected | Original checker absent; current checker has its own controls |
| Generated roadmap matched original JSON | Original pair absent; new pair checked independently |
| 29 links across 11 Markdown documents | Original full package absent; current local links checked |
| 3 scripts parsed and 10 installer helper cases passed | Original scripts/installer absent; not reproduced |
| Original PRD SHA-256 preserved | Original baseline/hash evidence absent; unverifiable locally |

`validation-report.json`, `checks/validate_design.py`, `checks/render_plan.py`,
the baseline directory and installer mentioned by the imported package are not
available. The new `checks/validate_run_plan.py` is a different, deliberately
narrow checker. It does not validate an exchange JSON Schema or authenticate
evidence, and it cannot prove execution safety.

No current tariff verification, account inspection, live model call, security
audit, agent benchmark, repository commit or push is part of this assessment.
Future runtime gates remain planned in [implementation-plan.md](implementation-plan.md).
