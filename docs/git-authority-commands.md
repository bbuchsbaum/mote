# Git-authority command contract

The Git-authority adapter is a development-only remote coordination surface. It
does not qualify a cloud service, a production deployment, or a physical pilot.
An already bound store never falls back to local replay or local publication:
unsupported commands fail with exit 3 before reading batch/import input or
creating an authority transaction.

| Command family | Remote support | Explicit limits or refusal |
| --- | --- | --- |
| `authority capabilities` | Storeless executable evidence: running-path BLAKE3 digest, package version, exact supported profiles. | Build source SHA, dirty state, and target are `unavailable_f10`; this does not establish F10 build provenance. |
| `authority init-git`, `migrate-git`, `rollback-git`, `connect`, `resume`, `refresh`, `cached`, `retry`, `receipt`, `export-legacy`, `recover-session`, `status` | Setup, recovery, and read paths use the selected remote and retained receipts. | Legacy `enable` and `migrate` are refused for a bound Git authority. `legacy-profile` requires the closed legacy build receipt. |
| `session start`, `end`, `list` | Explicit remote sessions; `start --as` resolves through the store actor resolver and therefore trims whitespace. | TTL-based start, and unsupported session mutations, are refused. |
| Task core: `new`, `set`, `assign`, `tag`, `dep`, `rel`, `note`, `claim`, `release`, `reserve`, `unreserve`, `begin`, `done`, `handoff`, `next` | The documented atomic and lifecycle actions are submitted as canonical remote transactions. | `new --tag/--dep/--id`, multi-tag add, TTL claim/reservation/lifecycle modes, and force-release/done/handoff variants are refused where the immutable profile lacks their meaning. |
| Task reads: `show`, `ls`, `ready`, `board`, `history`, `preflight`, `who-has` | Fresh remote projection. | `ready --graph`, `ls --ready/--graph`, candidate reservations, and unsupported graph/projection families are refused. |
| Communications: `send`, `msg`, `discuss`, `inbox` | Available only on the communications-capable immutable profiles. | `send --require-live` is refused because the authority cannot promise live delivery. |
| Observations: `events`, `watch`, `doctor`, read-only `fsck`, constrained `audit`, `actor list`, `actor status` | Fresh remote-derived observations. | `fsck --clean-tmp`, candidate/age audit modes, legacy actor-presence filters, and unsupported observation options are refused. |
| Local identity: `actor set`, `show`, `clear` | Local configuration remains local and does not publish. | It is not a remote actor-activity projection. |
| `candidate`, `role`, `ui`, `serve`, `batch`, `import`, `init`, `admit`, and unlisted task/graph commands | No remote implementation in this development profile. | Refused up front; `batch` and `import` do not consume their input. |

Remote command names and successful local command names are not evidence that a
new capability can be added to an existing authority. A manifest must use one
of the exact immutable supported profiles. Migrated roots additionally require
a verified retained archive for replay; a manifest alone does not prove it.
