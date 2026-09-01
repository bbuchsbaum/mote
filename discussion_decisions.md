# Cited decisions and open questions

Status: implementation contract for GitHub issue #12.

This protocol turns an explicit discussion conclusion into replayable state. It
does not decide whether consensus exists. Every agreement, answer, and closure
is supplied by an actor and bound to exact immutable references; the reducer
never infers agreement from silence or an answer from prose.

## 1. Compatibility boundary

Legacy `board_post` operations with `post_kind=decision` remain valid sticky
prose decisions. They increment the existing decision count but have no
structured citations or questions.

Structured decisions use a new `board_decision` operation. Question lifecycle
changes use a new `board_question` operation. Older binaries reject these
unknown kinds instead of accepting a decision while silently discarding its
evidence or lifecycle policy. A new binary replays a legacy store with empty
structured-decision and question maps and does not rewrite operations.

## 2. Explicit references

A `DiscussionReference` is one tagged value:

- `topic { topic }`
- `post { post_id }`
- `issue { issue_id }`
- `candidate { candidate_id }`
- `url { url }`

References are sorted and unique on the wire. At acceptance time the reducer
requires referenced topics, posts, live issues, and candidates to exist. A
post used as an agreed clause must be active and belong to the decision topic.
Other post references may name another topic because they are supporting
context rather than the agreement itself. HTTP(S) URLs must be absolute,
single-line, whitespace-free, and at most 2,048 bytes. Mote does not fetch
them. Later deletion, supersession, retraction, or terminal transition does not
erase an accepted citation; read surfaces show the cited object's current
disposition.

`--agreed POST` is a stronger reference than a general `--cite post:POST`: it
asserts that this exact post establishes one agreed clause. Its immutable body,
author, and post id remain the cited clause; Mote does not paraphrase it.

## 3. Structured decision operation

`board_decision` contains:

- one new `post_id`, also used as the decision id;
- one existing topic, the decision body, explicit notifications, and an
  optional actor-scoped idempotency key;
- a non-empty sorted unique `agreed_post_ids` list;
- sorted unique supporting references;
- zero or more question drafts, each with a unique `question-ULID` id and
  non-empty single-line question text.

Acceptance atomically creates a sticky public decision post, increments both
the legacy and structured decision counts, records the cited agreement, opens
all questions, updates topic activity, and notifies current topic watchers plus
explicit recipients. Issue references also route the decision post to those
existing issues. At least one agreed post is mandatory; open questions do not
substitute for evidence of agreement.

The CLI command is:

```sh
mote discuss decide --topic TOPIC \
  --agreed POST --agreed POST \
  --open "question text" \
  --cite candidate:CANDIDATE \
  --issue ISSUE \
  --body "decision context" \
  --idempotency-key KEY
```

The body is optional and defaults to the neutral label `Cited decision record`;
Mote never generates a conclusion from cited prose. Repeating a valid
idempotency key with identical caller-controlled content returns the existing
record. With a key, decision and question ids are derived deterministically
from the store, actor, key, and question position so competing retries converge.

The existing `mote discuss decision` command retains its legacy prose behavior.

## 4. Open-question projection

Each question records its decision, topic, immutable text, opener, open op,
current CAS clock, status, answer records, and lifecycle transitions. Status is
one of:

- `open`: unresolved and accepting explicit candidate answers;
- `deferred`: unresolved but intentionally postponed;
- `superseded`: terminal, with an exact successor question in the same topic;
- `closed`: terminal, with an explicit resolution.

`unresolved` means `open` or `deferred`. An answer is an append-only candidate
answer, not an automatic closure: it leaves status `open`, adds exact
references, and advances the question clock. An answer must cite at least one
post in the same topic. This makes “does this question have an answer in the
thread?” a state query without pretending that a matching sentence was
detected automatically.

Lifecycle commands are:

```sh
mote discuss question answer QUESTION --post POST \
  --expect QUESTION_CLOCK --idempotency-key KEY
mote discuss question defer QUESTION --reason TEXT \
  --expect QUESTION_CLOCK --idempotency-key KEY
mote discuss question supersede QUESTION SUCCESSOR \
  --expect QUESTION_CLOCK --idempotency-key KEY
mote discuss question close QUESTION --resolution TEXT \
  --cite post:POST --expect QUESTION_CLOCK --idempotency-key KEY
```

Every mutation carries exact `expect_question` CAS. Concurrent operations from
one clock have at most one winner in filename order. Actor-scoped idempotency
keys make an identical retry a no-op result and reject different reuse.

Any actor may add an answer with a valid same-topic post citation. Only the
decision author may defer, supersede, or close its questions. Answer is allowed
only while open. Defer is allowed only while open. Close or supersede is
allowed while open or deferred. A supersession target must be a different
unresolved question in the same topic; terminal targets and cycles therefore
fail closed. Terminal questions never reopen.

## 5. Read surfaces

`mote discuss decide --topic TOPIC --show` is read-only. It derives, in stable
operation order:

- every structured decision and its agreed posts and references;
- each question's explicit status and current clock;
- all candidate answers and their cited posts;
- open, deferred, and unresolved counts;
- current dispositions for cited posts, issues, and candidates when known.

Topic list, pinned summary, board JSON, terminal UI, HTTP snapshot, and web
console expose the same counts and records. Human status labels are textual,
not color-only. Empty and legacy topics report zero structured decisions and
zero unresolved questions; legacy decision posts remain separately visible.

Events retain the existing public discussion category. Structured decisions
emit `discussion.decided`; question operations emit
`discussion.question_answered`, `discussion.question_deferred`,
`discussion.question_superseded`, or `discussion.question_closed`.

## 6. Replay and validation

Reducers validate the complete operation before mutation. Malformed ids,
duplicate or unsorted references, missing targets, cross-topic agreement or
answer posts, blank text, invalid notification actors, stale CAS, unauthorized
lifecycle changes, invalid transitions, and idempotency conflicts are rejected
atomically and remain visible in replay history.

Derived maps, counts, ordering, routes, notification recipients, answer lists,
and question status depend only on accepted operations and the injected replay
order. Replay never reads the network, Git, filesystem content outside the op
store, or current wall clock.

## 7. Non-goals

- no natural-language consensus, objection, or answer detection;
- no agreement inferred from lack of replies;
- no automatic question closure after an answer;
- no edits or deletion of posts, decisions, questions, or transitions;
- no authority inferred from a citation, route, sticky marker, or topic role;
- no automatic issue or candidate creation;
- no remote URL retrieval or content attestation.
