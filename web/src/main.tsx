import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { HttpClient, type MoteClient } from "./api/client";
import { FixtureClient } from "./api/fixtures";
import "./styles.css";

/**
 * Wiring point. `mote serve` sets `window.__MOTE_LIVE__` in the page it serves;
 * `npm run dev` without a server falls back to the in-memory fixture store so
 * every view, write, and conflict path is exercisable before the Rust exists.
 */
declare global {
  interface Window { __MOTE_LIVE__?: boolean; __MOTE_FIXTURE__?: FixtureClient }
}

const live = window.__MOTE_LIVE__ === true || new URLSearchParams(location.search).has("live");

const makeClient = (getActor: () => string): MoteClient => {
  if (live) return new HttpClient(getActor);
  // Fixture mode only: lets the smoke test and a human demo stand in for a
  // second agent writing to the store. Never present when talking to a server.
  const fixture = new FixtureClient(getActor);
  window.__MOTE_FIXTURE__ = fixture;
  return fixture;
};

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App client={makeClient} />
  </StrictMode>,
);
