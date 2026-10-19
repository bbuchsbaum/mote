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

## Replicated stores: admission recovery

A fenced replica of a Git-versioned op log fails closed on ops it has not
admitted. Read the error before acting:

- `unadmitted operation <name> (<n> unadmitted in total) … run mote admit`:
  ops arrived by `git pull`/merge. Run `mote admit` (or `mote --json admit`),
  then retry the original command.
- `refusing to admit <name>: <defect>; nothing was admitted` (exit 2): the
  pulled op failed integrity checks (hash, JSON, filename, envelope). Nothing
  was admitted, so every read still fails. Do not edit or delete op files. Find
  the commit that introduced the file (`git log -- .mote/ops/<name>`), and fix
  the source or revert that commit with the owner's approval.
- `authority op changed: <name>`: the bytes of an admitted op changed, for
  example because a merge replaced them. Restore the admitted bytes from Git
  history (`git checkout <commit> -- .mote/ops/<name>`).
- `admitted operation <name> is missing from ops/`: a branch switch or reset
  removed an admitted file. Check out a commit that contains it, or restore it
  with `git checkout <commit> -- .mote/ops/<name>`.
- `authority binding names a missing genesis journal` or `authority genesis
  mismatch` naming `--discard-foreign-format`: `FORMAT.json` carries another
  clone's binding. Run `mote authority migrate --discard-foreign-format` only
  when you know that field came through Git. If this replica lost
  `.mote/authority/`, restore it from backup instead.

A landing blocked behind admission: while `authority/landing-active.json`
exists, unadmitted ops are hidden from reads and `mote admit` refuses with
`landing recovery required; retry the recorded candidate land request, then
run mote admit`. Repeat the exact original `candidate land` command and
idempotency key until it completes, then run `mote admit`. Never remove the
landing journal by hand.

When the CLI rejects a mutation, inspect the issue, candidate, reservation, and
history before retrying. Mote uses stable exit codes: success is `0`, domain or
conflict rejection is normally `2`, and usage errors are distinct. Prefer JSON
output when automation must branch on structured state rather than prose.
