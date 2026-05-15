import { useCallback, useEffect, useState } from "react";

/// Frontend-only daily-usage tracker for the enhancement action.
/// Persisted in localStorage; resets when the local date rolls over.
/// Cross-window updates flow through the native `storage` event
/// (fires in every window that didn't write the value) plus a
/// custom in-window event so the originating window also rerenders.

const STORAGE_KEY = "pf.enhancements.usage";
const UPDATE_EVENT = "pf-usage-changed";

export const DAILY_LIMIT = 50;

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

  const increment = useCallback(() => {
    const next = Math.min(loadCount() + 1, DAILY_LIMIT);
    saveCount(next);
    window.dispatchEvent(new CustomEvent(UPDATE_EVENT));
    setUsed(next);
  }, []);

  return {
    used,
    limit: DAILY_LIMIT,
    remaining: Math.max(DAILY_LIMIT - used, 0),
    limitReached: used >= DAILY_LIMIT,
    increment,
  };
}
