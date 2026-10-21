# Git authority mode

This opt-in development mode coordinates local machines through one metadata
branch per repository. Source branches share that authority. It needs Git
remote access, not an SSH-accessible workstation. Cloud, physical-machine
adoption and hosted-policy qualification remain separate gates.

Use `mote authority status` for fresh binding/profile/executable diagnostics.
`mote authority capabilities` is storeless. Unknown profiles refuse; never
recreate the authority or fall back to local publication to bypass an error.

For an empty initialized local store, connect to the selected existing branch:

```sh
mote authority connect --remote origin --branch mote-state
eval "$(mote session start --as actor-machine --label 'current work')"
mote preflight --issue bd-ID --paths src/example.rs
mote begin bd-ID --paths src/example.rs --note 'starting'
mote done bd-ID --note 'verified result'
mote session end
```

On a profile declaring `coordination-announcements-v1`, `begin --announce TOPIC`
also commits one public claim announcement atomically with ownership and its
optional note. Earlier profiles refuse it; `connect` never upgrades a root.

Bootstrap is a separate deliberate `authority init-git` action. A nonempty
local history requires stopped-source migration, not `admit` or a merge of
replicas. Preserve old operations and complete backup custody. Setup interrupted
after a durable intent uses `authority resume` against the same endpoint.

Remote claims/reservations use exact session/token generations and explicit
release. TTL, heartbeat, require-live delivery, candidate/role/landing, bulk
batch/import, web console and TUI modes are unsupported and refuse. Actor
status reports enrollment; process liveness is unknown. Do not infer release
from elapsed time, lack of activity or a cached view.

An active requester can deliberately recover an abandoned session:

```sh
mote session list --all
mote authority recover-session sess-SOURCE --expect-session TOKEN --reason 'reviewed abandoned holder'
```

Use its exact observed source-session token. This releases source holdings and
revokes that source session; `--to-session sess-RECIPIENT` explicitly transfers
holdings to an active recipient instead. Concurrent token changes reject.

Fresh reads validate remote history. Offline inspection is explicit:

```sh
mote authority cached
mote authority receipt REQUEST_ID
mote authority retry REQUEST_ID
```

Only `cached` is offline and labels `fresh:false`. A missing receipt is not
proof of failure. Retry requires the submitting client's retained journal and
reuses its exact intent; do not rebuild a timed-out command or substitute a new
communication idempotency key. After total machine-cache loss, explicitly
reconnect to restore routing custody, then look up receipts by retained ID.

Watch/events/inbox followers are read-only polls; they do not acknowledge,
advance discussion cursors or renew ownership. Event cursors identify the
authority root and canonical position, not wall-clock order. Polling intervals
are provisional and full-chain replay costs must be measured for each workload.

New migrated history requires a matching `portable-v2:` closed-build replay
profile: independently built Mac executables may differ, but the complete source
and locked inputs must match. Every client verifies full imported and canonical
replay. Compare `authority legacy-profile` on each machine; a version string is
insufficient. Old exact-v1 archives and interrupted rollback executor journals
retain their exact-binary pin. Before any canonical request, explicit rollback can
retire an unused migrated authority and restore its verified backup into a new
private local store. Post-use conversion is deferred. No command here qualifies
hosted branch policy, authenticates actor names, or publishes source changes.
