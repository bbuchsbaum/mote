# Defect: terminal candidate rows still emit their full blocker list

**Status:** open, reproduced 2026-09-01
**Applies to:** candidate protocol v3, `mote candidate list` / `show`
**Reported from:** storymodel4s store `st-01M14B2C58PJ9415QYYMFGPD0N`

## Summary

A candidate whose phase is `landed`, `landed_out_of_band`, `superseded` or
`abandoned` continues to emit every blocking landability reason it carried
before the transition. The row is dead, the blockers are unactionable, and they
dominate the output of `mote candidate list`.

storymodel4s AGENTS.md §L4 states the requirement directly:

> `superseded`, `abandoned`, `landed` and `landed_out_of_band` rows must emit no
> blocking reasons and must not appear in the default queue. Measured
> 2026-09-01: **93 of 133 candidates were dead or already landed and generated
> 3,737 blocker strings**, burying the 3 rows that mattered.

That rule was written on 2026-09-01. The behaviour is unchanged.

## Measurement

Against the storymodel4s store, later the same day:

```
total candidates: 141
terminal rows: 103  emitting 3308 blocking reasons   <-- must be 0
live rows:      38  emitting  838 blocking reasons
```

**79% of all blocker output comes from rows nobody can act on.** The 38 live
rows — the only ones a reader can do anything about — are outnumbered four to
one in the noise they are buried in.

## Reproduction

Reconcile any pending candidate, then read it back:

```
mote candidate reconcile <CAND> --target main --expect-phase <PHASE_OP_ID> \
  --operator-override --reason "..." --idempotency-key <KEY>

mote candidate show <CAND> --json
```

Observed on `cand-13J7K02VCBNBYWQG8354Y9MGZY` immediately after a successful
reconcile:

```
phase:        landed_out_of_band
landable:     false
reason_codes: [ancestor_abandoned, ancestor_pending, git_evidence_stale,
               phase_not_pending]
blocking reasons emitted: 13
```

The transition itself prints `preserved pre-transition blockers:
ancestor_abandoned,ancestor_pending,git_evidence_stale`, so the preservation is
deliberate. The defect is that preserved history is then re-emitted as *live
blocking reasons* on a row that has already landed.

Note `phase_not_pending` among the codes: the row is blocked partly on the
grounds that it is no longer pending. For a terminal row that is vacuous.

## Why this matters beyond tidiness

This is the mechanism behind a zero-ship day. On 2026-09-01 the storymodel4s
fleet proposed 170 candidates and landed 12. Three were `pending landable` with
no real blockers and were never found, because the queue that should have
surfaced them was emitting thousands of strings from rows that were already
dead. A queue that must be filtered by hand is a queue nobody reads, and the
cost was a full night of ready work.

## Suggested shape of the fix

Landability is derived state (§2 of `operational_audit.md`: computed from the
accepted op set, replayable, never consulting Git). Deriving it for a terminal
phase is the error — there is no landing left to gate.

1. Compute no landability reasons at all when phase is terminal; report
   `landable: false` with an empty reason set, or omit the field.
2. Keep the pre-transition blockers where they belong — as history on the
   reconciliation/landing record, which already carries them — not as live
   reasons.
3. Exclude terminal rows from the default `mote candidate list` queue, behind
   an explicit `--all` or `--include-terminal`.
4. Never emit `phase_not_pending` as a blocking reason; it is a statement about
   the phase, not an obstacle to it.

## Related

There is a separate three-commit branch, `salvage/candidate-target-scope`
(`be3197b`, `60f6804`, `16b566a`), recovered from
`/private/tmp/mote-target-scope-fix-20260901` on 2026-09-01 after the Codex
fleet exhausted its token budget. It merges cleanly with `main` and the full
suite passes (380 tests). It hardens candidate target scope and does **not**
address this defect.
