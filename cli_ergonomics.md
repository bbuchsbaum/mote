# CLI ergonomics disposition

This note records the grammar decisions for the high-frequency command guesses
observed during the nine-actor `storymodel4s` release session. The governing
rules are that accepted forms must be unambiguous, existing scripts must keep
their behavior, and rejected forms must point to a command that can be pasted
and corrected.

| Observed or proposed form | Disposition | Reason and shortest safe form |
| --- | --- | --- |
| `mote msg <actor> <body>` | Rejected with a targeted hint | `msg` owns a subcommand namespace (`send`, `reply`, `thread`, `requests`, `resolve`, `ack`). Treating an unknown first token as a recipient would turn misspelled subcommands into messages. Use `mote send <actor> <body>`; the canonical form remains `mote msg send --to <actor> <body>`. |
| `mote msg --to <actor> <body>` | Rejected with the same targeted hint | Flags before a required `msg` subcommand are ambiguous with current and future namespace options. Use `mote send <actor> <body>` or the canonical form. |
| `mote send <actor> <body>` | Accepted | `send` has one purpose, so recipient then body is unambiguous. It publishes the same operation and preserves the canonical command's flags, JSON, stdin, validation, idempotency, and stdout behavior. |
| `mote assign <bead> <actor>` | Accepted | Both positional values have one role. It is exactly a thin alias for `mote set <bead> assignee=<actor>` and uses the same compare-and-set patch path. |
| `mote discuss post <topic>` | Not reinterpreted | This was already the valid form for posting `<topic>` as the body in the default `general` topic. Reinterpreting it would silently break scripts. Topics therefore remain `--topic <topic>`. |
| `mote discuss post <topic> <body>` | Rejected with a targeted hint | Two positional values exceed the existing grammar and reveal the likely mistake. Use `mote discuss post --topic <topic> <body>`. |

Literal stdin bodies, the `design` note kind, human-readable TTLs, and flat
leaf discovery were split into their own work items and are documented in the
README. They do not add aliases to this grammar.
