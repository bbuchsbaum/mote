import { useCallback, useEffect, useRef, useState } from "react";
import type { EventCategory, MoteEvent } from "./api/types";
import type { MoteClient } from "./api/client";

/**
 * Cache slices. An event's category decides which of these go stale — the same
 * mapping the spec's live-updates table describes. Coarse on purpose: refetching
 * a list against a local store is cheaper than tracking per-entity dependencies.
 */
export type Slice =
  | "board" | "beads" | "bead" | "topics" | "posts" | "thread"
  | "unrouted" | "actors" | "dm";

const INVALIDATES: Record<EventCategory, Slice[]> = {
  issue: ["beads", "bead", "board", "unrouted"],
  claim: ["beads", "bead", "board"],
  reservation: ["board"],
  discussion: ["topics", "posts", "thread", "unrouted", "board"],
  message: ["actors", "dm", "board"],
  session: ["actors"],
  candidate: [],
};

class Revisions {
  private revs = new Map<Slice, number>();
  private listeners = new Set<() => void>();

  get(slice: Slice): number {
    return this.revs.get(slice) ?? 0;
  }
  bump(slices: Slice[]) {
    for (const s of slices) this.revs.set(s, this.get(s) + 1);
    for (const fn of this.listeners) fn();
  }
  onEvent(event: MoteEvent) {
    this.bump(INVALIDATES[event.category] ?? []);
  }
  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }
}

export const revisions = new Revisions();

/** Refetches whenever its slice is invalidated or `key` changes. */
export function useResource<T>(
  slice: Slice,
  key: string,
  loader: () => Promise<T>,
): { data: T | null; error: Error | null; loading: boolean; reload: () => void } {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);
  const loaderRef = useRef(loader);
  loaderRef.current = loader;

  useEffect(() => revisions.subscribe(() => setTick((t) => t + 1)), []);

  const rev = revisions.get(slice);
  useEffect(() => {
    let live = true;
    setLoading(true);
    loaderRef.current()
      .then((value) => { if (live) { setData(value); setError(null); } })
      .catch((e: Error) => { if (live) setError(e); })
      .finally(() => { if (live) setLoading(false); });
    return () => { live = false; };
    // `tick` is the invalidation signal; `rev` keeps it honest across slices.
  }, [slice, key, rev, tick]);

  return { data, error, loading, reload: () => setTick((t) => t + 1) };
}

/** Connects the event stream to the cache for the life of the app. */
export function useEventStream(client: MoteClient) {
  const [connected, setConnected] = useState(true);
  useEffect(() => {
    try {
      const stop = client.subscribe((event) => revisions.onEvent(event));
      setConnected(true);
      return stop;
    } catch {
      setConnected(false);
      return undefined;
    }
  }, [client]);
  return connected;
}

/**
 * Wraps a write so every caller gets the same treatment: run it, invalidate,
 * and surface ConflictError to a handler rather than swallowing it.
 */
export function useWrite(onError: (error: Error) => void) {
  const [busy, setBusy] = useState(false);
  const run = useCallback(
    async (fn: () => Promise<unknown>, slices: Slice[]) => {
      setBusy(true);
      try {
        await fn();
        revisions.bump(slices);
        return true;
      } catch (e) {
        onError(e as Error);
        return false;
      } finally {
        setBusy(false);
      }
    },
    [onError],
  );
  return { run, busy };
}

/* ---------------- small shared helpers ---------------- */

export function relativeTime(ts: string | null | undefined): string {
  if (!ts) return "—";
  const delta = Date.now() - new Date(ts).getTime();
  const mins = Math.round(delta / 60_000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.round(hours / 24)}d ago`;
}

export function leaseRemaining(until: string): { label: string; expiring: boolean } {
  const mins = Math.round((new Date(until).getTime() - Date.now()) / 60_000);
  if (mins <= 0) return { label: "expired", expiring: true };
  if (mins < 60) return { label: `${mins}m left`, expiring: mins <= 10 };
  return { label: `${Math.round(mins / 60)}h left`, expiring: false };
}

export const shortId = (id: string) => (id.length > 12 ? `${id.slice(0, 3)}…${id.slice(-6)}` : id);

export const initials = (actor: string) =>
  actor.replace(/[^a-z0-9]/gi, "").slice(0, 2).toLowerCase() || "??";
