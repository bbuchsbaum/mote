# Coordination lifecycle development profile

Status: revised contract draft for GA-15 and GA-16. This immutable capability
extends the coordination core with compound lifecycle actions. It neither
changes core meaning nor authorizes implementation, CLI wiring, migration,
adoption, publication, or hosted qualification.

An authority selecting it has exactly this sorted capability list:

```text
canonical-transactions-v1
coordination-core-v1
coordination-lifecycle-v1
explicit-release-v1
```

A core-only manifest rejects lifecycle actions. Unknown capabilities, action or
transition kinds, fields, and typed task envelopes fail closed.

## Normalized action schema

The command remains `{"kind":"coordination","action":ACTION}`. ACTION is a
strict tagged object: every listed field is required, including nullable fields;
unknown fields, duplicate keys, malformed IDs, floats, and omitted nullable
fields are syntax errors before publication.

Task IDs are `bd-` plus 1–100 ASCII letters, digits, hyphens, or underscores;
reservation IDs use the analogous `rv-` prefix. `expect_status` is a nonnull
clock string. All clocks and tokens are 1–256 printable ASCII bytes (`0x20`
through `0x7e`). A non-null `note` is 1–1,048,576 UTF-8 bytes. `reason` is
1–4096 UTF-8 bytes and `trim(reason)` must be nonempty. `tags` and `exclude`
are 0–64 sorted, distinct entries; tags use the core tag bounds and `exclude`
contains task IDs. `paths` has 0–64 normalized project-relative entries, sorted
and distinct, rejecting controls, backslashes, absolute paths, and dot or
dot-dot components. It is empty iff `reservation` is null; a non-null stable
reservation ID requires 1–64 paths. Ownership maps have lexically sorted keys
and printable-ASCII token values.

All actions, including recovery, first require the requesting session to be
active and owned by request actor. Timestamps stay descriptive. No lifecycle
planner calls TTL, ready, or read helpers.

```json
{"kind":"begin","id":"bd-one","expect_status":"gaop-00000000000000000007-000000-eff-old","reservation":"rv-one","paths":["src/a.rs"],"note":"starting"}
{"kind":"next","tags":["distributed"],"exclude":["bd-already"],"reservation":null,"paths":[],"note":null}
{"kind":"done","id":"bd-one","expect_status":"gaop-00000000000000000010-000000-eff-status","expect_claim":"eff-old-000001","reservations":{"rv-one":"eff-old-000002"},"note":"complete"}
{"kind":"handoff","id":"bd-one","expect_status":"gaop-00000000000000000010-000000-eff-status","expect_claim":"eff-old-000001","reservations":{"rv-one":"eff-old-000002"},"to_session":"sess-b","disposition":"release","note":"continue"}
{"kind":"handoff","id":"bd-one","expect_status":"gaop-00000000000000000010-000000-eff-status","expect_claim":"eff-old-000001","reservations":{"rv-one":"eff-old-000002"},"to_session":"sess-b","disposition":"transfer","note":"continue"}
{"kind":"session.end","expect_session":"eff-enroll-a","reason":"laptop retired"}
{"kind":"session.recover","source_session":"sess-a","expect_session":"eff-enroll-a","claims":{"bd-one":"eff-old-000001"},"reservations":{"rv-one":"eff-old-000002"},"disposition":"release","recipient_session":null,"reason":"holder crashed"}
{"kind":"session.recover","source_session":"sess-a","expect_session":"eff-enroll-a","claims":{"bd-one":"eff-old-000001"},"reservations":{"rv-one":"eff-old-000002"},"disposition":"transfer","recipient_session":"sess-b","reason":"holder crashed"}
```

| Action | Exact nullable and enumerated fields |
| --- | --- |
| `begin` | `note` is required nullable string; `reservation` is required nullable reservation ID. |
| `next` | `note` is required nullable string; `reservation` is required nullable reservation ID. |
| `done` | `note` is required nonnull string. |
| `handoff` | `note` is required nonnull string; `disposition` is exactly `release` or `transfer`. |
| `session.recover` | `disposition` is exactly `release` or `transfer`; `recipient_session` is exactly null for release and a nonnull string for transfer. |

Embedded session IDs use the request `session` identifier bounds. A malformed
nullable/enumerated combination is a syntax error and creates no transaction.

`begin` requires a known nondeleted task, status `open` or `doing`, satisfied
dependencies, and no claim. `deps_satisfied` means every dependency target is
absent, deleted, or `closed`; core still refuses adding absent/deleted endpoints.
`next` considers the same eligibility, additionally excluding tasks with an
open reservation. It retains entries having every requested tag, removes
excluded IDs, and selects priority ascending then task ID ascending. It has no
assignee filter and no `--resume` mode. A next retry reselects from its new
parent; direct begin retains its supplied status clock.

`done` needs an exact own claim and a complete exact map of this session's open
reservations for its task, and refuses any open reservation for that task held
by another session. `handoff` also needs the sender's exact claim/map but always
transfers the claim to its active recipient with a new event token; disposition
only controls whether the sender's listed reservations release or transfer.
Other-session reservations on a handoff task are permitted, untouched, and
reported. `session.end` enumerates all current own claims and reservations.
Recovery requires an active requester, a distinct active source, the source
enrollment token, complete exact observed source holdings, and a trimmed reason.
Release recovery removes claims and closes reservations retaining old token and
paths; transfer recovery retargets actor/session and rotates each token. Ended
or revoked session records retain their original enrollment token and presence;
only status changes.

## Effects and transitions

Planning clones one complete wrapper. A failure discards all provisional state;
a semantic rejection has no effects. Every compound effect gets its listed
zero-based index, ID `eff-<outer-request-id>-<index:06>`, and (for task effects)
adapter ID `gaop-<sequence:020>-<index:06>-<effect-id>`. It is one request with
one provisional wrapper, never multiple core requests. Acquisition and transfer
tokens are their ownership event IDs; releases retain old tokens.

| Action | Exact ordered effects |
| --- | --- |
| begin | optional `reservation_acquired`; `claim_acquired`; status task op if prior status is open; optional progress note task op |
| next | same as begin for the selected task |
| done | closed status task op; completion note task op; reservation releases by ID; claim release |
| handoff release | handoff note task op; claim transfer; reservation releases by ID |
| handoff transfer | handoff note task op; claim transfer; reservation transfers by ID |
| session.end | claim releases by task ID; reservation releases by ID; session ended last |
| recovery release | recovery claim releases by task ID; recovery reservation releases by ID; session revoked last |
| recovery transfer | recovery claim transfers by task ID; recovery reservation transfers by ID; session revoked last |

For begin/next the status patch is exactly
`{"kind":"patch","entity":SELECTED,"set":{"status":"doing"},"expect":{"status":OBSERVED_PARENT_CLOCK}}`.
For next, the observed clock is freshly derived on each attempt; for begin it
must equal the request clock. For done it is exactly
`{"kind":"patch","entity":TASK,"set":{"status":"closed"},"expect":{"status":REQUEST_EXPECT_STATUS}}`.
Every typed task envelope has its canonical adapter ID, request actor and
timestamp. Note envelopes are `kind:"note"` with exact request text and note
kind `progress` (begin/next), `note` (done), or `handoff` (handoff).

Lifecycle adds strict ownership transitions; all listed fields are required.
For recovery transitions, `new_actor`, `new_session`, and `new_token` are
explicit nulls for release. On transfer, `claim_recovered` and
`reservation_recovered` use their respective fresh effect IDs as `new_token`,
while `session_revoked.new_token` is the recipient's existing enrollment token.

```json
{"kind":"claim_transferred","task":"bd-one","from_actor":"alice","from_session":"sess-a","old_token":"eff-old-000001","to_actor":"bob","to_session":"sess-b","token":"eff-req-000001"}
{"kind":"reservation_transferred","reservation":"rv-one","task":"bd-one","paths":["src/a.rs"],"from_actor":"alice","from_session":"sess-a","old_token":"eff-old-000002","to_actor":"bob","to_session":"sess-b","token":"eff-req-000002"}
{"kind":"session_ended","session":"sess-a","actor":"alice","token":"eff-enroll-a","reason":"laptop retired"}
{"kind":"claim_recovered","task":"bd-one","requester_actor":"bob","requester_session":"sess-b","source_actor":"alice","source_session":"sess-a","old_token":"eff-old-000001","disposition":"release","new_actor":null,"new_session":null,"new_token":null,"reason":"holder crashed"}
{"kind":"reservation_recovered","reservation":"rv-one","task":"bd-one","paths":["src/a.rs"],"requester_actor":"bob","requester_session":"sess-b","source_actor":"alice","source_session":"sess-a","old_token":"eff-old-000002","disposition":"transfer","new_actor":"bob","new_session":"sess-b","new_token":"eff-req-000002","reason":"holder crashed"}
{"kind":"session_revoked","session":"sess-a","actor":"alice","token":"eff-enroll-a","requester_actor":"bob","requester_session":"sess-b","disposition":"transfer","new_actor":"bob","new_session":"sess-b","new_token":"eff-enroll-b","reason":"holder crashed"}
```

The recovery effects therefore retain requester, old actor/session/token,
disposition, new actor/session/token, and reason in immutable history. The
end/revoke transition token is always the source's original enrollment token;
its session record changes only `status` to `ended` or `revoked`.

## Deterministic rejection order

Syntax validation precedes semantic planning. Semantic rejections are exactly
`{"status":"rejected","result":{"code":CODE,"subject":SUBJECT}}`, with no
effects. Commonly, active requester is checked first. Exact remaining order is:

| Action | Checks after active requester |
| --- | --- |
| begin | known nondeleted task; status clock; open/doing plus deps; absent claim; requested reservation ID unused; global path conflict; task adapter |
| next | select eligible task; if none `no_ready_task` subject `next`; requested reservation ID unused; global path conflict; task adapter |
| done | known nondeleted, nonclosed task; status clock; exact own claim; exact own reservation map; other-session reservation guard; task adapter |
| handoff | known nondeleted, nonclosed task; status clock; exact own claim; recipient active; recipient not sender; exact own reservation map; task adapter |
| session.end | enrollment token; enumerate current holdings; effects |
| recovery | source active; source not requester; source enrollment token; recipient active if transfer; recipient not source; exact claim map; exact reservation map; effects |

Thus wrong source token plus a bad recipient rejects `enrollment_mismatch` with
the source session; unavailable recipient plus bad claim map rejects
`session_unavailable` with recipient session; both bad maps reject
`claim_mismatch` using the lexically first differing task ID. Exact holding-map
comparison considers the sorted union of supplied and actual IDs. It returns
`claim_mismatch` with the first task ID or `reservation_mismatch` with the first
reservation ID. Recipient equals sender/source rejects `self_target` with that
session ID. Enrollment mismatch uses subject session ID.

The lifecycle codes are: `session_unavailable`, `self_target`,
`enrollment_mismatch`, `task_unavailable`, `stale_field`, `claim_occupied`,
`claim_mismatch`, `reservation_exists`, `path_conflict`,
`reservation_mismatch`, `held_work`, `no_ready_task`, and
`task_effect_rejected`. Existing core code subjects remain unchanged. Multiple
paths select the lexically first conflicting open reservation ID.

## Exact accepted results

Every accepted result contains `code:"ok"`, `subject`, and `effect_ids` in
effect order. All result arrays are ascending by their ID key.

```json
{"code":"ok","subject":"bd-one","effect_ids":["eff-r-000000","eff-r-000001"],"task":"bd-one","claim_token":"eff-r-000000","reservation":null}
{"code":"ok","subject":"bd-one","effect_ids":["eff-r-000000","eff-r-000001"],"task":"bd-one","claim_token":"eff-r-000001","reservation":{"id":"rv-one","token":"eff-r-000000","paths":["src/a.rs"]}}
{"code":"ok","subject":"bd-one","effect_ids":["eff-r-000000","eff-r-000001","eff-r-000002","eff-r-000003"],"task":"bd-one","released_claim":"eff-old-000001","released_reservations":[{"id":"rv-one","token":"eff-old-000002","paths":["src/a.rs"]}]}
{"code":"ok","subject":"bd-one","effect_ids":["eff-r-000000","eff-r-000001","eff-r-000002"],"task":"bd-one","disposition":"release","recipient_session":"sess-b","claim_token":"eff-r-000001","reservations":[{"id":"rv-one","paths":["src/a.rs"],"previous_token":"eff-old-000002","token":null}],"unaffected_other_session_reservations":[]}
{"code":"ok","subject":"bd-one","effect_ids":["eff-r-000000","eff-r-000001","eff-r-000002"],"task":"bd-one","disposition":"transfer","recipient_session":"sess-b","claim_token":"eff-r-000001","reservations":[{"id":"rv-one","paths":["src/a.rs"],"previous_token":"eff-old-000002","token":"eff-r-000002"}],"unaffected_other_session_reservations":[{"id":"rv-other","task":"bd-one","actor":"carol","session":"sess-c","token":"eff-other","paths":["docs/a.md"]}]}
{"code":"ok","subject":"sess-a","effect_ids":["eff-r-000000","eff-r-000001","eff-r-000002"],"session":"sess-a","released_claims":[{"task":"bd-one","token":"eff-old-000001"}],"released_reservations":[{"id":"rv-one","task":"bd-one","token":"eff-old-000002","paths":["src/a.rs"]}]}
{"code":"ok","subject":"sess-a","effect_ids":["eff-r-000000","eff-r-000001","eff-r-000002"],"source_session":"sess-a","disposition":"release","recipient_session":null,"claims":[{"task":"bd-one","previous_token":"eff-old-000001","token":null}],"reservations":[{"id":"rv-one","task":"bd-one","paths":["src/a.rs"],"previous_token":"eff-old-000002","token":null}],"unaffected_other_session_reservations":[]}
{"code":"ok","subject":"sess-a","effect_ids":["eff-r-000000","eff-r-000001","eff-r-000002"],"source_session":"sess-a","disposition":"transfer","recipient_session":"sess-b","claims":[{"task":"bd-one","previous_token":"eff-old-000001","token":"eff-r-000000"}],"reservations":[{"id":"rv-one","task":"bd-one","paths":["src/a.rs"],"previous_token":"eff-old-000002","token":"eff-r-000001"}],"unaffected_other_session_reservations":[]}
```

The first two objects are begin/next results; the next is done; then handoff
release and transfer; then end; then recovery release and transfer. The
unaffected list contains only open reservations held by another session on task
IDs touched by source claims/reservations, or the handoff task. Receipt recovery
returns the exact original accepted or rejected result before session checks.
Expected compound golden fixtures are required before implementation acceptance.
They must distinguish the source enrollment token, recipient enrollment token,
and each fresh transferred-holding token.

## Explicit refusals

Generic status/close/delete/bulk commands, `--force`, automatic TTL expiry,
automatic takeover, and `next --resume` are unsupported and must refuse while
this profile is active; they cannot bypass held-work checks. Announcement has no
effect. Until GA-19 qualifies it, lifecycle `--announce` must return an explicit
unsupported-capability error, never silently omit it.
