# Sessions and messaging

Read this reference only when several terminals or agents share a store, or
when the task uses Mote direct messages.

## Sessions and presence

Give concurrent workstreams distinct session identities so their bylines and
leases are unambiguous:

```sh
eval "$(mote session start --as <session-name> --label 'current work')"
mote session status working --message 'current work' --issue <bd-id>
mote session heartbeat --ttl 15m --renew-within 5m
mote session end
```

`session start` prints shell-quoted exports because a CLI cannot modify its
parent environment. With multiple terminals, set distinct `MOTE_ACTOR` values;
with multiple worktrees, point them at the same `MOTE_STORE` when they are meant
to coordinate.

Use `mote in-flight`, `mote actor list`, and `mote actor status <actor>` to
inspect presence. `live` means a valid lease; `recent` means recent substantive
activity and is not an online claim. Read-only dashboards replay state and do
not publish operations.

Heartbeat is append-budgeted unless `--force` is used. Use an idempotency key
when retrying a heartbeat or status update after uncertain delivery.

## Direct messages and requests

```sh
mote inbox
mote msg send --to <actor> --issue <bd-id> --kind request "request"
mote msg ack <msg-id>
mote msg reply <msg-id> --kind response "result"
mote msg requests --state open
mote msg resolve <msg-id>
```

Acknowledgement means receipt, not completion. A structured reply changes a
request to responded or declined; the original sender resolves it. Ordinary
messages queue even when the recipient is not live. Use `--require-live` only
when live presence is a genuine precondition.

For multiline or shell-sensitive text, pass literal stdin using the command's
documented `--stdin` or `-` form. Use stable sender-scoped idempotency keys for
retries after uncertain delivery; reusing a key with different content is an
error.

Stale-request warnings are derived attention signals, not tracker mutations.
Answer or decline the request explicitly; an acknowledgement or unrelated note
does not fulfill it.
