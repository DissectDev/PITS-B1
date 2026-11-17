import { useCallback, useEffect, useState } from 'react';
import { fetchPitsEntityDetail } from '../api/pitsClient';
import type { PitsEntityDetail } from '../types';

interface CacheEntry {
  data?: PitsEntityDetail;
  error?: string;
  loading: boolean;
  refreshing: boolean;
  fetchedAt: number;
  promise?: Promise<PitsEntityDetail>;
}

const STALE_REVALIDATE_MS = 30_000;
const LIVE_UPGRADE_POLL_MS = 3_000;
const LIVE_UPGRADE_MAX_POLLS = 10;

const store = new Map<string, CacheEntry>();
const listeners = new Map<string, Set<() => void>>();

function notify(key: string) {
  const subs = listeners.get(key);
  if (subs) {
    subs.forEach(fn => fn());
  }
}

async function loadEntityDetail(
  namespace: string,
  name: string,
  forceRefresh: boolean,
): Promise<PitsEntityDetail> {
  const key = `${namespace}/${name}`;
  const existing = store.get(key);

  if (!forceRefresh && existing?.promise) {
    return existing.promise;
  }

  const nextState: CacheEntry = {
    data: existing?.data,
    error: undefined,
    loading: !existing?.data,
    refreshing: Boolean(existing?.data) && forceRefresh,
    fetchedAt: existing?.fetchedAt ?? 0,
  };

  const p = fetchPitsEntityDetail(namespace, name, forceRefresh)
    .then(detail => {
      store.set(key, {
        data: detail,
        error: undefined,
        loading: false,
        refreshing: false,
        fetchedAt: Date.now(),
      });
      notify(key);
      return detail;
    })
    .catch(err => {
      const msg = err instanceof Error ? err.message : String(err);
      store.set(key, {
        data: existing?.data,
        error: msg,
        loading: false,
        refreshing: false,
        fetchedAt: existing?.fetchedAt ?? 0,
      });
      notify(key);
      throw err;
    });

  nextState.promise = p;
  store.set(key, nextState);
  notify(key);
  return p;
}

async function revalidateEntityDetailSilent(
  namespace: string,
  name: string,
): Promise<PitsEntityDetail | undefined> {
  const key = `${namespace}/${name}`;
  const existing = store.get(key);

  if (existing?.promise) {
    return existing.promise.catch(() => existing.data);
  }

  const p = fetchPitsEntityDetail(namespace, name, false)
    .then(detail => {
      store.set(key, {
        data: detail,
        error: undefined,
        loading: false,
        refreshing: false,
        fetchedAt: Date.now(),
      });
      notify(key);
      return detail;
    })
    .catch(err => {
      const latest = store.get(key);
      store.set(key, {
        data: latest?.data ?? existing?.data,
        error: latest?.data ? undefined : latest?.error,
        loading: false,
        refreshing: false,
        fetchedAt: latest?.fetchedAt ?? existing?.fetchedAt ?? 0,
      });
      throw err;
    });

  if (existing) {
    store.set(key, {
      ...existing,
      loading: false,
      promise: p,
    });
  }

  return p.catch(() => store.get(key)?.data);
}

export function usePitsEntityDetail(namespace: string, name: string) {
  const key = `${namespace}/${name}`;
  const [, setTick] = useState(0);

  const entry = store.get(key) ?? {
    loading: true,
    refreshing: false,
    fetchedAt: 0,
  };
  const hasData = Boolean(entry.data);
  const isLiveConnected = Boolean(entry.data?.liveConnected);

  useEffect(() => {
    const rerender = () => setTick(t => t + 1);
    let set = listeners.get(key);
    if (!set) {
      set = new Set();
      listeners.set(key, set);
    }
    set.add(rerender);

    const current = store.get(key);
    if (!current || (!current.data && !current.loading && !current.error)) {
      void loadEntityDetail(namespace, name, false).catch(() => {});
    } else if (current.data && Date.now() - current.fetchedAt > STALE_REVALIDATE_MS) {
      void revalidateEntityDetailSilent(namespace, name);
    }

    return () => {
      set?.delete(rerender);
      if (set && set.size === 0) {
        listeners.delete(key);
      }
    };
  }, [key, namespace, name]);

  useEffect(() => {
    const current = store.get(key);
    if (!hasData) {
      return undefined;
    }

    let cancelled = false;
    if (!isLiveConnected && !(current?.data && current.data.liveConnected)) {
      let attempts = 0;
      const iv = window.setInterval(() => {
        if (cancelled) return;
        const cur = store.get(key);
        if (cur?.data?.liveConnected || attempts >= LIVE_UPGRADE_MAX_POLLS) {
          window.clearInterval(iv);
          return;
        }
        attempts += 1;
        void revalidateEntityDetailSilent(namespace, name);
      }, LIVE_UPGRADE_POLL_MS);

      return () => {
        cancelled = true;
        window.clearInterval(iv);
      };
    }

    // Once Wave-1 live telemetry is connected, silently pick up Wave-2 COSI enrichment & periodic refreshes
    const tWave2 = window.setTimeout(() => {
      if (!cancelled) {
        void revalidateEntityDetailSilent(namespace, name);
      }
    }, 14_000);
    const ivSteady = window.setInterval(() => {
      if (!cancelled) {
        void revalidateEntityDetailSilent(namespace, name);
      }
    }, STALE_REVALIDATE_MS);

    return () => {
      cancelled = true;
      window.clearTimeout(tWave2);
      window.clearInterval(ivSteady);
    };
  }, [key, namespace, name, hasData, isLiveConnected]);

  const refreshLive = useCallback(async () => {
    return loadEntityDetail(namespace, name, true);
  }, [namespace, name]);

  return {
    detail: entry.data,
    error: entry.error,
    loading: entry.loading,
    refreshing: entry.refreshing,
    refreshLive,
  };
}
