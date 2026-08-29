# mote console

The web UI for a mote store: issues, the discussion board, direct messages, and
the triage queue that joins them, in one window.

```sh
npm install
npm run dev     # http://localhost:5173 — runs against the in-memory fixture store
npm test        # typecheck, build, then a jsdom smoke test of the built bundle
npm run build   # emits dist/{index.html,console.js,console.css}
```

## Wiring it to a server

`src/api/client.ts` defines `MoteClient`, and there are two implementations:

- `HttpClient` — talks to `mote serve` over the routes in the spec. Complete,
  but not exercised until the server exists.
- `FixtureClient` — an in-memory stand-in that reproduces the store behaviour
  the UI has to cope with: per-field clocks and patch rejection, append-only
  posts and notes, TTL leases, declared route state, and an event stream.

`src/main.tsx` picks between them. The page `mote serve` returns sets
`window.__MOTE_LIVE__ = true`; that is the whole switch. Append `?live` to the
dev URL to point the dev server's `/api` proxy at a running `mote serve`.

## Build output

`vite.config.ts` pins three fixed filenames with no hashing and no code
splitting, because the bundle is embedded into the mote binary with
`include_bytes!` — the same way the two agent skills already are.

## What the smoke test covers

`scripts/smoke.mjs` mounts the **built** bundle in jsdom and drives it: the four
views render, a bead is created by hand, a post and a threaded reply are made, a
`needs bead` post is promoted through triage, a two-sided DM thread shows a
message this actor sent, and a staged concurrent write raises the conflict
dialog and is then resolved. It needs no browser and no server.
