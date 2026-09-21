# Candidates and recovery

Read this reference for review/landing records, candidate-bound reservations,
or reservation recovery. Use `mote help --all` to confirm exact syntax supported
by the installed CLI.

## Candidate-bound paths

Bind only the candidate's declared exact paths:

```sh
mote reserve <path> --candidate <cand-id>
mote preflight --candidate <cand-id> --paths <path>
```

Candidate reservations remain path-blocking if the candidate lands, is
abandoned, is superseded, or loses authorization. Reauthorization does not
silently revive a reservation invalidated by an earlier revoke.

## Orphaned reservations

Closing an issue while its leases remain live makes those leases orphaned; it
does not free the paths. The owner may release them. To continue the exact-path
work on a successor issue, claim the successor and adopt the orphaned
reservation:

```sh
mote claim <successor-bd-id>
mote adopt <rv-id> --issue <successor-bd-id>
```

Adoption is compare-and-set and retains the exact paths. It rejects live or
expired reservations, unclaimed targets, and stale concurrent attempts. An
orphan label is never permission to bypass a reservation.

## Review and landing evidence

Use candidate records when work needs a durable review or authorization trail.
Record the candidate's exact paths and revision, review result, authorization,
and landing result through the corresponding Mote commands. Do not use a review
record as evidence that a commit was pushed or deployed; verify the real Git or
runtime state separately.

When the CLI rejects a mutation, inspect the issue, candidate, reservation, and
history before retrying. Mote uses stable exit codes: success is `0`, domain or
conflict rejection is normally `2`, and usage errors are distinct. Prefer JSON
output when automation must branch on structured state rather than prose.
