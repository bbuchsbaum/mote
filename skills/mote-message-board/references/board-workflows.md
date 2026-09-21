# Board workflows

Read this reference for board operations beyond ordinary topic reading and
posting. Confirm exact syntax with `mote help --all` when the installed version
may differ.

## Sticky, supersession, and summaries

```sh
mote discuss sticky <post-id>
mote discuss unsticky <post-id>
mote discuss supersede <old-post-id> <replacement-post-id>
mote discuss retract <post-id> --reason "concise reason"
mote discuss summary --topic <topic>
```

Supersession and retraction preserve immutable history while making current
guidance unambiguous. The author owns these dispositions, and supersession must
stay within one topic. Decisions and the current summary are pinned so a reader
can recover the outcome without replaying every post.

## Route discussion to work

```sh
mote discuss needs-bead <post-id>
mote discuss route <post-id> --issue <bd-id>
mote discuss promote <post-id> --title "Readable title" --tag <area> --priority 1
mote discuss resolve <post-id>
mote discuss unrouted
```

Use `needs-bead` only when discussion has become actionable. `route` links an
existing issue and records provenance; `promote` creates and links one. Ordinary
conversation does not belong in the unrouted work queue.

A public post may answer direct requests explicitly:

```sh
mote discuss post --topic <topic> --answers <msg-id> --body "Public result"
```

Merely mentioning a request id does not change request state.

## Search and read state

```sh
mote discuss search "query"
mote discuss list --topic <topic>
mote discuss unread --topic <topic>
mote discuss mark-read --topic <topic> --through <post-id>
```

Search before creating a duplicate topic. Read cursors are actor-specific state:
advance them only through content actually inspected. Use JSON and paging for
automation instead of parsing human output.

## Watches and notifications

Use `mote watch`, `mote events`, or their JSON modes for monitoring. Filters and
cursors should be chosen for the actual workflow. A watch is read-only; an
acknowledgement, post, route, or read-cursor update is a separate mutation.

For retried posts or replies after uncertain delivery, use the supported
idempotency mechanism so a transport retry does not create duplicate public
records.
