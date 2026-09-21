---
name: mote-tracker
description: "Track work and coordinate paths with Mote issues, claims, reservations, notes, and direct messages."
---

# Mote Tracker

Mote is a daemonless, append-only local issue tracker and coordination system.
Use the CLI for all state changes; never hand-edit `.mote/ops/*.json`.

Run `mote help --all` when exact or newly added command syntax matters. Its JSON
form, `mote --json help --all`, is the stable discovery surface for automation.

## Core workflow

Start with the scope the request needs:

```sh
mote doctor
mote ready
mote board
mote show <bd-id>
```

Create an issue only when no suitable one exists. Before editing shared paths,
check and reserve the exact files:

```sh
mote preflight --issue <bd-id> --paths <path> [<path> ...]
mote begin <bd-id> --paths <path> [<path> ...] --note "starting"
```

If preflight or begin reports a conflict, do not edit the path. Inspect it with
`mote who-has <path>`, `mote show <bd-id>`, and, when needed,
`mote history <bd-id> --include-rejected`.

Record only meaningful state changes:

```sh
mote note <bd-id> --kind progress "what changed"
mote note <bd-id> --kind decision "decision and why"
mote note <bd-id> --kind blocker "what is blocked"
```

When implementation is complete but required review is still outstanding, move
the issue to `review` rather than closing it:

```sh
mote set <bd-id> status=review
mote note <bd-id> --kind progress "implementation complete; review requested; evidence: ..."
mote ls --status review
```

The `review` status removes the issue from `mote ready`; it does not itself
record a reviewer, verdict, or release claims and reservations. If review asks
for changes, return the issue to active work:

```sh
mote set <bd-id> status=doing
```

Use the candidate protocol when review must be tied to an immutable Git
candidate with explicit reviewers and verdicts.

After required review passes, finish the work. Otherwise release unfinished
work with a useful handoff:

```sh
mote done <bd-id> --note "reviewed result and evidence"
mote handoff <bd-id> --to <actor> --note "state, evidence, and next action"
mote release <bd-id>
```

Keep reservations narrow and let the issue describe the task. Do not create
tracker churn for routine observations that no future worker needs.

## Conditional guidance

- For multiple terminals, actor ambiguity, sessions, presence, heartbeats,
  direct messages, or request lifecycle, read
  [sessions and messaging](references/sessions-and-messaging.md).
- For candidate-bound reservations, review records, orphaned reservations,
  adoption, landing, or recovery, read
  [candidates and recovery](references/candidates-and-recovery.md).
- For an uncommon command or the complete established semantics retained from
  the earlier skill, read the [full command guide](references/command-guide.md)
  after checking `mote help --all` for current syntax.
- For public cross-agent discussion, use `$mote-message-board`; the tracker owns
  issues, claims, reservations, notes, and private requests.

## Boundaries and completion

Read-only inspection commands do not require a claim. Mutating commands publish
append-only operations, so use the actor and issue intended by the request and
do not infer permission for unrelated external actions.

When an implementation task is finished, the Mote issue is done only after the
requested artifact and appropriate checks are complete. If work remains,
record the actual blocker or next step and release the claim rather than marking
partial work complete.
