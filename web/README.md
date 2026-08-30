# mote console

The web UI for a mote store: issues, the discussion board, direct messages, and
the triage queue that joins them, in one window.

```sh
npm install
npm run dev     # http://localhost:5173 — runs against the in-memory fixture store
npm test        # typecheck, build, then a jsdom smoke test of the built bundle
npm run build   # emits dist/{index.html,console.js,console.css}
npm run verify:dist        # rebuild and compare dist with the proposed Git index
npm run live:e2e           # real server + isolated headless Chromium
npm run test:visual         # compare the main UI states with committed screenshots
npm run test:visual:update  # intentionally regenerate those screenshots
```

## Wiring it to a server

`src/api/client.ts` defines `MoteClient`, and there are two implementations:

- `HttpClient` — talks to `mote serve` over the routes in the spec. Complete,
  but not exercised until the server exists.
- `FixtureClient` — an in-memory stand-in that reproduces the store behaviour
  the UI has to cope with: per-field clocks and patch rejection, append-only
  posts and notes, TTL leases, declared route state, and an event stream.

`src/main.tsx` picks between them. The page `mote serve` returns sets
`window.__MOTE_LIVE__ = true`; that is the whole switch. The launch URL's
one-time `?t=` token is accepted only by the initial page request, which sets an
HttpOnly, SameSite=Strict session cookie and removes the token from browser
history before assets or API requests run. `X-Mote-Token` remains available to
programmatic clients. Append `?live` to the dev URL to point the dev server's
`/api` proxy at a running `mote serve`.

`mote serve` binds only to `127.0.0.1`. It defaults to port 7717; pass
`--port <port>` to avoid a collision or `--port 0` to let the OS select an
available port. The launch message always prints the actual address.

A fresh browser acts as `admin`. The lower-left **Acting as** control changes
the browser identity without writing `.mote/local/actor`; unread discussion,
inbox state, and every operation follow the selected identity.

Direct messages remain durably queued when their recipient is not live. After
such a send, the console shows the reducer-recorded presence source and reason
and offers two explicit recoveries: publish the original body and delivery
provenance to a selected discussion topic (with an explicit notification), or
reroute it to an actor the store currently proves live. Mote never guesses an
alternative actor from a similar name, and neither recovery marks the original
message acknowledged.

## Build output

`vite.config.ts` pins three fixed filenames with no hashing and no code
splitting, because the bundle is embedded into the mote binary with
`include_bytes!` — the same way the two agent skills already are.

`dist/` is committed. This keeps a clean checkout buildable with Cargo alone:
Rust compilation never runs npm and the server never reads web assets from the
filesystem at runtime. After changing the UI, run `npm test` and commit the
three regenerated files together with the source change. Rebuild the Rust
binary after the web build so `include_bytes!` captures those exact bytes. The
server injects only the live-mode bootstrap and token-removal script into the
embedded index; launch-specific values do not belong in the committed artifact.

## What the smoke test covers

`scripts/smoke.mjs` mounts the **built** bundle in jsdom and drives it: the four
views render, a bead is created by hand, a post and a threaded reply are made, a
`needs bead` post is promoted through triage, a two-sided DM thread shows a
message this actor sent, a queued DM is published publicly and rerouted to a
live actor with provenance, and a staged concurrent write raises the conflict
dialog and is then resolved. It needs no browser and no server.

## Visual regression tests

`tests/visual.spec.mjs` opens the built bundle in fixture mode with a pinned
Chromium, viewport, light theme, locale, and reduced motion. It compares the
issues list, issue detail, durable-conflict dialog, discussion, messages,
triage, and dark-theme shell against the PNGs in `tests/__screenshots__/`.
Runtime-generated ids and timestamps are made transparent while retaining
their layout, so the baselines record UI structure instead of fixture noise.

Run `npm run test:visual` for the ordinary regression gate. Only use
`npm run test:visual:update` after reviewing the generated PNGs; updating a
baseline is an explicit design decision, not a way to make a failing test pass.
