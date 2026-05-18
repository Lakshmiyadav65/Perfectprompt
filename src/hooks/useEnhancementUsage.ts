import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

/// Daily-usage tracker for the enhancement action.
///
/// Two sources, in order of precedence:
///   1. Server-backed: when the hosted-tier pipeline emits a
///      `hosted:quota` event (Rust → JS, fired after every signed-in
///      /enhance call), we snapshot the server's count and ignore the
///      localStorage path. This is authoritative — consume_quota() in
///      Postgres is the source of truth.
///   2. Local fallback: BYOK path or signed-out users keep the legacy
///      localStorage counter. Resets when the local date rolls over.

const STORAGE_KEY = "pf.enhancements.usage";
const UPDATE_EVENT = "pf-usage-changed";

export const DAILY_LIMIT = 50;

interface HostedQuota {
  used: number;
  limit: number;
  remaining: number;
  plan_tier: string;
  resets_at?: string | null;
}

interface StoredUsage {
  date: string; // YYYY-MM-DD (local)
  count: number;
}

function todayKey(): string {
  const d = new Date();
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

function loadCount(): number {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return 0;
    const parsed: StoredUsage = JSON.parse(raw);
    if (parsed.date !== todayKey()) return 0;
    const n = Number(parsed.count);
    return Number.isFinite(n) && n > 0 ? n : 0;
  } catch {
    return 0;
  }
}

function saveCount(count: number) {
  try {
    const data: StoredUsage = { date: todayKey(), count };
    localStorage.setItem(STORAGE_KEY, JSON.stringify(data));
  } catch {
    /* localStorage unavailable or quota-exceeded — drop silently */
  }
}

export function useEnhancementUsage() {
  const [used, setUsed] = useState<number>(loadCount);
  const [hosted, setHosted] = useState<HostedQuota | null>(null);

  useEffect(() => {
    const refresh = () => setUsed(loadCount());
    refresh();
    window.addEventListener(UPDATE_EVENT, refresh);
    // `storage` fires in other windows when localStorage mutates —
    // keeps the sidebar count in sync if a popup window increments.
    window.addEventListener("storage", refresh);
    return () => {
      window.removeEventListener(UPDATE_EVENT, refresh);
      window.removeEventListener("storage", refresh);
    };
  }, []);

  useEffect(() => {
    // `alive` closes the StrictMode listen-race: in dev, effects run
    // twice on mount, so if the listen() promise resolves AFTER the
    // simulated unmount, the unlisten function leaks and the next
    // mount registers a second active listener for the same event.
    let alive = true;
    let unlisten: (() => void) | undefined;
    void listen<HostedQuota>("hosted:quota", (event) => {
      if (!alive) return;
      setHosted(event.payload);
    }).then((u) => {
      if (!alive) {
        u();
        return;
      }
      unlisten = u;
    });
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  const increment = useCallback(() => {
    const next = Math.min(loadCount() + 1, DAILY_LIMIT);
    saveCount(next);
    window.dispatchEvent(new CustomEvent(UPDATE_EVENT));
    setUsed(next);
  }, []);

  // Server data wins when present — it's authoritative for signed-in
  // users. Falls back to localStorage for BYOK / signed-out.
  const effectiveUsed = hosted ? hosted.used : used;
  const effectiveLimit = hosted ? hosted.limit : DAILY_LIMIT;

  return {
    used: effectiveUsed,
    limit: effectiveLimit,
    remaining: Math.max(effectiveLimit - effectiveUsed, 0),
    limitReached: effectiveUsed >= effectiveLimit,
    source: hosted ? ("hosted" as const) : ("local" as const),
    increment,
  };
}
