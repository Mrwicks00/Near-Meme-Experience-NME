"use client";
import { useEffect, useRef, useState } from "react";
import { NME } from "./config";
import { Coin, Commentary, FeedItem, Round, WarConfig, loadCoins, loadCommentary, loadFeedLastId, loadFeedSince, fillJobTickers, loadNextRoundMs, loadRound, loadWarConfig } from "./nme";

// Background tabs poll at most every 30 s, so an open tab never hammers the public RPC.
const idle = (ms: number) => (typeof document !== "undefined" && document.hidden ? Math.max(ms, 30000) : ms);

/** Fired after the user's own transaction: every poller re-reads now and again shortly after (block inclusion lag). */
export const REFRESH = "nme:refresh";
export function announceChange() {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new Event(REFRESH));
}
function onRefresh(run: () => void) {
  const timers: ReturnType<typeof setTimeout>[] = [];
  const h = () => { timers.push(setTimeout(run, 1500), setTimeout(run, 6000)); };
  window.addEventListener(REFRESH, h);
  return () => { window.removeEventListener(REFRESH, h); timers.forEach(clearTimeout); };
}

export type Loadable<T> = { data: T | null; error: string | null; loading: boolean };

/** Ticking clock for countdowns. */
export function useNow(everyMs = 1000) {
  // 0 until mounted: the page is prerendered, and the clock must be read in the browser.
  const [now, setNow] = useState(0);
  useEffect(() => {
    const first = setTimeout(() => setNow(Date.now()), 0);
    const id = setInterval(() => setNow(Date.now()), everyMs);
    return () => { clearTimeout(first); clearInterval(id); };
  }, [everyMs]);
  return now;
}

function usePoll<T>(fn: () => Promise<T>, everyMs: number, deps: unknown[] = []): Loadable<T> & { reload: () => void } {
  const [state, setState] = useState<Loadable<T>>({ data: null, error: null, loading: true });
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout>;
    const run = async () => {
      try {
        const data = await fn();
        if (alive) setState({ data, error: null, loading: false });
      } catch (e) {
        if (alive) setState((s) => ({ data: s.data, error: (e as Error).message, loading: false }));
      }
      if (alive && everyMs > 0) timer = setTimeout(run, idle(everyMs));
    };
    run();
    const off = onRefresh(() => { clearTimeout(timer); run(); });
    return () => {
      alive = false;
      clearTimeout(timer);
      off();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nonce, everyMs, ...deps]);
  return { ...state, reload: () => setNonce((n) => n + 1) };
}

export type Arena = { round: Round | null; war: WarConfig; nextRoundMs: number };

export const useCoins = () => usePoll<Coin[]>(loadCoins, 30000);
export const useArena = () =>
  usePoll<Arena>(async () => {
    const [round, war, nextRoundMs] = await Promise.all([loadRound(), loadWarConfig(), loadNextRoundMs()]);
    return { round, war, nextRoundMs };
  }, 10000);
export const useCommentary = () => usePoll<Commentary[]>(() => loadCommentary(20), 20000);

/**
 * Live feed: loads the last `keep` items, then polls `feed_since` every 2.5 s.
 * `fresh` holds ids that arrived after the first load, so the UI can flash them.
 */
export function useFeed(keep = 40) {
  const [items, setItems] = useState<FeedItem[]>([]);
  const [fresh, setFresh] = useState<Set<number>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const lastId = useRef<number | null>(null);
  const jobs = useRef(new Map<string, string>());

  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout>;
    const tick = async () => {
      try {
        if (lastId.current === null) {
          const last = await loadFeedLastId();
          const first = fillJobTickers(await loadFeedSince(Math.max(0, last - keep), keep), jobs.current);
          if (!alive) return;
          setItems(first.slice().reverse());
          lastId.current = first.length ? first[first.length - 1].id : last;
        } else {
          const more = fillJobTickers(await loadFeedSince(lastId.current, 100), jobs.current);
          if (!alive) return;
          if (more.length) {
            lastId.current = more[more.length - 1].id;
            setItems((old) => [...more.slice().reverse(), ...old].slice(0, keep));
            setFresh(new Set(more.map((m) => m.id)));
          }
        }
        setError(null);
      } catch (e) {
        if (alive) setError((e as Error).message);
      }
      if (alive) {
        setLoading(false);
        timer = setTimeout(tick, idle(NME.feedPollMs));
      }
    };
    tick();
    const off = onRefresh(() => { clearTimeout(timer); tick(); });
    return () => {
      alive = false;
      clearTimeout(timer);
      off();
    };
  }, [keep]);

  return { items, fresh, error, loading };
}

/** Poll a loader while the page is open. */
export function usePolled<T>(fn: (() => Promise<T>) | null, everyMs: number, deps: unknown[]) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!fn) return;
    let alive = true;
    let t: ReturnType<typeof setTimeout>;
    const run = async () => {
      try { const d = await fn(); if (alive) { setData(d); setError(null); } }
      catch (e) { if (alive) setError((e as Error).message); }
      if (alive && everyMs > 0) t = setTimeout(run, idle(everyMs));
    };
    run();
    return () => { alive = false; clearTimeout(t); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return { data, error };
}

