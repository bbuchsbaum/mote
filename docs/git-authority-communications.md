# Canonical communication development contract

This is the GA-19 implementation contract. Local behavior and existing immutable
authority profiles retain their meanings. The new fresh profile is exactly
canonical-transactions-v1, coordination-communications-v1, coordination-core-v1,
coordination-lifecycle-v1, explicit-release-v1. Its migrated counterpart adds
authority-retirement-v1 first and legacy-packed-v1 last. Bootstrap and migration
select these new profiles only after integration and qualification.

Current bootstrap/migration select the separate
[announcement-capable profiles](git-authority-announcements.md), which include
this unchanged communications capability. The earlier five/seven-capability
profiles retain their exact meanings and refuse the new compound action.

The coordination action is a strict `communication` variant containing a strict
`action` object tagged by `kind`. Unknown fields, including nested reference and
question fields, are invalid. Nullable fields must be present; lists are required.
All authorization uses the canonical active session belonging to the request
actor. Retirement refusal precedes session checks. No communication operation
creates legacy sessions or changes remote ownership. Live-delivery requirements
are unsupported; message adapters always set require_live false. Imported
recipient-presence evidence retains its historical meaning.

The action fields below exclude metadata: actor, timestamp, operation ID and
effect position are always derived by the planner. New message, post, question
and task IDs are fixed in the request before journaling. New object IDs use the
existing prefixes and ASCII identifier bounds; references to existing imported
objects and operation clocks are opaque UTF-8 strings, including long Unicode
IDs. They are nonempty, exclude control characters, and may be as long as the
maximum packed archive plus 128 bytes (268435584 bytes). Bodies have at most 1 MiB;
short text has at most 4096 bytes. Lists are bounded to 64 unless specified.

| Kind | Required fields |
| --- | --- |
| message.send | id, to, issue (nullable), reservation (nullable), msg_kind, body, answers, idempotency_key (nullable) |
| message.reply | id, request_id, msg_kind, body, idempotency_key (nullable) |
| message.ack | id |
| message.resolve | id |
| post | id, topic, body, reply_to (nullable), post_kind, answers, notify, idempotency_key (nullable) |
| read | topic (nullable), through_post, expect (nullable operation clock) |
| watch | topic, watching |
| topic | topic, title (nullable), description (nullable), post (nullable initial-post draft) |
| sticky | post_id, sticky |
| supersede | old_post_id, new_post_id |
| retract | post_id, reason |
| route | post_id (nullable), topic (nullable), route_state, issue (nullable), note (nullable) |
| decision | id, topic, body, agreed_post_ids, references, open_questions, notify, idempotency_key (nullable), issues |
| question | question_id, action, expect_question, references, note (nullable), successor_question_id (nullable), idempotency_key (nullable) |
| promote | post_id, task_id, title, body, priority, tags, deps |

An initial-post draft has exactly id, body, notify and nullable idempotency_key.
A question draft has exactly question_id and text. References are strict tagged
objects: topic/topic, post/post_id, issue/issue_id, candidate/candidate_id, url/url.
References (at most 256), notifications, answers, agreed posts, issue targets,
tags and dependencies are sorted and duplicate-free. Strings use Rust `str::Ord`;
references use declaration-order kind (topic, post, issue, candidate, url), then
their string target. Question drafts retain
their explicit order and have unique IDs. Message kinds, post kinds, question
actions, route states, topic normalization and field validation follow the
existing reducer's allowlists. The request cannot pass arbitrary reducer ops.

Request receipt recovery is the retry mechanism. A distinct request reusing an
actor-scoped domain idempotency key is rejected, even for matching content;
changed content also rejects. Imported keys stay consumed and original object
IDs remain intact. The CLI explains this remote distinction. Retrying the exact
retained request returns its original accepted or rejected receipt and IDs.

Replies derive recipient, issue/reservation context and correlation from the
canonical root request, then use the actual reducer for recipient/open-state
checks. Acknowledgement and resolution use the actual reducer; double or stale
actions produce durable rejection receipts. Read compares the actor's effective
global/topic cursor to expect before resolving through_post to its actual post
operation ID and applying strict BoardRead. It never marks later posts read.

Each action replays the complete verified legacy State, then all new task and
communication adapters in their global canonical effect order. Never sort the
combined adapters by ID. A task effect retains its existing gaop position ID.
A communication adapter uses the Rust string maximum of ALL original verified
filename stems (including rejected entries, removing only the exact .json
suffix), followed by ~gaop-, the 20-digit sequence, six-digit global effect
index and effect ID. An empty seed uses ordinary gaop-. Thus every new
communication operation sorts above every imported cursor. Derive this prefix
from the immutable archive, never from client input. Indices above 999999 refuse.
CommunicationOp's adapter ID is validated through complete canonical replanning;
its effect identity is independent of its reducer operation ID.

Only the new migrated communication profile checks operational legacy
eligibility, both before publication and during root validation before cache
advancement. Check the exact maximum-derived adapter with the longest legal
effect suffix, accepted message IDs and sender/recipient actors; accepted post
IDs, sent operation IDs, author and topic; question IDs, current clocks, opening
actor and topic; retained topic names/creating actors; global cursor actor/clock;
topic cursor actor/topic/clock; and watcher actor/topic. Apply the same request
reference, actor and topic validators. An unreachable rejected input below the
maximum, historical bodies/reasons and consumed keys remain untouched. Refusal
does not sanitize or rewrite the archive. Older profiles retain their acceptance
rules. Qualification must distinguish rejected maximum controls, earlier accepted
unaddressable IDs/clocks, valid Unicode, and irrelevant rejected control stems.

Planning applies typed effects sequentially to a cloned actual reducer State.
Append adapters to the coordination wrapper only if all effects accept. A failed
effect discards every tentative change and returns no effects, code
communication_effect_rejected, failing global index and the reducer reason.
Explicit preconditions instead use stable specific codes, including stale_cursor,
task_exists, task_unavailable, session_unavailable and invalid promotion dependency.
Syntax validation occurs before journaling; semantic failures have receipts.

Ordinary sends/replies/acks/resolves/read/watch/sticky/supersede/retract/question
produce one communication effect. Post emits BoardPost then BoardSticky for
decision or summary kinds. Topic emits BoardTopic followed by its optional
initial BoardPost. Route emits BoardRoute and, for routed state only, a task
decision note with exact text `routed from discussion TARGET[: NOTE]`, where
TARGET is the post ID or `topic NAME`. Both effects are atomic.

Decision requires issues to equal exactly the sorted issue targets in references
(a syntax error otherwise). It emits BoardDecision followed, for each sorted issue target, by its
BoardRoute and mirrored task decision note. The decision operation itself owns
its question drafts and sticky state. Promote emits task create, sorted tags,
sorted blocking dependencies, BoardRoute, then a task decision provenance note.
The task body is exactly `BODY\n\npromoted from discussion post POST in topic TOPIC`;
the note is exactly `promoted from discussion post POST in topic TOPIC`.
Promotion validates never-used task ID, core create constraints,
all dependency targets and graph rules before committing. No communication
action bypasses existing task validation or consumed-ID constraints. A new
request may deliberately promote one post again; an exact retry never does.

Acceptance vectors must include two independent clients, concurrent duplicate
ack/reply, read expectation changes, imported request/reply and consumed keys,
Unicode/rejected maximum stems, reversed timestamps, interleaved watch/post/read
and task references, route-note atomicity, a reachable late reducer rejection
after earlier provisional effects (for example an initial topic post reusing a
consumed domain key), and private injected-final-effect promotion rollback.
The latter is a planner-engine witness, not a realizable public request. Both
must leave tasks, board, cursors and ownership unchanged. This contract does not
qualify hosted Git, cloud clients, physical second machines or a release.

## Exact validation and result rules

New IDs are msg-/post-/question- followed by 1..100 ASCII alphanumerics, hyphen
or underscore; task IDs follow the identical bd- rule. Existing issue and
reservation references follow the core bd-/rv- rule. Existing communication IDs
and clocks use the opaque bound above. Topics must equal their nonempty trimmed
form, have at most 4096 bytes and no control characters. Actors and recipients
use the request actor rule: nonblank, at most 128 bytes, no control characters.
The CLI trims actor identities consistently at session enrollment and later
requests. The migrated communications profile refuses imported actor identities
with leading or trailing whitespace before publication, because the CLI cannot
enroll those exact identities. It never rewrites the archived actor strings.
Retained explicit notification and resolved notification recipient lists must
also contain enrollable actors; an accepted legacy notification to an invalid
actor refuses the new profile before publication, without rewriting history.
Keys are nullable or 1..128 trimmed bytes without control characters. Bodies,
post/decision/question text and notes are UTF-8 and at most 1 MiB; message/post/
decision/question bodies must be nonblank. Titles, topic title/description and
retract reason have at most 4096 bytes; supplied title and retract reason must
be nonblank, retract reason contains no control characters. Priority is 0..3;
tags follow core nonblank 128-byte no-control validation. URLs/references are
opaque nonempty single-line strings bounded above, with the reducer enforcing
reference existence and URL schemes. Empty lists are valid except where the
existing reducer requires content (such as an agreed decision clause).
Message.send kinds are note, request, handoff, blocked or fyi; reply kinds are
response or decline. Post kinds are post, decision or summary. Question actions
are answer, defer, supersede or close. Route states are open, needs_bead, routed,
resolved; exactly one post/topic is required and issue is present iff routed.

Syntax validation runs first; then retired and active-session checks. The
communication dispatcher performs only these explicit semantic checks in order:
read compares effective cursor (stale_cursor, subject through_post) then requires
its post (post_unavailable, subject through_post); reply requires root existence
(message_unavailable, subject request_id); promote requires absent/unconsumed
task ID (task_exists, subject task_id), then existing live dependencies in sorted
order (task_unavailable, subject first missing dependency), then source post
(post_unavailable, subject post_id). All other state-dependent validation,
including duplicate object/key, question expectations, reply root/recipient,
acknowledgement and resolution, runs in the actual reducer's recorded order.
Promote validates core create syntax before planning; its new task cannot form
a dependency cycle because its ID is never-used and all parents already exist.
At each generated task effect, before applying it to scratch State, validate its
FINAL payload against the corresponding core TaskCreate/TaskTag/TaskDependency/
TaskNote syntax bounds, including generated provenance. This validation can
depend on canonical post/topic data and is therefore semantic. Failure rejects
with exactly code=communication_payload_invalid, subject (as below) and index;
no effects survive. This check precedes reducer application for that effect;
prior explicit preconditions and prior effects' failures retain precedence.
No other precondition or silent state repair is permitted.

Explicit precondition rejections contain exactly code and subject; generated
task payload rejection additionally contains index as specified above. A reducer
rejection contains exactly code=communication_effect_rejected, subject, index
(integer global effect index), reason (the actual reducer rejection string).
It has no effects. Accepted results contain exactly code=ok, subject and
effect_ids in global order. Subject is id for message actions and post/decision,
through_post for read, topic for watch/topic, post_id for sticky/retract,
task_id for promote, old_post_id for supersede, post_id or `topic NAME`
for route, and question_id for question. No additional operation-specific result
fields are emitted; callers obtain records through refreshed projections.

Adapters omit nullable absent fields and empty optional lists as prescribed by
their typed local structs. Message.send has no reply_to, sets correlation_id=id
only for request kind, and sets require_live=false. Reply inherits root sender,
entity, reservation and correlation_id; reply_to=request_id and answers is empty.
An initial post has post_kind=post, no reply_to and no answers. BoardPost for a
regular post retains its explicitly selected kind. The planner creates canonical
metadata before deserializing to its allowlisted typed op; payload fields cannot
override it. Representative nested request action:

```json
{"kind":"communication","action":{"kind":"message.ack","id":"msg-example"}}
```

Representative accepted result:

```json
{"code":"ok","subject":"msg-example","effect_ids":["eff-request1-000000"]}
```

Representative cursor conflict (with no effects):

```json
{"code":"stale_cursor","subject":"post-example"}
```
