# Atomic begin announcements

The immutable `coordination-announcements-v1` capability adds one compound
action. It does not change the seven earlier profile meanings. Fresh roots use
exactly canonical-transactions-v1, coordination-announcements-v1,
coordination-communications-v1, coordination-core-v1, coordination-lifecycle-v1,
explicit-release-v1. Migrated roots add authority-retirement-v1 first and
legacy-packed-v1 last. Bootstrap/migration select these profiles; connection
never upgrades an existing root. Migrated eligibility and pre-use retirement
requirements remain in force.

The strict action is `kind:"begin.announce"` with exactly `begin` and
`announcement`. `begin` has the existing strict Begin fields: id, expect_status,
reservation (required nullable), paths, note (required nullable). `announcement`
has exactly id (a new post- identifier) and topic. Unknown nested fields reject.
The post ID is fixed in the durable outer request, before any publication.

The deterministic announcement body is `claiming ID` for pathless work, or
`claiming ID for RESERVATION on PATHS` with paths joined by comma and space.
It uses post_kind=post, null reply_to and idempotency_key, and empty answers and
notify. Actor, timestamp, sequence and effect index come from the outer request.
Validation checks the complete generated Post, including the 1 MiB body limit;
valid long Begin paths do not permit an oversized generated communication.

Planning first uses the existing Begin planner on provisional state, then the
existing communication planner on that same state. The latter starts at the
number of Begin effects. Effect ID suffix, numeric index, adapter ID and any
rejection index use that global position. Checked offset plus effect count must
not exceed 1,000,000. The inner Begin request is only a pure planner input; it
is never digested, journaled or published as another intent. Canonical history
retains and replans the original `begin.announce` request.

For pathless open work without a note, the golden order is claim index 0,
status patch 1, announcement 2. With reservation and note it is reservation 0,
claim 1, status patch 2, note 3, announcement 4. Already-doing work emits no extra
status patch. Success retains the original Begin task, claim-token and
reservation fields, adds `announcement:{id,topic}`, and replaces `effect_ids`
with the complete ordered list from both stages.

Syntax errors refuse before submission. Normal Begin preconditions retain
their established durable rejection precedence. If the Post reducer rejects
after provisional Begin—for example, its ID already exists—the entire compound
request rejects with no effects and the original task, ownership and domain
state. Its communication rejection reports the global effect index. A rejected
canonical receipt still advances transaction sequence, as any other rejection
does. Retirement and active-session checks retain their prior precedence.

Exact retained-request recovery returns the original receipt, reservation and
announcement IDs without another post or ownership generation. An existing
profile without this capability refuses the action even if it independently
supports Begin and communications. A newly connected client cannot add this
capability to that root.
